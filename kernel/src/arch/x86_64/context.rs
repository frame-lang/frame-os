// kernel/src/arch/x86_64/context.rs
//
// The x86_64 implementation of `hal::Context` (B-HAL.4.3). A zero-sized handle
// that delegates to the existing `context_switch` global asm in `context.rs` —
// the same six-callee-saved-GPRs save/restore (rbp, rbx, r12, r13, r14, r15)
// the cooperative scheduler has used since B1 Step 2. Lifting it behind the
// HAL trait is a no-behavior-change refactor: the asm is unchanged and the
// only caller (`sched_demo`) now invokes it through `hal::context()`.
//
// The preemptive switch (the timer ISR full-frame save in `interrupts.rs`) is
// a different beast — interrupt-frame-shaped, ISR-side — and stays purely
// arch-specific. Only the cooperative switch goes behind the trait.

use crate::context::{context_switch, init_stack};
use crate::hal::Context;

/// The x86_64 cooperative-switch surface. A zero-sized handle — the HAL's
/// `Context` device.
pub struct X86Context;

static CTX: X86Context = X86Context;

/// The x86_64 cooperative-switch device.
pub fn context() -> &'static X86Context {
    &CTX
}

impl Context for X86Context {
    unsafe fn switch(&self, old_sp: *mut u64, new_sp: u64) {
        unsafe { context_switch(old_sp, new_sp) };
    }

    unsafe fn init_stack(&self, stack_top: *mut u8, entry: extern "C" fn() -> !) -> u64 {
        unsafe { init_stack(stack_top, entry) }
    }
}
