// kernel/src/arch/aarch64/sched_preempt.rs
//
// Timer-driven preemptive scheduling on aarch64 (B-HAL.4.5) — the aarch64
// analogue of the bottom half of x86's `sched.rs`. The Frame `Scheduler`
// (`scheduler.frs`) owns the run/halt *mode* ($Idle vs $Active); this module
// owns the *register/stack mechanics* — exactly the same native/Frame line the
// x86 path draws (architecture.md: "the FSM models the invariant; the native
// does the mechanism").
//
// Flow:
//   1. `run()` initializes the Frame Scheduler + the TCB table, spawns two
//      non-yielding workers (`task_ready` ×2 → $Active), enables IRQs, idles.
//   2. The generic timer fires every 100 ms; vectors.rs's irq_stub saves the
//      *full interrupt frame* (x0..x30 + ELR_EL1 + SPSR_EL1, 272 B) of the
//      interrupted thread and calls into the Rust handler with the saved-frame
//      SP. The handler EOIs the GIC, then — because `ACTIVE` is set — calls
//      `schedule(sp)` here.
//   3. `schedule` stashes the outgoing thread's SP into its TCB and walks the
//      ready table round-robin for the next Runnable (falling back to the boot
//      context if none). It returns the new thread's saved-frame SP; the stub
//      `mov sp, x0`'s + restores + `eret`s into it. The first preemption into
//      a freshly-`init_thread`'d worker pops 31 zeroed GPRs + a synthetic ELR
//      = entry + SPSR = EL1h-IRQs-on, so `eret` lands at the worker's entry.
//   4. Each worker prints '1' or '2' a few rounds (busy spin between prints,
//      so the timer can preempt mid-spin), then calls `exit_current` —
//      `task_unready` on the Frame Scheduler, mark this TCB Dead, park; the
//      next tick switches away and a Dead thread is never picked again.
//   5. The boot context, idling in `run()`, sees `Scheduler::is_idle()` once
//      both workers have unreadied; disables IRQs, returns. Done.
//
// Single-core only; no critical sections needed inside the ISR path because
// the ISR is the only place that touches `CURRENT` / TCB rsp/state writes, and
// `with_sched` runs only outside it.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::frame_systems::Scheduler;
use crate::serial;

const MAX_THREADS: usize = 4;
const STACK_SIZE: usize = 16 * 1024;
const ROUNDS_PER_WORKER: u32 = 4;

#[derive(Clone, Copy, PartialEq)]
enum RunState {
    Free,
    Runnable,
    Dead,
}

#[derive(Clone, Copy)]
struct Tcb {
    sp: u64,
    state: RunState,
}

const TCB_INIT: Tcb = Tcb {
    sp: 0,
    state: RunState::Free,
};

static mut TCBS: [Tcb; MAX_THREADS] = [TCB_INIT; MAX_THREADS];
static mut N: usize = 0; // total TCBs including boot slot 0
static mut CURRENT: usize = 0;
static ACTIVE: AtomicBool = AtomicBool::new(false);

// The Frame Scheduler — guarded only by IRQs-off via DAIF.I when read from
// the boot context (the ISR never touches it). Same arrangement as x86.
static mut SCHED: Option<Scheduler> = None;

