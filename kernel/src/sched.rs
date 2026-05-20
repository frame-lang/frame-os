// kernel/src/sched.rs
//
// B1 Step 3c: preemptive round-robin over kernel threads.
//
// This is the native half of preemption — the register/stack mechanics the
// Frame `Scheduler` deliberately does NOT model. The timer ISR
// (interrupts.rs `isr_timer`) saves the full register frame and calls
// `schedule(rsp)`, which:
//   - records the tick + ACKs the PIC (always), then
//   - if preemption is active: stashes the outgoing thread's rsp in its TCB,
//     advances round-robin, and returns the next thread's rsp;
//   - else: returns the same rsp (no switch).
//
// A fresh thread's stack is crafted to look exactly like a thread that was
// preempted: 15 zeroed GPRs below a synthetic `iretq` frame (RIP=entry,
// CS, RFLAGS with IF=1, RSP=its running stack, SS). The first switch into
// it pops the GPRs and `iretq`s it to life with interrupts enabled, so it
// is immediately preemptible.
//
// The demo runs two threads that busy-loop and print '1' / '2' WITHOUT ever
// yielding. The only way both make progress (both digits appear) is the
// timer preempting them — which is the whole point of the milestone.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::interrupts;
use crate::serial;

const MAX_THREADS: usize = 8;
const STACK_SIZE: usize = 16 * 1024;

#[derive(Clone, Copy)]
struct Tcb {
    rsp: u64,
}

static mut TCBS: [Tcb; MAX_THREADS] = [Tcb { rsp: 0 }; MAX_THREADS];
static mut N: usize = 0;
static mut CURRENT: usize = 0;
static ACTIVE: AtomicBool = AtomicBool::new(false);

static mut STACK1: [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut STACK2: [u8; STACK_SIZE] = [0; STACK_SIZE];

static PRINTS: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// The scheduler callback (invoked from the timer ISR)
// ---------------------------------------------------------------------------

/// Called by `isr_timer` with the interrupted thread's stack pointer.
/// Returns the stack pointer to resume (same context if preemption is
/// inactive, else the next thread round-robin).
#[no_mangle]
extern "C" fn schedule(current_rsp: u64) -> u64 {
    interrupts::record_tick();

    if !ACTIVE.load(Ordering::Relaxed) {
        return current_rsp;
    }

    unsafe {
        let tcbs = (&raw mut TCBS) as *mut Tcb;
        let n = (&raw const N).read();
        let cur = (&raw const CURRENT).read();
        (*tcbs.add(cur)).rsp = current_rsp;
        let next = (cur + 1) % n;
        (&raw mut CURRENT).write(next);
        (*tcbs.add(next)).rsp
    }
}

// ---------------------------------------------------------------------------
// Thread setup
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

/// Craft a fresh thread's stack so the first `context_switch`/`iretq` into
/// it starts `entry` with interrupts enabled. Returns the saved rsp (where
/// the timer ISR's `pop`s begin). Layout at the returned rsp (low → high):
///   [r15..rax = 15 zeroed GPRs][RIP=entry][CS][RFLAGS=0x202][RSP][SS]
///
/// # Safety
/// `stack_top` must point one past a writable stack of at least 256 bytes.
unsafe fn init_thread(stack_top: *mut u8, entry: extern "C" fn() -> !, cs: u16, ss: u16) -> u64 {
    let top = (stack_top as u64) & !0xF; // 16-aligned
    let saved_rsp = top - 160; // 20 u64 slots; 160 is 16-aligned
    let s = saved_rsp as *mut u64;
    // 15 GPRs, zeroed (slots 0..15).
    let mut i = 0;
    while i < 15 {
        s.add(i).write(0);
        i += 1;
    }
    // iretq frame (slots 15..20).
    s.add(15).write(entry as *const () as usize as u64); // RIP
    s.add(16).write(cs as u64); // CS
    s.add(17).write(0x202); // RFLAGS: IF=1, reserved bit1=1
    s.add(18).write(top - 8); // RSP the thread runs on (≡ 8 mod 16)
    s.add(19).write(ss as u64); // SS
    saved_rsp
}

/// Reserve TCB[0] for the currently-running boot context. Its rsp is filled
/// in by the first timer tick (when it's switched away from).
fn init_boot() {
    unsafe {
        (&raw mut N).write(1);
        (&raw mut CURRENT).write(0);
    }
}

/// Add a thread with the given stack top and entry point.
unsafe fn spawn(stack_top: *mut u8, entry: extern "C" fn() -> !) {
    let cs = read_cs();
    let ss = read_ss();
    let rsp = init_thread(stack_top, entry, cs, ss);
    let n = (&raw const N).read();
    let tcbs = (&raw mut TCBS) as *mut Tcb;
    (*tcbs.add(n)).rsp = rsp;
    (&raw mut N).write(n + 1);
}

// ---------------------------------------------------------------------------
// Demo threads
// ---------------------------------------------------------------------------

/// Busy-spin to pace serial output. Never yields — preemption is the only
/// thing that can interrupt this. Sized small enough that, within the
/// demo's tick window, each thread prints several times (clear repeated
/// interleaving), but large enough not to flood the serial capture.
fn pace() {
    for _ in 0..50_000u64 {
        core::hint::spin_loop();
    }
}

extern "C" fn worker1() -> ! {
    loop {
        pace();
        serial::write_str("1");
        PRINTS.fetch_add(1, Ordering::Relaxed);
    }
}

extern "C" fn worker2() -> ! {
    loop {
        pace();
        serial::write_str("2");
        PRINTS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Run the preemptive demo: start two non-yielding threads, enable the
/// timer, let them run preempted for a fixed number of ticks, then stop.
/// Both '1' and '2' appearing proves the timer preempted them — neither
/// thread ever yields voluntarily.
pub fn run() {
    serial::writeln("[preempt] starting two non-yielding threads");
    unsafe {
        init_boot();
        let s1 = (&raw mut STACK1).add(1) as *mut u8;
        let s2 = (&raw mut STACK2).add(1) as *mut u8;
        spawn(s1, worker1);
        spawn(s2, worker2);
    }

    ACTIVE.store(true, Ordering::Relaxed);
    interrupts::enable();

    // Run for ~50 timer ticks (~0.5s at 100 Hz). During this window the
    // round-robin gives both workers many slices, so both print.
    let target = interrupts::ticks() + 50;
    while interrupts::ticks() < target {
        interrupts::wait_for_interrupt();
    }

    interrupts::disable();
    ACTIVE.store(false, Ordering::Relaxed);

    serial::writeln("\n[preempt] done — both threads ran under preemption");
}
