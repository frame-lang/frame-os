// kernel/src/arch/aarch64/percpu.rs
//
// The aarch64 implementation of `hal::PerCpu` (B-HAL.4.0). x86's per-CPU base
// register is the GS-base MSR (read in one instruction via `gs:[0]`); the ARM
// counterpart is `TPIDR_EL1` (Thread Process ID Register at EL1), accessed via
// `msr`/`mrs`. With this seam in place, the arch-agnostic per-CPU *data* layer
// in the top-level `percpu.rs` compiles for both ISAs — the same `PerCpu`
// struct, the same accessors, just a different base-register primitive
// underneath.

use crate::hal::PerCpu;
use core::arch::asm;

/// The aarch64 per-CPU base-register surface. A zero-sized handle — the HAL's
/// `PerCpu` device.
pub struct AArch64PerCpu;

static PER_CPU: AArch64PerCpu = AArch64PerCpu;

/// The aarch64 per-CPU base register (`TPIDR_EL1`).
pub fn per_cpu() -> &'static AArch64PerCpu {
    &PER_CPU
}

impl PerCpu for AArch64PerCpu {
    unsafe fn set_base(&self, base: u64) {
        unsafe { asm!("msr tpidr_el1, {0}", in(reg) base, options(nomem, nostack)) };
    }

    fn this_cpu_index(&self) -> u32 {
        // Read the per-CPU base, then deref the first u32 (cpu_index is the
        // first field of the `PerCpu` data block).
        let base: u64;
        unsafe { asm!("mrs {0}, tpidr_el1", out(reg) base, options(nomem, nostack)) };
        unsafe { core::ptr::read_volatile(base as *const u32) }
    }
}
