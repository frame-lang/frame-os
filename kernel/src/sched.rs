// kernel/src/sched.rs
//
// B1 Step 3c + completion: preemptive round-robin over kernel threads,
// with the Frame `Scheduler` made load-bearing for the run/halt decision.
//
// Split of responsibilities (the honest native/Frame line):
//   - NATIVE (this module): the register/stack mechanics. A fixed TCB array,
//     a per-TCB run state, and `schedule()` — called from the timer ISR —
//     which saves the outgoing thread's rsp and picks the next *runnable*
//     worker round-robin (or the boot context if none). The ISR path never
//     touches a Frame system (Frame dispatch is non-reentrant).
//   - FRAME (`Scheduler`, $Idle/$Active): the run/halt *mode*. Spawning a
//     worker fires `task_ready` ($Idle→$Active); a worker exiting fires
//     `task_unready` (→$Idle at zero). The boot context reads `is_idle()`
//     to decide when to stop. Because the Scheduler is shared across
//     preemptible threads and is non-reentrant, every dispatch runs inside
//     `interrupts::without_interrupts` — single-core mutual exclusion.
//
// The demo: two threads busy-loop printing '1'/'2' (no yield) for a few
// rounds, then each EXITS. Preemption interleaves them (B1-4/B1-5); once
// both have exited the Frame Scheduler is $Idle and the kernel halts
// (B1-6).

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::frame_systems::Scheduler;
use crate::interrupts;
use crate::serial;

const MAX_THREADS: usize = 8;
const STACK_SIZE: usize = 16 * 1024;
const ROUNDS_PER_WORKER: u32 = 6;

#[derive(Clone, Copy, PartialEq)]
enum RunState {
    Free,
    Runnable,
    Dead,
}

#[derive(Clone, Copy)]
struct Tcb {
    rsp: u64,
    state: RunState,
}

static mut TCBS: [Tcb; MAX_THREADS] = [Tcb {
    rsp: 0,
    state: RunState::Free,
}; MAX_THREADS];
static mut N: usize = 0; // total TCBs incl. boot (slot 0)
static mut CURRENT: usize = 0;
static ACTIVE: AtomicBool = AtomicBool::new(false);

static mut STACK1: [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut STACK2: [u8; STACK_SIZE] = [0; STACK_SIZE];

// The Frame Scheduler — guarded by interrupts-off critical sections (it is
// non-reentrant and shared across preemptible threads).
static mut SCHED: Option<Scheduler> = None;

fn tcbs() -> *mut Tcb {
    (&raw mut TCBS) as *mut Tcb
}

/// Run `f` with the Frame Scheduler, in a critical section.
fn with_sched<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    interrupts::without_interrupts(|| unsafe {
        let p = &raw mut SCHED;
        let s = (*p).as_mut().expect("scheduler initialized");
        f(s)
    })
}

// ---------------------------------------------------------------------------
// The scheduler callback (invoked from the timer ISR — native only)
// ---------------------------------------------------------------------------

/// Called by `isr_timer` with the interrupted thread's stack pointer.
/// Returns the stack pointer to resume. Pure native: it must not touch the
/// Frame Scheduler (that would re-enter a non-reentrant system from
/// interrupt context).
#[no_mangle]
extern "C" fn schedule(current_rsp: u64) -> u64 {
    interrupts::record_tick();

    if !ACTIVE.load(Ordering::Relaxed) {
        return current_rsp;
    }

    unsafe {
        let t = tcbs();
        let n = (&raw const N).read();
        let cur = (&raw const CURRENT).read();
        (*t.add(cur)).rsp = current_rsp;

        // Round-robin over worker slots 1..n (skip boot slot 0); fall back
        // to boot (the idle context) when no worker is runnable.
        let mut next = 0usize;
        let mut step = 1usize;
        while step <= n {
            let cand = (cur + step) % n;
            step += 1;
            if cand == 0 {
                continue;
            }
            if (*t.add(cand)).state == RunState::Runnable {
                next = cand;
                break;
            }
        }

        (&raw mut CURRENT).write(next);
        (*t.add(next)).rsp
    }
}

