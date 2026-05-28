// kernel/src/arch/x86_64/percpu.rs
//
// The x86_64 implementation of `hal::PerCpu`: the per-core base register
// (B-HAL.1). x86_64 points the GS segment base at this core's per-CPU block via
// the IA32_GS_BASE MSR, so a core finds "its" state with a single `gs:[..]`
// access — the standard x86_64 per-CPU mechanism (Linux's `__per_cpu`, the
// `%gs`-relative this_cpu). AArch64's counterpart is TPIDR_EL1.
//
// This is the *mechanism* (the MSR write + the gs-relative read), relocated
// behind the HAL seam. The per-CPU data blocks + the per-field accessors are
// arch-agnostic and live in the top-level `percpu.rs`, which calls through
// `hal::per_cpu()`.

use crate::hal::PerCpu;
use core::arch::asm;

const IA32_GS_BASE: u32 = 0xC000_0101;

unsafe fn wrmsr(msr: u32, val: u64) {
    let lo = val as u32;
    let hi = (val >> 32) as u32;
    asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") lo,
        in("edx") hi,
        options(nostack, preserves_flags),
    );
}

/// The x86_64 per-CPU base register surface. A zero-sized handle — the HAL's
/// `PerCpu` device.
pub struct X86PerCpu;

static PER_CPU: X86PerCpu = X86PerCpu;

/// The x86_64 per-CPU base register (GS base).
pub fn per_cpu() -> &'static X86PerCpu {
    &PER_CPU
}

impl PerCpu for X86PerCpu {
    unsafe fn set_base(&self, base: u64) {
        unsafe { wrmsr(IA32_GS_BASE, base) };
    }

    fn this_cpu_index(&self) -> u32 {
        // `cpu_index` is the first field of the GS-based per-CPU block, so
        // `gs:[0]` reads it in one instruction.
        let v: u32;
        unsafe {
            asm!("mov {0:e}, gs:[0]", out(reg) v, options(nostack, preserves_flags));
        }
        v
    }
}
