// kernel/src/pcsched.rs
//
// Per-core context-switched execution (R1b) — the deeper half of R1. R1a
// (`ksched.rs`) gave each core its own `Scheduler` Frame instance and drove it
// with cross-core posts, but only *tracked* runnable counts; no core actually
// time-sliced anything. R1b wires each AP's LAPIC timer (B7 Step 4) to a real
// per-core context switch: each core owns a small run queue of kernel-thread
// workers, and its periodic LAPIC interrupt round-robins among the runnable ones
// — so every core genuinely interleaves several threads of execution.
//
// This is `sched.rs` (the BSP's single-core preemptive scheduler) replicated
// *per core* and restricted to **kernel threads** (ring 0). Per-core *user
// processes* (ring-3-on-APs, per-CPU TSS.RSP0) are a separate, larger native lift
// deferred to R5; they are not needed for the question R1b exists to answer.
//
// The honest native/Frame split (identical to `sched.rs`, now N-fold):
//   - NATIVE (this module): register/stack mechanics. A per-core TCB array, a
//     per-core `CURRENT`, and `schedule()` — called from *this core's* LAPIC ISR
//     — which saves the outgoing thread's rsp and picks the next runnable worker
//     round-robin (or the per-core boot/idle context if none). The ISR path never
//     touches a Frame system (Frame dispatch allocates and is non-reentrant —
//     dispatching from interrupt context against the spin-locked heap can
//     deadlock; see `frame_assessment.md` finding #3).
//   - FRAME (`Scheduler`, $Idle/$Active): the per-core run/halt *mode*. Admitting a
//     worker is `task_ready` ($Idle→$Active); a worker exiting is `task_unready`
//     (→$Idle at zero). **The Scheduler is driven as an actor (R7a):** workers and
//     the spawn path only *post* a `SchedMsg` into the core's mailbox; the idle
//     loop is the sole drainer that dispatches those messages to the FSM and reads
//     `is_idle()`. So exactly one context touches the instance — no lock, no
//     `without_interrupts` around dispatch (the one remaining critical section, in
//     `exit_current`, guards the Dead-mark + the mailbox push, not the FSM). This
//     is the post/drain (actor) pattern — the native hand-rolling of framec's
//     proposed deferred-send / `@@[cast]` primitive (RFC-0038). The instance is
//     pinned to its core; only the mailbox's Send data is shared.
//
// The Frame-relevant payoff: N cores each run a *real* `Scheduler` instance
// through a live, time-sliced run queue, every one of them allocating per
// dispatch against the single shared heap behind its spinlock — the load case
// R1a could not exercise. The measurement (heap-alloc delta across the whole
// phase, reported by the BSP) confirms the R2a verdict holds under concurrent
// multi-core scheduling: per-event allocation is fine for control-plane
// lifecycles even when N cores hammer the allocator at once.

use core::arch::asm;

use crate::frame_systems::Scheduler;
use crate::interrupts;
use crate::percpu::{this_cpu_index, MAX_CPUS};
use crate::spin::SpinLock;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

/// Workers spawned per core (plus the boot/idle context in slot 0).
const WORKERS_PER_CORE: usize = 3;
/// TCB slots per core: slot 0 is the per-core boot/idle context, 1..=N the workers.
const SLOTS_PER_CORE: usize = WORKERS_PER_CORE + 1;
/// Per-worker kernel stack. Workers do trivial work, so this is generous.
const WORKER_STACK: usize = 8 * 1024;
/// Spin rounds a worker does per work-step — long enough that the LAPIC timer
/// preempts it mid-step under TCG, so siblings interleave (real time-slicing).
const WORK_ROUNDS: u32 = 6;
const SPIN_PER_ROUND: u64 = 40_000;

#[derive(Clone, Copy, PartialEq)]
enum RunState {
    Free,
    Runnable,
    Dead,
}

#[derive(Clone, Copy)]
struct PcTcb {
    rsp: u64,
    state: RunState,
}
const TCB_INIT: PcTcb = PcTcb {
    rsp: 0,
    state: RunState::Free,
};

// Per-core scheduler state. Each core touches only its own row, except the
// result atomics (written by the owning core, read by the BSP afterwards).
static mut PC_TCBS: [[PcTcb; SLOTS_PER_CORE]; MAX_CPUS] = [[TCB_INIT; SLOTS_PER_CORE]; MAX_CPUS];
static mut PC_CURRENT: [usize; MAX_CPUS] = [0; MAX_CPUS];
static PC_ACTIVE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