// ---------------------------------------------------------------------------
// Thread setup (native)
// ---------------------------------------------------------------------------

fn read_cs() -> u16 {
    let v: u16;
    unsafe {
        asm!("mov {0:x}, cs", out(reg) v, options(nomem, nostack, preserves_flags));
    }
    v
}

fn read_ss() -> u16 {
    let v: u16;
    unsafe {
        asm!("mov {0:x}, ss", out(reg) v, options(nomem, nostack, preserves_flags));
    }
    v
}

/// Craft a fresh thread's stack as a synthetic preemption frame so the
/// first switch `iretq`s `entry` to life with interrupts enabled. Layout at
/// the returned rsp (low → high): [15 zeroed GPRs][RIP][CS][RFLAGS][RSP][SS].
///
/// # Safety
/// `stack_top` must point one past a writable stack of at least 256 bytes.
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

/// Reserve TCB[0] for the boot context (the idle fallback).
fn init_boot() {
    unsafe {
        (&raw mut N).write(1);
        (&raw mut CURRENT).write(0);
        (*tcbs().add(0)).state = RunState::Runnable; // boot is always available
    }
}

/// Add a worker thread; fires the Frame Scheduler's `task_ready`.
unsafe fn spawn(stack_top: *mut u8, entry: extern "C" fn() -> !) {
    let cs = read_cs();
    let ss = read_ss();
    let rsp = init_thread(stack_top, entry, cs, ss);
    let n = (&raw const N).read();
    let t = tcbs();
    (*t.add(n)).rsp = rsp;
    (*t.add(n)).state = RunState::Runnable;
    (&raw mut N).write(n + 1);
    with_sched(|s| s.task_ready());
}

/// Exit the calling worker: mark it dead (so the ISR stops scheduling it),
/// fire the Frame Scheduler's `task_unready`, then park. Never returns — the
/// next tick switches away and this thread is never resumed.
fn exit_current() -> ! {
    interrupts::without_interrupts(|| unsafe {
        let cur = (&raw const CURRENT).read();
        (*tcbs().add(cur)).state = RunState::Dead;
    });
    with_sched(|s| s.task_unready());
    loop {
        interrupts::wait_for_interrupt();
    }
}

// ---------------------------------------------------------------------------
// Demo threads
// ---------------------------------------------------------------------------

fn pace() {
    for _ in 0..50_000u64 {
        core::hint::spin_loop();
    }
}

extern "C" fn worker1() -> ! {
    for _ in 0..ROUNDS_PER_WORKER {
        pace();
        serial::write_str("1");
    }
    exit_current();
}

extern "C" fn worker2() -> ! {
    for _ in 0..ROUNDS_PER_WORKER {
        pace();
        serial::write_str("2");
    }
    exit_current();
}

/// Run the preemptive demo to completion: start two non-yielding threads,
/// let preemption interleave them, and once both have exited (the Frame
/// Scheduler reports `$Idle`) halt the demo.
pub fn run() {
    serial::writeln("[preempt] starting two non-yielding threads");
    unsafe {
        let p = &raw mut SCHED;
        *p = Some(Scheduler::__create());
        init_boot();
        let s1 = (&raw mut STACK1).add(1) as *mut u8;
        let s2 = (&raw mut STACK2).add(1) as *mut u8;
        spawn(s1, worker1);
        spawn(s2, worker2);
    }

    ACTIVE.store(true, Ordering::Relaxed);
    interrupts::enable();

    // Idle when both workers have exited — the Frame Scheduler's $Idle
    // state, read here from normal context, drives the halt.
    while !with_sched(|s| s.is_idle()) {
        interrupts::wait_for_interrupt();
    }

    interrupts::disable();
    ACTIVE.store(false, Ordering::Relaxed);

    serial::writeln("\n[preempt] both threads exited; scheduler is $Idle — done");
}
