// kernel/src/arch/aarch64/context.rs
//
// The aarch64 implementation of `hal::Context` (B-HAL.4.3). x86's cooperative
// switch saves 6 callee-saved GPRs (rbp/rbx/r12–r15); the ARM AAPCS counterpart
// is 12: x19–x28 (10 callee-saved GPRs) + x29 (FP) + x30 (LR). 12 × 8 B =
// 96 B of saved state on the thread's own stack. SP must stay 16-byte aligned
// at all times (the ARM ABI requires it) — 96 is a multiple of 16, so we land
// aligned naturally.
//
// `switch` (the global_asm below):
//   1. stp's the 12 callee-saved regs onto the *current* thread's stack
//      (decrement-before, 16 B/pair, total 96 B);
//   2. stores SP into `*old_sp` (x0);
//   3. loads SP from `new_sp` (x1);
//   4. ldp's the 12 regs back from the new thread's stack
//      (increment-after, mirror order — first ldp pops x29/x30);
//   5. `ret` — into wherever the new thread last switched out from, or, for a
//      freshly `init_stack`-ed thread, into its `entry` function (because the
//      first ldp restored x30 = entry).
//
// AAPCS argument registers: x0 = old_sp, x1 = new_sp. Caller-saved regs (x0–
// x18) are spilled by the caller per AAPCS, so we don't touch them.

use crate::hal::Context;
use core::arch::global_asm;

global_asm!(
    ".global aarch64_context_switch",
    "aarch64_context_switch:",
    // Save x19..x30 onto the current stack. stp uses pre-decrement (`!`) so SP
    // ends 96 B lower with the regs at [sp, #0..#80]. Order pushed *first*
    // (x19/x20) ends up at the highest address; x29/x30 (pushed last) end up
    // at the lowest — so the matching ldp series gets x29/x30 first.
    "  stp x19, x20, [sp, #-16]!",
    "  stp x21, x22, [sp, #-16]!",
    "  stp x23, x24, [sp, #-16]!",
    "  stp x25, x26, [sp, #-16]!",
    "  stp x27, x28, [sp, #-16]!",
    "  stp x29, x30, [sp, #-16]!",
    // *old_sp = current SP. mov-from-sp needs an intermediate (str cannot use sp).
    "  mov x9, sp",
    "  str x9, [x0]",
    // SP ← new_sp (the saved SP of the thread we're switching to).
    "  mov sp, x1",
    // Restore x19..x30 in the mirror order (post-increment `]` form, no `!` so
    // each ldp reads then bumps SP up by 16). First ldp pops x29/x30 — and
    // x30 carries the resume LR (the freshly-init'd thread's `entry`, or the
    // resume address of an already-switched thread).
    "  ldp x29, x30, [sp], #16",
    "  ldp x27, x28, [sp], #16",
    "  ldp x25, x26, [sp], #16",
    "  ldp x23, x24, [sp], #16",
    "  ldp x21, x22, [sp], #16",
    "  ldp x19, x20, [sp], #16",
    "  ret",
);

extern "C" {
    fn aarch64_context_switch(old_sp: *mut u64, new_sp: u64);
}

/// The aarch64 cooperative-switch surface. A zero-sized handle — the HAL's
/// `Context` device.
pub struct AArch64Context;

static CTX: AArch64Context = AArch64Context;

/// The aarch64 cooperative-switch device.
pub fn context() -> &'static AArch64Context {
    &CTX
}

impl Context for AArch64Context {
    unsafe fn switch(&self, old_sp: *mut u64, new_sp: u64) {
        unsafe { aarch64_context_switch(old_sp, new_sp) };
    }

    /// Craft a fresh thread's initial stack so the first `switch` into it loads
    /// 12 zeros into x19..x29 + `entry` into x30, then `ret`s — landing at
    /// `entry`. Layout at the returned SP (low → high addresses), 16-byte
    /// aligned, 96 B total:
    ///
    /// ```text
    ///   [x29=0][x30=entry] [x27=0][x28=0] [x25=0][x26=0]
    ///   [x23=0][x24=0]     [x21=0][x22=0] [x19=0][x20=0]
    /// ```
    unsafe fn init_stack(&self, stack_top: *mut u8, entry: extern "C" fn() -> !) -> u64 {
        let aligned_top = (stack_top as u64) & !0xF;
        let base = aligned_top - 96; // 16-aligned: 12 used 8-byte slots
        let slots = base as *mut u64;
        unsafe {
            slots.add(0).write(0); // x29 (FP)
            slots.add(1).write(entry as usize as u64); // x30 (LR) → entry
            slots.add(2).write(0); // x27
            slots.add(3).write(0); // x28
            slots.add(4).write(0); // x25
            slots.add(5).write(0); // x26
            slots.add(6).write(0); // x23
            slots.add(7).write(0); // x24
            slots.add(8).write(0); // x21
            slots.add(9).write(0); // x22
            slots.add(10).write(0); // x19
            slots.add(11).write(0); // x20
        }
        base
    }
}