static mut STACK1: [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut STACK2: [u8; STACK_SIZE] = [0; STACK_SIZE];

/// Whether preemptive scheduling is currently active. The IRQ handler calls
/// `schedule` only when this is set.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// The full interrupt frame layout the irq_stub saves on entry and restores
/// on `eret`. `init_thread` lays this out at the top of a fresh stack so the
/// first preemption-into-it restores zeroed GPRs + the right ELR/SPSR and the
/// `eret` lands at the worker's entry. 33 u64s + 1 padding u64 = 272 B, 16-aligned.
#[repr(C)]
struct TrapFrame {
    gprs: [u64; 31], // x0..x30
    elr: u64,        // ELR_EL1 — `eret`'s PC
    spsr: u64,       // SPSR_EL1 — `eret`'s SPSR
    _pad: u64,       // 16-byte SP alignment
}

const _: () = assert!(core::mem::size_of::<TrapFrame>() == 272);

/// Craft a fresh thread's stack as a synthetic interrupt frame, so the first
/// preemption into this thread pops 31 zeroed GPRs, restores `ELR_EL1 = entry`
/// and `SPSR_EL1 = EL1h-IRQs-on`, and `eret`s into `entry` at EL1 with the
/// generic-timer IRQ unmasked. Returns the SP to record in this thread's TCB.
///
/// SPSR_EL1 bits we set:
///   M[3:0] = 0b0101  (EL1h: EL1 using its own SP_EL1)
///   F (bit 6)  = 0   (FIQ unmasked)
///   I (bit 7)  = 0   (IRQ unmasked)
///   A (bit 8)  = 0   (SError unmasked)
///   D (bit 9)  = 0   (Debug unmasked)
/// → SPSR_EL1 = 0x5.
///
/// # Safety
/// `stack_top` must point one past a writable stack region of at least
/// `sizeof(TrapFrame)` = 272 B (in practice a whole per-thread stack).
unsafe fn init_thread(stack_top: *mut u8, entry: extern "C" fn() -> !) -> u64 {
    let top = (stack_top as u64) & !0xF;
    let saved_sp = top - core::mem::size_of::<TrapFrame>() as u64;
    let frame = saved_sp as *mut TrapFrame;
    unsafe {
        (*frame).gprs = [0; 31];
        (*frame).elr = entry as usize as u64;
        (*frame).spsr = 0x5; // EL1h, all masks clear → IRQs on
        (*frame)._pad = 0;
    }
    saved_sp
}

/// Initialize the run table for a fresh `run()`: boot slot is always Runnable
/// (the idle fallback when no worker is ready).
fn init_boot() {
    unsafe {
        let t = (&raw mut TCBS) as *mut Tcb;
        for i in 0..MAX_THREADS {
            (*t.add(i)) = TCB_INIT;
        }
        (&raw mut N).write(1);
        (&raw mut CURRENT).write(0);
        (*t.add(0)).state = RunState::Runnable;
    }
}

/// Add a worker thread; fires the Frame Scheduler's `task_ready` ($Idle→$Active
/// or stays $Active). Single-core single-threaded init context — the ISR is
/// masked here so no critical section needed.
unsafe fn spawn(stack_top: *mut u8, entry: extern "C" fn() -> !) {
    let sp = unsafe { init_thread(stack_top, entry) };
    let n = (&raw const N).read();
    assert!(n < MAX_THREADS, "sched_preempt: out of TCB slots");
    let t = (&raw mut TCBS) as *mut Tcb;
    unsafe {
        (*t.add(n)).sp = sp;
        (*t.add(n)).state = RunState::Runnable;
        (&raw mut N).write(n + 1);
        let p = &raw mut SCHED;
        if let Some(s) = (*p).as_mut() {
            s.task_ready();
        }
    }
}

/// Called by `rust_irq_handler` from inside the timer ISR with the *interrupted
/// thread's* saved-frame SP. Records that SP into the outgoing thread's TCB,
/// picks the next Runnable thread round-robin (or the boot context if none),
/// returns its saved-frame SP for the stub to restore + `eret`.
///
/// Pure native — must not touch the Frame Scheduler (non-reentrant: a worker
/// outside the ISR could be holding it). Just register/state mechanics.
///
/// # Safety
/// Called only from the IRQ stub with a valid full-frame SP in `current_sp`.
pub unsafe fn schedule(current_sp: u64) -> u64 {
    let t = (&raw mut TCBS) as *mut Tcb;
    let n = (&raw const N).read();
    let cur = (&raw const CURRENT).read();
    unsafe {
        (*t.add(cur)).sp = current_sp;
    }

    // Round-robin over worker slots 1..n (skip boot slot 0); fall back to
    // boot (idle context) when no worker is runnable.
    let mut next = 0usize;
    let mut step = 1usize;
    while step <= n {
        let cand = (cur + step) % n;
        step += 1;
        if cand == 0 {
            continue;
        }
        unsafe {
            if (*t.add(cand)).state == RunState::Runnable {
                next = cand;
                break;
            }
        }
    }

    unsafe { (&raw mut CURRENT).write(next) };
    unsafe { (*t.add(next)).sp }
}

/// Exit the calling worker: fire `task_unready` on the Frame Scheduler, mark
/// this TCB Dead, then park. The next timer tick switches away; a Dead thread
/// is never picked again, so this never returns.
pub fn exit_current() -> ! {
    // Mask IRQs while we touch the shared Scheduler + flip state. Then park
    // IRQs-on so the next tick switches away.
    unsafe {
        core::arch::asm!("msr daifset, #2", options(nomem, nostack));
        let cur = (&raw const CURRENT).read();
        let t = (&raw mut TCBS) as *mut Tcb;
        (*t.add(cur)).state = RunState::Dead;
        let p = &raw mut SCHED;
        if let Some(s) = (*p).as_mut() {
            s.task_unready();
        }
        core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
    }
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

/// Read the Frame Scheduler's `is_idle` mode with IRQs masked (the boot
/// context's loop predicate). The Scheduler is non-reentrant, so the read is
/// guarded — even though the ISR doesn't touch it, this style keeps the
/// invariant local and obvious.
fn sched_is_idle() -> bool {
    unsafe {
        core::arch::asm!("msr daifset, #2", options(nomem, nostack));
        let p = &raw mut SCHED;
        let r = (*p).as_mut().map(|s| s.is_idle()).unwrap_or(true);
        core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
        r
    }
}

// ---------------------------------------------------------------------------
// Demo workers + entry point.
// ---------------------------------------------------------------------------

fn pace() {
    // Under TCG the 10 Hz timer still preempts mid-spin (TCG runs the guest
    // far slower than wall-clock); keep the spin small so total boot time
    // stays well inside the smoke timeout.
    for _ in 0..200_000u64 {
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

/// Run the preemptive demo end-to-end: create the Frame Scheduler, spawn two
/// non-yielding workers, enable IRQs, idle the boot context until the Frame
/// Scheduler reports `$Idle` (both workers exited), then disable IRQs and
/// return. Caller must have brought up the GIC + generic timer + vectors
/// first (B-HAL.3.5).
pub fn run() {
    serial::writeln("[preempt] starting two non-yielding threads");
    unsafe {
        let p = &raw mut SCHED;
        *p = Some(Scheduler::__create());
    }
    init_boot();
    unsafe {
        let s1 = (&raw mut STACK1).add(1) as *mut u8;
        let s2 = (&raw mut STACK2).add(1) as *mut u8;
        spawn(s1, worker1);
        spawn(s2, worker2);
    }

    ACTIVE.store(true, Ordering::Relaxed);
    // Unmask IRQs (DAIF.I=0). The next generic-timer tick will land in the
    // boot context and immediately switch to a worker.
    unsafe { core::arch::asm!("msr daifclr, #2", options(nomem, nostack)) };

    // Idle until the Frame Scheduler reports $Idle (both workers exited).
    while !sched_is_idle() {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }

    unsafe { core::arch::asm!("msr daifset, #2", options(nomem, nostack)) };
    ACTIVE.store(false, Ordering::Relaxed);

    serial::writeln("\n[preempt] both threads exited; Frame Scheduler $Idle — done");
}
