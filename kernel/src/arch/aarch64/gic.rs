// kernel/src/arch/aarch64/gic.rs
//
// Minimal GICv2 distributor + CPU-interface bring-up on QEMU `virt` (B-HAL.3.5).
// The harness forces `-machine virt,gic-version=2` so the GIC is plain MMIO at
// the well-known `virt` addresses below. (QEMU's default is GICv3 with the
// system-register interface + a redistributor; the v2 path is enough to
// establish the `Irq` trait shape against real ARM hardware here, and the v3
// variant lands when the trait is implemented for real in B-HAL.4/.5.)

use core::ptr::{read_volatile, write_volatile};

// QEMU `virt` GICv2 base addresses.
const GICD: usize = 0x0800_0000;
const GICC: usize = 0x0801_0000;

// Distributor offsets.
const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER: usize = 0x100; // base; +4 per 32 IRQs

// CPU-interface offsets.
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00C;
const GICC_EOIR: usize = 0x010;

fn d(off: usize) -> *mut u32 {
    (GICD + off) as *mut u32
}
fn c(off: usize) -> *mut u32 {
    (GICC + off) as *mut u32
}

/// Enable the distributor + CPU interface; allow all priorities through.
///
/// # Safety
/// Touches GIC MMIO; GICv2 must be present (`-machine virt,gic-version=2`).
pub unsafe fn init() {
    unsafe {
        write_volatile(d(GICD_CTLR), 1);
        write_volatile(c(GICC_PMR), 0xFF);
        write_volatile(c(GICC_CTLR), 1);
    }
}

/// Unmask interrupt `irq` (set its enable bit in the distributor).
///
/// # Safety
/// As [`init`]; `irq` must be a valid interrupt id.
pub unsafe fn unmask(irq: u32) {
    let reg = d(GICD_ISENABLER + ((irq / 32) as usize) * 4);
    unsafe { write_volatile(reg, 1 << (irq % 32)) };
}

/// Acknowledge: read GICC_IAR (returns the pending interrupt id + CPU id; 1023
/// indicates "spurious").
///
/// # Safety
/// As [`init`].
pub unsafe fn iar() -> u32 {
    unsafe { read_volatile(c(GICC_IAR)) }
}

/// End-of-interrupt: write the IAR value back to GICC_EOIR.
///
/// # Safety
/// As [`init`].
pub unsafe fn eoi(iar_val: u32) {
    unsafe { write_volatile(c(GICC_EOIR), iar_val) };
}