// Per-core worker stacks (kernel threads, ring 0).
static mut PC_STACKS: [[[u8; WORKER_STACK]; WORKERS_PER_CORE]; MAX_CPUS] =
    [[[0; WORKER_STACK]; WORKERS_PER_CORE]; MAX_CPUS];

// Each core's own `Scheduler` Frame instance (created on its core, never shared).
static mut PC_SCHED: [Option<Scheduler>; MAX_CPUS] = [const { None }; MAX_CPUS];

// --- scheduler-as-actor mailbox (R7a) ---------------------------------------
//
// The `Scheduler` instance is driven through a per-core *mailbox* rather than a
// shared lock: a worker (in a preemptible context) only *posts* a Send message
// (`SchedMsg`) into the mailbox; the idle loop is the **sole** drainer that
// dispatches those messages to the `Scheduler` FSM. So exactly one context ever
// touches the instance — the FSM needs no lock, and the `without_interrupts`
// dance around dispatch is gone. This is the post/drain (actor) pattern the rest
// of the OS uses at hard boundaries, now applied to the in-core scheduler; it's
// the native hand-rolling of framec's proposed deferred-send / `@@[cast]`
// primitive (RFC-0038). Only the mailbox (plain Copy data) is shared.
#[derive(Clone, Copy)]
enum SchedMsg {
    Ready,   // a task became runnable (admit)
    Unready, // a task retired (a worker exited)
}

// One mailbox per core, on the shared `reactor::Mailbox` primitive (IRQ-safe
// lock: a worker posts from a preemptible context, the idle loop drains from the
// same core).
type SchedMailbox = crate::reactor::Mailbox<SchedMsg, 16>;
static PC_MAILBOX: [SpinLock<SchedMailbox>; MAX_CPUS] =
    [const { SpinLock::new(SchedMailbox::new()) }; MAX_CPUS];

// BSP "go" gate: each AP waits here before spawning its workers, so the BSP can
// snapshot the heap-alloc counter *before* any per-core dispatch happens — making
// the alloc delta a clean measurement of the whole phase rather than a tail.
static PC_GO: AtomicBool = AtomicBool::new(false);

// Results, written by the owning core, read by the BSP after the phase.
static PC_SWITCHES: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];
static PC_EXITED: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];
static PC_IDLE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static PC_DONE: AtomicUsize = AtomicUsize::new(0);

// --- raw-pointer accessors (mutable statics, no references) -----------------

fn tcb(cpu: usize, slot: usize) -> *mut PcTcb {
    let base = &raw mut PC_TCBS as *mut PcTcb;
    unsafe { base.add(cpu * SLOTS_PER_CORE + slot) }
}
fn worker_stack_top(cpu: usize, w: usize) -> *mut u8 {
    // &PC_STACKS[cpu][w] one-past-end.
    let base = &raw mut PC_STACKS as *mut u8;
    let off = (cpu * WORKERS_PER_CORE + w) * WORKER_STACK + WORKER_STACK;
    unsafe { base.add(off) }
}
fn pc_current(cpu: usize) -> usize {
    let base = &raw const PC_CURRENT as *const usize;
    unsafe { base.add(cpu).read() }
}
fn set_pc_current(cpu: usize, v: usize) {
    let base = &raw mut PC_CURRENT as *mut usize;
    unsafe { base.add(cpu).write(v) }
}

/// This core's `Scheduler` instance (created lazily on the owning core).
fn sched(cpu: usize) -> &'static mut Scheduler {
    let base = &raw mut PC_SCHED as *mut Option<Scheduler>;
    let slot = unsafe { &mut *base.add(cpu) };
    slot.get_or_insert_with(Scheduler::__create)
}

/// Post a scheduling message into core `cpu`'s mailbox. Called by a worker (or
/// the spawn path) — it never touches the `Scheduler` FSM, only enqueues Send
/// data. The mailbox is sized for the whole phase, so a push never blocks here.
fn post_sched(cpu: usize, m: SchedMsg) {
    let _ = PC_MAILBOX[cpu].lock().push(m);
}

/// Drain core `cpu`'s mailbox into its `Scheduler` FSM. **Only the idle loop
/// calls this** — so the `Scheduler` instance has a single owner and needs no
/// lock. The mailbox lock is held only for each `pop` (released before the
/// dispatch, which allocates), never across an FSM dispatch.
fn drain_mailbox(cpu: usize) {
    loop {
        let next = PC_MAILBOX[cpu].lock().pop();
        match next {
            Some(SchedMsg::Ready) => sched(cpu).task_ready(),
            Some(SchedMsg::Unready) => sched(cpu).task_unready(),
            None => break,
        }
    }
}

// --- thread setup (native) --------------------------------------------------

