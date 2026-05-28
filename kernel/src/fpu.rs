// kernel/src/fpu.rs
//
// The arch-agnostic FPU/SSE management facade — it sits on the `hal::Fpu` seam
// (B-HAL.1). The register mechanics (CR0/CR4 enable, fxsave/fxrstor, the clean
// template) now live behind the HAL in `arch/<isa>/fpu.rs`; this module keeps
// the `fpu::*` API the scheduler uses (init/save/restore/clean) and re-exports
// the per-thread save-area type, so sched.rs + main.rs are unchanged.
//
// Eager save/restore (on switch-out/switch-in) rather than lazy (CR0.TS + #NM
// trap-on-first-use): simpler, correct, and avoids the lazy-FP-restore
// information-leak class — fine at this scale.

pub use crate::hal::FpuState;
use crate::hal::{self, Fpu as _};

/// Enable SSE + x87 on the calling core and initialize the FPU, capturing the
/// clean state as the template for new threads. Call once per core (BSP + each
/// AP) before the scheduler runs. Idempotent.
pub fn init_this_cpu() {
    hal::fpu().init();
}

/// FXSAVE the live x87/SSE register file into `area`.
///
/// # Safety
/// `area` must point at a writable, 16-byte-aligned 512-byte `FpuState`.
pub unsafe fn save(area: *mut FpuState) {
    unsafe { hal::fpu().save(area) };
}

/// FXRSTOR the live x87/SSE register file from `area`.
///
/// # Safety
/// `area` must point at a valid FXSAVE image (e.g. from `save` or `clean`).
pub unsafe fn restore(area: *const FpuState) {
    unsafe { hal::fpu().restore(area) };
}

/// A copy of the clean (post-`fninit`) FPU template — the initial state for a
/// freshly spawned thread or an `exec`'d image.
pub fn clean() -> FpuState {
    hal::fpu().clean()
}
