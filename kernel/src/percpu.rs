// kernel/src/percpu.rs
//
// Per-CPU data (B7 Step 1). Each core gets its own `PerCpu` block, and points
// its per-core base register at it so a core can find "its" state with a single
// access — the standard per-CPU mechanism (Linux's `__per_cpu`, the `%gs`-
// relative this_cpu on x86). The BSP and every AP call `init_this_cpu` once at
// startup.
//
// This module owns the arch-agnostic *data* (the per-CPU blocks + field
// accessors); the base-register mechanism (x86_64: IA32_GS_BASE MSR + the
// gs-relative index read) lives behind `hal::PerCpu` in `arch/<isa>/percpu.rs`
// (B-HAL.1). The `PerCpu` *struct* here and the `hal::PerCpu` *trait* share a
// name, so the trait is imported anonymously (`as _`).
//
// At B7 Step 1 the block holds just identity (index + LAPIC id); later steps add
// the per-CPU current task, run-queue handle, and TSS pointer here.

use crate::hal::{self, PerCpu as _};

/// Max cores we support (QEMU is launched with `-smp 4`; headroom to 8).
pub const MAX_CPUS: usize = 8;

/// One core's private block. `cpu_index` is first so `gs:[0]` reads it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PerCpu {
    pub cpu_index: u32,
    pub lapic_id: u32,
    /// LAPIC-timer ticks this core has taken (B7 Step 4 — proof of per-core
    /// preemption). Written only by this core's timer ISR.
    pub ticks: u64,
    /// Work iterations this core completed between ticks (proof it ran a thread).
    pub work: u64,
}

const PERCPU_INIT: PerCpu = PerCpu {
    cpu_index: 0,
    lapic_id: 0,
    ticks: 0,
    work: 0,
};
static mut PERCPU: [PerCpu; MAX_CPUS] = [PERCPU_INIT; MAX_CPUS];

/// Initialize this core's per-CPU block and point the per-core base register at
/// it. Called once per core (BSP with index 0, each AP with its assigned index)
/// at startup.
pub fn init_this_cpu(cpu_index: usize, lapic_id: u32) {
    // Bind the array base as a raw pointer, then offset — indexing the static
    // directly (`PERCPU[i]`) would create a reference to a mutable static.
    let base = &raw mut PERCPU as *mut PerCpu;
    let p = unsafe { base.add(cpu_index) };
    unsafe {
        (*p).cpu_index = cpu_index as u32;
        (*p).lapic_id = lapic_id;
        hal::per_cpu().set_base(p as u64);
    }
}

/// This core's index, read through the per-core base register. Valid only after
/// `init_this_cpu` on this core.
pub fn this_cpu_index() -> u32 {
    hal::per_cpu().this_cpu_index()
}

fn slot(index: usize) -> *mut PerCpu {
    let base = &raw mut PERCPU as *mut PerCpu;
    unsafe { base.add(index) }
}

/// Record a LAPIC-timer tick on the calling core. Called from this core's timer
/// ISR; the field is single-writer (only this core touches its own slot).
pub fn record_tick() {
    let p = slot(this_cpu_index() as usize);
    unsafe { (*p).ticks += 1 };
}

/// This core's tick count, read by its own preemptible loop. Volatile so the
/// loop re-reads it (the ISR updates it asynchronously on the same core).
pub fn this_cpu_ticks() -> u64 {
    let p = slot(this_cpu_index() as usize);
    unsafe { core::ptr::read_volatile(&(*p).ticks) }
}

/// Store this core's completed work-iteration count.
pub fn set_this_cpu_work(w: u64) {
    let p = slot(this_cpu_index() as usize);
    unsafe { (*p).work = w };
}

/// Read core `index`'s tick count (the BSP reads each AP's after the demo).
pub fn cpu_ticks(index: usize) -> u64 {
    unsafe { (*slot(index)).ticks }
}

/// Read core `index`'s work count.
pub fn cpu_work(index: usize) -> u64 {
    unsafe { (*slot(index)).work }
}