fn read_cs() -> u16 {
    let v: u16;
    unsafe { asm!("mov {0:x}, cs", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}
fn read_ss() -> u16 {
    let v: u16;
    unsafe { asm!("mov {0:x}, ss", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Craft a fresh kernel thread's stack as a synthetic preemption frame so the
/// first switch `iretq`s `entry` to life with interrupts enabled. Layout at the
/// returned rsp (low → high): [15 zeroed GPRs][RIP][CS][RFLAGS][RSP][SS] — the
/// exact frame the LAPIC-timer ISR restores. (Same shape as `sched::init_thread`.)
///
/// # Safety
/// `stack_top` must point one past a writable stack ≥ 256 bytes.
unsafe fn init_thread(stack_top: *mut u8, entry: extern "C" fn() -> !, cs: u16, ss: u16) -> u64 {
    let top = (stack_top as u64) & !0xF;
    let saved_rsp = top - 160;
    let s = saved_rsp as *mut u64;
    let mut i = 0;
    while i < 15 {
        s.add(i).write(0);
        i += 1;
    }
    s.add(15).write(entry as *const () as usize as u64); // RIP
    s.add(16).write(cs as u64); // CS
    s.add(17).write(0x202); // RFLAGS: IF=1, reserved bit1=1
    s.add(18).write(top - 8); // RSP (≡ 8 mod 16)
    s.add(19).write(ss as u64); // SS
    saved_rsp
}

// --- the per-core scheduler callback (from this core's LAPIC ISR) -----------

/// Whether per-core scheduling is active on `cpu` (the LAPIC ISR checks this
/// before doing any context switch — pure native, no Frame).
pub fn active(cpu: usize) -> bool {
    PC_ACTIVE[cpu].load(Ordering::Relaxed)
}

/// Called from `cpu`'s LAPIC-timer ISR with the interrupted thread's stack
/// pointer; returns the stack pointer to resume. Pure native — it must not touch
/// the Frame Scheduler (that would re-enter a non-reentrant, allocating system
/// from interrupt context). Round-robins this core's runnable workers, falling
/// back to the boot/idle context (slot 0) when none is runnable.
pub fn schedule(cpu: usize, current_rsp: u64) -> u64 {
    let cur = pc_current(cpu);
    unsafe { (*tcb(cpu, cur)).rsp = current_rsp };

    let mut next = 0usize; // boot/idle fallback
    let mut step = 1usize;
    while step <= SLOTS_PER_CORE {
        let cand = (cur + step) % SLOTS_PER_CORE;
        step += 1;
        if cand == 0 {
            continue; // skip boot here; it's the fallback chosen only if no worker runs
        }
        if unsafe { (*tcb(cpu, cand)).state } == RunState::Runnable {
            next = cand;
            break;
        }
    }

    if next != cur {
        PC_SWITCHES[cpu].fetch_add(1, Ordering::Relaxed);
    }
    set_pc_current(cpu, next);
    unsafe { (*tcb(cpu, next)).rsp }
}

// --- worker thread ----------------------------------------------------------

/// A per-core kernel-thread worker: do a few spin-paced work rounds (so the LAPIC
/// timer preempts it and siblings interleave), then exit. Never returns.
extern "C" fn pc_worker() -> ! {
    for _ in 0..WORK_ROUNDS {
        for _ in 0..SPIN_PER_ROUND {
            core::hint::spin_loop();
        }
    }
    exit_current();
}

/// Exit the calling worker (runs on its owning core): mark it Dead so the ISR
/// stops scheduling it, **post** a retire message to this core's mailbox, bump the
/// exit count, then park interrupt-enabled so the next tick switches away. Never
/// returns. The worker does **not** touch the `Scheduler` FSM — it only posts Send
/// data; the idle loop drains it (R7a, scheduler-as-actor).
///
/// The mark-dead and the post must happen in **one** critical section: if a timer
/// tick landed between them, `schedule()` would switch away from a now-dead slot
/// before the Unready was posted, and — the dead slot never being picked again —
/// the Scheduler would never see the retire and never reach `$Idle`. (This is the
/// same atomicity the lock-guarded version needed; here it guards the Dead write
/// + a mailbox push, not an FSM dispatch.)
fn exit_current() -> ! {
    let cpu = this_cpu_index() as usize;
    interrupts::without_interrupts(|| unsafe {
        let cur = pc_current(cpu);
        (*tcb(cpu, cur)).state = RunState::Dead;
        let _ = PC_MAILBOX[cpu].lock().push(SchedMsg::Unready);
    });
    PC_EXITED[cpu].fetch_add(1, Ordering::Relaxed);
    // A dead worker must park with IF=1 so the next LAPIC tick switches away.
    unsafe { asm!("sti", options(nomem, nostack)) };
    loop {
        unsafe { asm!("hlt", options(nomem, nostack)) };
    }
}

// --- driver (runs in this core's ap_entry context) --------------------------

/// Run this core's R1b phase: build a run queue of `WORKERS_PER_CORE` kernel
/// threads, drive its own `Scheduler` Frame instance, time-slice them under the
/// LAPIC timer, and idle until the run queue drains (the Scheduler reports
/// `$Idle`). Records results via atomics and signals done. Returns to the AP's
/// resting idle loop.
///
/// Preconditions: this core's LAPIC timer is already periodic (B7 Step 4) and
/// the IDT is loaded; called on the owning AP only.
pub fn ap_run(cpu: usize) {
    // Wait for the BSP to snapshot the heap-alloc counter, so every dispatch in
    // this phase is measured (clean alloc delta, not a tail). The LAPIC timer
    // keeps ticking while we spin (pcsched inactive ⇒ ISR is a no-op switch).
    while !PC_GO.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }

    // Fresh Scheduler + run queue for this core.
    {
        let base = &raw mut PC_SCHED as *mut Option<Scheduler>;
        unsafe { *base.add(cpu) = Some(Scheduler::__create()) };
    }
    unsafe {
        for slot in 0..SLOTS_PER_CORE {
            (*tcb(cpu, slot)) = TCB_INIT;
        }
        // Slot 0 = this core's boot/idle context: always "runnable" as the
        // fallback, but never spawned — its rsp is captured on the first switch.
        (*tcb(cpu, 0)).state = RunState::Runnable;
    }
    set_pc_current(cpu, 0);

    // Spawn the workers. Spawning *posts* an admit message per worker into the
    // mailbox (preemption is not active yet, so no worker runs during spawn);
    // the idle loop drains them into the Scheduler below.
    let cs = read_cs();
    let ss = read_ss();
    for w in 0..WORKERS_PER_CORE {
        let top = worker_stack_top(cpu, w);
        let rsp = unsafe { init_thread(top, pc_worker, cs, ss) };
        unsafe {
            let t = tcb(cpu, w + 1);
            (*t).rsp = rsp;
            (*t).state = RunState::Runnable;
        }
        post_sched(cpu, SchedMsg::Ready);
    }

    // Activate per-core preemption and idle until the run queue drains. The LAPIC
    // timer (already firing) now round-robins the workers; each exiting worker
    // posts a retire message. This loop is the **sole owner** of the Scheduler
    // FSM: every iteration it drains the mailbox (dispatching the queued admits
    // and retires) and then reads `is_idle()` — no lock, because no other context
    // touches the instance (R7a). The drain runs *before* the check, so the
    // admits posted during spawn move the Scheduler to `$Active` before the first
    // `is_idle()` (which would otherwise be vacuously true at start).
    PC_ACTIVE[cpu].store(true, Ordering::Relaxed);
    unsafe { asm!("sti", options(nomem, nostack)) };
    loop {
        drain_mailbox(cpu);
        if sched(cpu).is_idle() {
            break;
        }
        unsafe { asm!("hlt", options(nomem, nostack)) };
    }
    PC_ACTIVE[cpu].store(false, Ordering::Relaxed);

    PC_IDLE[cpu].store(sched(cpu).is_idle(), Ordering::SeqCst);
    PC_DONE.fetch_add(1, Ordering::SeqCst);
}

// --- BSP read-back ----------------------------------------------------------

/// Release the APs to begin their R1b phase. The BSP calls this *after*
/// snapshotting the heap-alloc counter, so the whole phase is measured.
pub fn release() {
    PC_GO.store(true, Ordering::Release);
}
/// How many cores have finished their R1b phase.
pub fn done_count() -> usize {
    PC_DONE.load(Ordering::SeqCst)
}
/// Context switches `cpu` performed while time-slicing (proof of preemption).
pub fn switches(cpu: usize) -> u32 {
    PC_SWITCHES[cpu].load(Ordering::SeqCst)
}
/// Workers that ran to completion on `cpu`.
pub fn threads_run(cpu: usize) -> u32 {
    PC_EXITED[cpu].load(Ordering::SeqCst)
}
/// Whether `cpu`'s Scheduler ended `$Idle` (its run queue fully drained).
pub fn ended_idle(cpu: usize) -> bool {
    PC_IDLE[cpu].load(Ordering::SeqCst)
}
/// Workers spawned per core (so the BSP can assert all ran).
pub fn workers_per_core() -> u32 {
    WORKERS_PER_CORE as u32
}
