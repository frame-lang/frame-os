// kernel/src/fpu.rs
//
// x87/SSE (FPU) state management (B11-3a). Pure native — register mechanics.
//
// Until now no kernel or user code touched the FPU/SSE registers (the kernel is
// integer-only; the hand-written user programs and the gcc-built C programs
// compiled with -ffreestanding used zero XMM). The on-device C toolchain (tcc,
// B11-3) changes that: tcc and the code it compiles use SSE for `double` and
// x87 for `long double`. Two things become necessary:
//
//   1. SSE must be *enabled* (CR0.EM=0, CR0.MP=1, CR4.OSFXSR|OSXMMEXCPT) and the
//      FPU initialized (`fninit`). Limine already enables SSE per its boot
//      protocol, but we assert the bits + `fninit` so x87/MXCSR are in a known
//      state and the APs are covered too — idempotent and cheap.
//
//   2. The FPU/SSE register file must be *saved and restored across context
//      switches*, exactly like the GPRs. The scheduler (sched.rs) previously
//      saved only the 15 GPRs + the iretq frame; the XMM/x87 file was shared,
//      so two preemptively-interleaved FPU users would clobber each other's
//      registers (the same class of corruption as a missing GPR save, just for
//      the FPU). This module provides the FXSAVE/FXRSTOR primitives + a clean
//      template; sched.rs owns the per-thread save areas and the switch.
//
// Eager save/restore (FXSAVE on switch-out, FXRSTOR on switch-in) rather than
// lazy (CR0.TS + #NM trap-on-first-use): eager is simpler, correct, and avoids
// the lazy-FP-restore information-leak class — fine at this scale.

use core::arch::asm;

/// One thread's saved x87 + SSE state: the 512-byte FXSAVE image. `fxsave`/
/// `fxrstor` require a 16-byte-aligned destination, hence `align(16)`.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct FpuState([u8; 512]);

impl FpuState {
    pub const fn zeroed() -> Self {
        FpuState([0; 512])
    }
}

// The post-`fninit` "clean" FPU image, captured once at `init_this_cpu`. New
// threads start from this (a zeroed area is NOT a valid FXSAVE image — its
// MXCSR=0 unmasks every SSE exception, so the first `double` op would #XM).
static mut CLEAN: FpuState = FpuState::zeroed();

/// Enable SSE + x87 on the calling core and initialize the FPU, then capture the
/// clean state as the template for new threads. Call once per core (BSP + each
/// AP) before the scheduler runs. Idempotent.
pub fn init_this_cpu() {
    unsafe {
        asm!(
            "mov {t}, cr0",
            "and {t}, {clr_em}", // CR0.EM = 0: no x87 emulation (use the real FPU)
            "or  {t}, {set_mp}", // CR0.MP = 1: monitor coprocessor
            "mov cr0, {t}",
            "mov {t}, cr4",
            "or  {t}, {set_sse}", // CR4.OSFXSR | OSXMMEXCPT: FXSAVE + SSE exceptions
            "mov cr4, {t}",
            "fninit", // x87 to a known state
            t = out(reg) _,
            clr_em = const !(1u64 << 2),
            set_mp = const 1u64 << 1,
            set_sse = const (1u64 << 9) | (1u64 << 10),
            options(nostack, preserves_flags),
        );
        // Capture the clean state (post-fninit; MXCSR at its 0x1F80 reset default).
        save(&raw mut CLEAN);
    }
}

/// FXSAVE the live x87/SSE register file into `area` (512 bytes, 16-aligned).
///
/// # Safety
/// `area` must point at a writable, 16-byte-aligned 512-byte `FpuState`.
#[inline]
pub unsafe fn save(area: *mut FpuState) {
    asm!("fxsave [{}]", in(reg) area, options(nostack, preserves_flags));
}

/// FXRSTOR the live x87/SSE register file from `area`.
///
/// # Safety
/// `area` must point at a valid FXSAVE image (e.g. from `save` or `clean`).
#[inline]
pub unsafe fn restore(area: *const FpuState) {
    asm!("fxrstor [{}]", in(reg) area, options(nostack, readonly, preserves_flags));
}

/// A copy of the clean (post-`fninit`) FPU template — the initial state for a
/// freshly spawned thread or an `exec`'d image.
pub fn clean() -> FpuState {
    unsafe { (&raw const CLEAN).read() }
}
