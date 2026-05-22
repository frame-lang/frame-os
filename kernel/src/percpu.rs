// kernel/src/percpu.rs
//
// Per-CPU data (B7 Step 1). Each core gets its own `PerCpu` block, and points
// the GS segment base at it (via the IA32_GS_BASE MSR) so a core can find "its"
// state with a single `gs:[..]` access — the standard x86_64 per-CPU mechanism
// (Linux's `__per_cpu`, the `%gs`-relative this_cpu). The BSP and every AP call
// `init_this_cpu` once at startup.
//
// At B7 Step 1 the block holds just identity (index + LAPIC id); later steps add
// the per-CPU current task, run-queue handle, and TSS pointer here.

use core::arch::asm;

/// Max cores we support (QEMU is launched with `-smp 4`; headroom to 8).
pub const MAX_CPUS: usize = 8;

/// One core's private block. `cpu_index` is first so `gs:[0]` reads it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PerCpu {
    pub cpu_index: u32,
    pub lapic_id: u32,
}

const PERCPU_INIT: PerCpu = PerCpu {
    cpu_index: 0,
    lapic_id: 0,
};
static mut PERCPU: [PerCpu; MAX_CPUS] = [PERCPU_INIT; MAX_CPUS];

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

/// Initialize this core's per-CPU block and point GS base at it. Called once per
/// core (BSP with index 0, each AP with its assigned index) at startup.
pub fn init_this_cpu(cpu_index: usize, lapic_id: u32) {
    // Bind the array base as a raw pointer, then offset — indexing the static
    // directly (`PERCPU[i]`) would create a reference to a mutable static.
    let base = &raw mut PERCPU as *mut PerCpu;
    let p = unsafe { base.add(cpu_index) };
    unsafe {
        (*p).cpu_index = cpu_index as u32;
        (*p).lapic_id = lapic_id;
        wrmsr(IA32_GS_BASE, p as u64);
    }
}

/// This core's index, read through GS base (`cpu_index` is the first field of
/// the GS-based `PerCpu`). Valid only after `init_this_cpu` on this core.
pub fn this_cpu_index() -> u32 {
    let v: u32;
    unsafe {
        asm!("mov {0:e}, gs:[0]", out(reg) v, options(nostack, preserves_flags));
    }
    v
}
