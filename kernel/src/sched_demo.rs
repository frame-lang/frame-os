// kernel/src/sched_demo.rs
//
// B1 Step 2 transitional demo: two cooperative kernel threads ping-pong via
// the HAL's cooperative context switch (`hal::context()`), proving the native
// switch works (control transfers between two independent stacks and back to
// main) before Step 3 wires the timer ISR + Frame `Scheduler` for real
// preemption. This whole module is replaced at Step 3 — it exists only to
// isolate and validate the #1-risk assembly.
//
// As of B-HAL.4.3 this demo goes through `hal::Context::{switch, init_stack}`
// — the same trait the aarch64 boot's mirror demo uses, just with the x86_64
// impl wired in. The asm is unchanged; the call sites moved one indirection
// out, behind the seam.
//
// Flow:  main → A → B → A → B → … (5 rounds) → back to main.
// Output: "[switch] starting A/B ping-pong", then "ABABABABAB", then
// "[switch] back in main, demo done".

use core::sync::atomic::{AtomicU32, Ordering};

use crate::hal::{self, Context as _};
use crate::serial;

const STACK_SIZE: usize = 16 * 1024;

static mut STACK_A: [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut STACK_B: [u8; STACK_SIZE] = [0; STACK_SIZE];

// Saved stack pointers. Single-core, no timer at Step 2, so no concurrent
// access — plain statics accessed via raw pointers (no `static_mut_refs`).
static mut MAIN_RSP: u64 = 0;
static mut A_RSP: u64 = 0;
static mut B_RSP: u64 = 0;

const MAX_ROUNDS: u32 = 5;
static ROUNDS: AtomicU32 = AtomicU32::new(0);

extern "C" fn thread_a() -> ! {
    loop {
        serial::write_str("A");
        // Yield to B. context_switch saves our rsp into A_RSP and resumes
        // B; when B yields back to A, we continue this loop.
        unsafe {
            let b = (&raw const B_RSP).read();
            hal::context().switch(&raw mut A_RSP, b);
        }
    }
}

extern "C" fn thread_b() -> ! {
    loop {
        serial::write_str("B");
        let done = ROUNDS.fetch_add(1, Ordering::SeqCst) + 1 >= MAX_ROUNDS;
        unsafe {
            if done {
                // Last round: hand control back to main and never return.
                let m = (&raw const MAIN_RSP).read();
                hal::context().switch(&raw mut B_RSP, m);
            } else {
                let a = (&raw const A_RSP).read();
                hal::context().switch(&raw mut B_RSP, a);
            }
        }
    }
}

/// Run the ping-pong. Returns to the caller once thread B has handed
/// control back to `main` after `MAX_ROUNDS`.
pub fn run() {
    serial::writeln("[switch] starting A/B ping-pong");
    unsafe {
        // One byte past each array = the (exclusive) stack top; stacks grow
        // downward from there.
        let a_top = (&raw mut STACK_A).add(1) as *mut u8;
        let b_top = (&raw mut STACK_B).add(1) as *mut u8;
        (&raw mut A_RSP).write(hal::context().init_stack(a_top, thread_a));
        (&raw mut B_RSP).write(hal::context().init_stack(b_top, thread_b));

        let a_start = (&raw const A_RSP).read();
        // Switch into A; control returns here when B switches back to main.
        hal::context().switch(&raw mut MAIN_RSP, a_start);
    }
    serial::writeln("\n[switch] back in main, demo done");
}
