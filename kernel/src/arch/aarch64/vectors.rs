// kernel/src/arch/aarch64/vectors.rs
//
// EL1 exception vector table + IRQ stub + Rust IRQ handler (B-HAL.3.5,
// preemption-extended at B-HAL.4.5).
//
// VBAR_EL1 is set to point at `vectors` (2 KiB-aligned, 16 entries × 128 B):
//   0..3   Current EL with SP_EL0    (sync/IRQ/FIQ/SError)  — unused at EL1h
//   4      Current EL with SP_ELx Sync                      — unhandled fault
//   5      Current EL with SP_ELx IRQ                       — the timer IRQ
//   6..7   Current EL with SP_ELx FIQ/SError                — unhandled
//   8..15  Lower EL (aarch64 / aarch32)                     — unused at EL1
//
// Slot 5 branches to `irq_stub`, which saves the **full interrupt frame** of
// the interrupted thread — x0..x30 (31 GPRs) + ELR_EL1 + SPSR_EL1, 272 B,
// 16-byte aligned — and passes the saved-frame SP to `rust_irq_handler`. The
// handler returns the SP to resume on: the same one if no thread switch is
// needed (the B-HAL.3.5 tick-counter demo path), or a *different* thread's
// saved frame when preemptive scheduling is active (B-HAL.4.5). The stub then
// `mov sp, x0`, restores the full frame, and `eret`s — so the same code path
// services both "service this IRQ and resume the interrupted thread" and
// "service this IRQ and switch to a different thread". Every other slot
// `wfe`-loops — a clean park if anything else fires, since the skeleton has no
// other expected exception sources yet.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::aarch64::{gic, timer};

/// Number of generic-timer ticks the IRQ handler has serviced. The boot CPU
/// waits for a small count to prove IRQs are being taken end-to-end.
pub static TICK_COUNT: AtomicU32 = AtomicU32::new(0);

global_asm!(
    ".global vectors",
    ".balign 0x800",
    "vectors:",
    // 0: Current EL, SP_EL0, Sync.
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    // 1: Current EL, SP_EL0, IRQ.
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    // 2: Current EL, SP_EL0, FIQ.
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    // 3: Current EL, SP_EL0, SError.
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    // 4: Current EL, SP_ELx, Sync (an unhandled fault would land here).
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    // 5: Current EL, SP_ELx, IRQ — the generic timer.
    ".balign 0x80",
    "b irq_stub",
    // 6: Current EL, SP_ELx, FIQ.
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    // 7: Current EL, SP_ELx, SError.
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    // 8..11: Lower EL, aarch64.
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    // 12..15: Lower EL, aarch32.
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    ".balign 0x80",
    "1: wfe",
    "b 1b",
    // The IRQ stub — full-frame save (B-HAL.4.5 preemption-ready).
    //
    // Save x0..x30 (31 GPRs, 248 B) + ELR_EL1 + SPSR_EL1 (16 B) on the
    // interrupted thread's stack. Total 264 B; pad to 272 for 16-byte SP
    // alignment. Layout:
    //
    //   [sp, #0]   x0  / x1
    //   [sp, #16]  x2  / x3
    //   ...
    //   [sp, #240] x30
    //   [sp, #248] ELR_EL1
    //   [sp, #256] SPSR_EL1
    //   [sp, #264] (8 B pad)
    //
    // After the save: `mov x0, sp` — the saved-frame SP becomes arg0 to the
    // Rust handler. The handler returns the SP to resume on (the same one in
    // the tick-counter / non-preemption case; a different thread's saved
    // frame when scheduling). `mov sp, x0`, restore symmetrically, `eret`.
    // The saved frame format is the *same* one `sched_preempt::init_thread`
    // crafts for freshly-spawned threads, so the first preemption into a new
    // thread `eret`s to its `entry` at EL1h with IRQs unmasked.
    ".global irq_stub",
    "irq_stub:",
    "  sub  sp, sp, #272",
    "  stp  x0,  x1,  [sp, #0]",
    "  stp  x2,  x3,  [sp, #16]",
    "  stp  x4,  x5,  [sp, #32]",
    "  stp  x6,  x7,  [sp, #48]",
    "  stp  x8,  x9,  [sp, #64]",
    "  stp  x10, x11, [sp, #80]",
    "  stp  x12, x13, [sp, #96]",
    "  stp  x14, x15, [sp, #112]",
    "  stp  x16, x17, [sp, #128]",
    "  stp  x18, x19, [sp, #144]",
    "  stp  x20, x21, [sp, #160]",
    "  stp  x22, x23, [sp, #176]",
    "  stp  x24, x25, [sp, #192]",
    "  stp  x26, x27, [sp, #208]",
    "  stp  x28, x29, [sp, #224]",
    "  str  x30, [sp, #240]",
    "  mrs  x9, elr_el1",
    "  str  x9, [sp, #248]",
    "  mrs  x9, spsr_el1",
    "  str  x9, [sp, #256]",
    "  mov  x0, sp",
    "  bl   rust_irq_handler",
    "  mov  sp, x0",
    "  ldr  x9, [sp, #256]",
    "  msr  spsr_el1, x9",
    "  ldr  x9, [sp, #248]",
    "  msr  elr_el1, x9",
    "  ldr  x30, [sp, #240]",
    "  ldp  x28, x29, [sp, #224]",
    "  ldp  x26, x27, [sp, #208]",
    "  ldp  x24, x25, [sp, #192]",
    "  ldp  x22, x23, [sp, #176]",
    "  ldp  x20, x21, [sp, #160]",
    "  ldp  x18, x19, [sp, #144]",
    "  ldp  x16, x17, [sp, #128]",
    "  ldp  x14, x15, [sp, #112]",
    "  ldp  x12, x13, [sp, #96]",
    "  ldp  x10, x11, [sp, #80]",
    "  ldp  x8,  x9,  [sp, #64]",
    "  ldp  x6,  x7,  [sp, #48]",
    "  ldp  x4,  x5,  [sp, #32]",
    "  ldp  x2,  x3,  [sp, #16]",
    "  ldp  x0,  x1,  [sp, #0]",
    "  add  sp, sp, #272",
    "  eret",
);

