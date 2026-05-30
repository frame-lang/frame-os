// kernel/src/arch/aarch64/timer.rs
//
// ARM generic timer bring-up (B-HAL.3.5). On QEMU `virt`, the EL1 non-secure
// physical timer fires PPI 30. We program CNTP_TVAL_EL0 with a countdown of
// `CNTFRQ_EL0 / hz` cycles, then enable CNTP_CTL_EL0 (bit 0 = ENABLE,
// bit 1 = IMASK clear). The IRQ handler (see `vectors.rs`) reprograms TVAL for
// the next tick on every fire.

use core::arch::asm;

/// PPI used by the EL1 non-secure physical timer on QEMU `virt`.
pub const TIMER_IRQ: u32 = 30;

/// CNTFRQ_EL0 — the generic-timer frequency in Hz (set by the firmware/QEMU).
pub fn frequency() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {0}, cntfrq_el0", out(reg) v, options(nomem, nostack)) };
    v
}

/// Program the next tick to fire in `ticks` cycles, and enable the timer.
///
/// # Safety
/// Mutates this CPU's CNTP_TVAL / CNTP_CTL state.
pub unsafe fn arm(ticks: u64) {
    unsafe {
        asm!("msr cntp_tval_el0, {0}", in(reg) ticks, options(nomem, nostack));
        let ctl: u64 = 1; // ENABLE=1, IMASK=0
        asm!("msr cntp_ctl_el0, {0}", in(reg) ctl, options(nomem, nostack));
    }
}

/// Initialize the timer to fire at roughly `hz` Hz. The first tick comes in
/// `CNTFRQ_EL0 / hz` cycles.
///
/// # Safety
/// As [`arm`].
pub unsafe fn init(hz: u32) {
    let interval = frequency() / hz as u64;
    unsafe { arm(interval) };
}
