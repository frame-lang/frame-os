// kernel/src/ksched.rs
//
// Per-CPU Frame schedulers (R1a) — the cross-core-`post` finding (B7) under
// *many* real Frame instances. Each core owns its own `Scheduler` Frame system
// (the B1 `$Idle ⇄ $Active` machine); the BSP `post`s `task_ready`/`task_unready`
// events into each core's MPSC queue, and that core drains the queue and
// dispatches the events to *its* Scheduler. As at B7's `EventCounter` demo, the
// instance is **pinned to its owner core** (the AP), only `Send` event data
// (`SchedPost`) crosses cores, and the per-core results are reported through
// plain atomics — so framec's non-`Send` codegen is fine even with N schedulers.
//
// This is the scheduling *coordination / run-mode* layer per core: each core's
// Scheduler tracks "how many runnable tasks do I have, am I `$Active` or
// `$Idle`", driven by cross-core admit/retire posts. It does **not** yet do
// per-core context-switched multi-thread *execution* (each core actually
// time-slicing several threads from its own ready queue) — that's the deeper R1b
// refinement, which would wire each core's LAPIC timer (B7 Step 4) to a per-core
// context switch picking from this ready set.

use crate::frame_systems::Scheduler;
use crate::percpu::MAX_CPUS;
use crate::spin::SpinLock;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

/// A scheduling event posted across cores. Plain `Copy` data — all that crosses
/// the core boundary; the per-core `Scheduler` instance never does.
#[derive(Clone, Copy)]
pub enum SchedPost {
    Ready,
    Unready,
}

const QUEUE_CAP: usize = 64;

/// A per-core MPSC ring of scheduling posts (BSP produces, the owner core drains).
struct SchedQueue {
    buf: [SchedPost; QUEUE_CAP],
    head: usize,
    tail: usize,
    len: usize,
}
impl SchedQueue {
    const fn new() -> Self {
        Self {
            buf: [SchedPost::Ready; QUEUE_CAP],
            head: 0,
            tail: 0,
            len: 0,
        }
    }
    fn push(&mut self, e: SchedPost) -> bool {
        if self.len == QUEUE_CAP {
            return false;
        }
        self.buf[self.tail] = e;
        self.tail = (self.tail + 1) % QUEUE_CAP;
        self.len += 1;
        true
    }
    fn pop(&mut self) -> Option<SchedPost> {
        if self.len == 0 {
            return None;
        }
        let e = self.buf[self.head];
        self.head = (self.head + 1) % QUEUE_CAP;
        self.len -= 1;
        Some(e)
    }
}

// Per-core state. The Scheduler instances are pinned to their owner cores; the
// queues + result atomics are the only cross-core-shared data.
static mut PERCPU_SCHED: [Option<Scheduler>; MAX_CPUS] = [const { None }; MAX_CPUS];
static PERCPU_QUEUE: [SpinLock<SchedQueue>; MAX_CPUS] =
    [const { SpinLock::new(SchedQueue::new()) }; MAX_CPUS];
static PERCPU_EXPECTED: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];
static PERCPU_PEAK: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];
static PERCPU_IDLE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static SCHED_DONE: AtomicUsize = AtomicUsize::new(0);

/// This core's `Scheduler` instance (created on first use, on the owning core).
fn percpu_sched(cpu: usize) -> &'static mut Scheduler {
    let base = &raw mut PERCPU_SCHED as *mut Option<Scheduler>;
    let slot = unsafe { &mut *base.add(cpu) };
    slot.get_or_insert_with(Scheduler::__create)
}

// --- BSP (producer) side ---------------------------------------------------

/// Post a `task_ready` to core `cpu` (cross-core admit).
pub fn post_ready(cpu: usize) {
    while !PERCPU_QUEUE[cpu].lock().push(SchedPost::Ready) {
        core::hint::spin_loop();
    }
}
/// Post a `task_unready` to core `cpu` (cross-core retire).
pub fn post_unready(cpu: usize) {
    while !PERCPU_QUEUE[cpu].lock().push(SchedPost::Unready) {
        core::hint::spin_loop();
    }
}
/// Tell core `cpu` how many posts to expect (set *after* staging them, so a
/// nonzero value means the events are already queued).
pub fn set_expected(cpu: usize, n: usize) {
    PERCPU_EXPECTED[cpu].store(n, Ordering::Release);
}
/// How many cores have finished draining.
pub fn done_count() -> usize {
    SCHED_DONE.load(Ordering::SeqCst)
}
/// Core `cpu`'s peak runnable count (recorded by that core).
pub fn peak(cpu: usize) -> u32 {
    PERCPU_PEAK[cpu].load(Ordering::SeqCst)
}
/// Whether core `cpu`'s Scheduler ended `$Idle` (recorded by that core).
pub fn ended_idle(cpu: usize) -> bool {
    PERCPU_IDLE[cpu].load(Ordering::SeqCst)
}

// --- AP (owner / consumer) side --------------------------------------------

/// Run this core's scheduler-coordination phase: wait for the BSP to stage our
/// posts, then drain them into *our* `Scheduler` instance, tracking the peak
/// runnable count and the final run-mode. Records results via atomics (the
/// instance stays on this core) and signals done.
pub fn ap_run(cpu: usize) {
    // Wait until the BSP has staged our events (expected becomes nonzero).
    let expected = loop {
        let e = PERCPU_EXPECTED[cpu].load(Ordering::Acquire);
        if e > 0 {
            break e;
        }
        core::hint::spin_loop();
    };

    let mut processed = 0usize;
    let mut peak = 0u32;
    while processed < expected {
        let next = PERCPU_QUEUE[cpu].lock().pop();
        match next {
            Some(SchedPost::Ready) => {
                let s = percpu_sched(cpu);
                s.task_ready();
                let r = s.runnable_count();
                if r > peak {
                    peak = r;
                }
                processed += 1;
            }
            Some(SchedPost::Unready) => {
                percpu_sched(cpu).task_unready();
                processed += 1;
            }
            None => core::hint::spin_loop(),
        }
    }

    PERCPU_PEAK[cpu].store(peak, Ordering::SeqCst);
    PERCPU_IDLE[cpu].store(percpu_sched(cpu).is_idle(), Ordering::SeqCst);
    SCHED_DONE.fetch_add(1, Ordering::SeqCst);
}