extern "C" {
    #[link_name = "vectors"]
    static VECTORS_BASE: u8;
}

/// Install the vector table: VBAR_EL1 ← `vectors`, then `isb`.
///
/// # Safety
/// Call once at EL1 before unmasking interrupts (DAIF.I).
pub unsafe fn install() {
    let base = (&raw const VECTORS_BASE) as u64;
    unsafe { asm!("msr vbar_el1, {0}", "isb", in(reg) base, options(nostack)) };
}

/// The Rust half of the IRQ handler, called by `irq_stub` with the *full
/// interrupt frame* saved on the interrupted thread's stack at `current_sp`.
/// Reads GICC_IAR, services the timer PPI (reprograms the next tick + bumps
/// `TICK_COUNT`), EOIs the GIC. Returns the SP the stub should `mov sp, x0`
/// before restoring + `eret`:
///   - When preemptive scheduling is *not* active (the B-HAL.3.5 tick-counter
///     demo): return `current_sp` unchanged, so the stub restores the same
///     frame and `eret`s back to the interrupted thread.
///   - When preemptive scheduling *is* active (B-HAL.4.5): hand control to
///     `sched_preempt::schedule(current_sp)`, which records this thread's
///     SP, picks the next runnable thread, and returns *its* saved-frame SP.
///     The stub restores from there and `eret`s into the new thread.
#[no_mangle]
unsafe extern "C" fn rust_irq_handler(current_sp: u64) -> u64 {
    let ack = unsafe { gic::iar() };
    let intid = ack & 0x3ff;
    if intid == timer::TIMER_IRQ {
        // Reprogram for the next tick at the same 10 Hz rate.
        let interval = timer::frequency() / 10;
        unsafe { timer::arm(interval) };
        TICK_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    unsafe { gic::eoi(ack) };
    // B-HAL.4.5: if preemptive scheduling is enabled, this is where we pick
    // the next thread. Inactive ⇒ same SP, the interrupted thread resumes.
    if crate::arch::aarch64::sched_preempt::is_active() {
        unsafe { crate::arch::aarch64::sched_preempt::schedule(current_sp) }
    } else {
        current_sp
    }
}
