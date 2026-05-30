// kernel/src/arch/aarch64/vectors.rs
//
// EL1 exception vector table + IRQ stub + Rust IRQ handler (B-HAL.3.5).
//
// VBAR_EL1 is set to point at `vectors` (2 KiB-aligned, 16 entries × 128 B):
//   0..3   Current EL with SP_EL0    (sync/IRQ/FIQ/SError)  — unused at EL1h
//   4      Current EL with SP_ELx Sync                      — unhandled fault
//   5      Current EL with SP_ELx IRQ                       — the timer IRQ
//   6..7   Current EL with SP_ELx FIQ/SError                — unhandled
//   8..15  Lower EL (aarch64 / aarch32)                     — unused at EL1
//
// Slot 5 branches to `irq_stub`, which saves the caller-saved GPRs + FP/LR,
// calls `rust_irq_handler` (reads GICC_IAR, reprograms the generic timer, bumps
// `TICK_COUNT`, writes GICC_EOIR), restores, and `eret`s. Every other slot
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
    // The IRQ stub. Save caller-saved x0..x18, FP (x29), and LR (x30) — the
    // Rust handler will preserve x19..x28 per the SysV-style ABI. 22 regs ⇒
    // 176 bytes, 16-byte aligned. Then bl, restore, eret.
    ".global irq_stub",
    "irq_stub:",
    "  sub  sp, sp, #176",
    "  stp  x0,  x1,  [sp, #0]",
    "  stp  x2,  x3,  [sp, #16]",
    "  stp  x4,  x5,  [sp, #32]",
    "  stp  x6,  x7,  [sp, #48]",
    "  stp  x8,  x9,  [sp, #64]",
    "  stp  x10, x11, [sp, #80]",
    "  stp  x12, x13, [sp, #96]",
    "  stp  x14, x15, [sp, #112]",
    "  stp  x16, x17, [sp, #128]",
    "  stp  x18, x29, [sp, #144]",
    "  str  x30, [sp, #160]",
    "  bl   rust_irq_handler",
    "  ldr  x30, [sp, #160]",
    "  ldp  x18, x29, [sp, #144]",
    "  ldp  x16, x17, [sp, #128]",
    "  ldp  x14, x15, [sp, #112]",
    "  ldp  x12, x13, [sp, #96]",
    "  ldp  x10, x11, [sp, #80]",
    "  ldp  x8,  x9,  [sp, #64]",
    "  ldp  x6,  x7,  [sp, #48]",
    "  ldp  x4,  x5,  [sp, #32]",
    "  ldp  x2,  x3,  [sp, #16]",
    "  ldp  x0,  x1,  [sp, #0]",
    "  add  sp, sp, #176",
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

/// The Rust half of the IRQ handler, called by `irq_stub` with all GPRs saved.
/// Reads GICC_IAR, services the timer PPI (reprograms the next tick + bumps the
/// counter), and EOIs the GIC.
#[no_mangle]
unsafe extern "C" fn rust_irq_handler() {
    let ack = unsafe { gic::iar() };
    let intid = ack & 0x3ff;
    if intid == timer::TIMER_IRQ {
        // Reprogram for the next tick at the same 10 Hz rate.
        let interval = timer::frequency() / 10;
        unsafe { timer::arm(interval) };
        TICK_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    unsafe { gic::eoi(ack) };
}
