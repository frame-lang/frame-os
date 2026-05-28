// kernel/src/arch/x86_64/fpu.rs
//
// The x86_64 implementation of `hal::Fpu`: x87/SSE register-file management
// (B11-3a), relocated behind the HAL seam (B-HAL.1).
//
// The on-device C toolchain (tcc) and the code it compiles use SSE for `double`
// and x87 for `long double`, so two things are necessary:
//
//   1. SSE must be *enabled* (CR0.EM=0, CR0.MP=1, CR4.OSFXSR|OSXMMEXCPT) and the
//      FPU initialized (`fninit`). Limine already enables SSE per its boot
//      protocol, but we assert the bits + `fninit` so x87/MXCSR are in a known
//      state and the APs are covered too — idempotent and cheap.
//
//   2. The FPU/SSE register file must be *saved and restored across context
//      switches*, exactly like the GPRs (sched.rs owns the per-thread save
//      areas and the switch; this module provides the FXSAVE/FXRSTOR primitives
//      + the clean template).
//
// Eager save/restore (FXSAVE on switch-out, FXRSTOR on switch-in) rather than
// lazy (CR0.TS + #NM trap-on-first-use): eager is simpler, correct, and avoids
// the lazy-FP-restore information-leak class — fine at this scale.
//
// `FpuState` (the 512-byte FXSAVE image) is x86-specific and is the type the
// scheduler embeds per-thread; the HAL re-exports it as `hal::FpuState` so the
// arch-agnostic scheduler names it without naming this module.

use crate::hal::Fpu;
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

// The post-`fninit` "clean" FPU image, captured once at `init`. New threads
// start from this (a zeroed area is NOT a valid FXSAVE image — its MXCSR=0
// unmasks every SSE exception, so the first `double` op would #XM).
static mut CLEAN: FpuState = FpuState::zeroed();

/// The x86_64 FPU control surface. A zero-sized handle — the HAL's `Fpu` device.
pub struct X86Fpu;

static FPU: X86Fpu = X86Fpu;

/// The x86_64 FPU (the executing core's x87/SSE register file).
pub fn fpu() -> &'static X86Fpu {
    &FPU
}

impl Fpu for X86Fpu {
    fn init(&self) {
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
            self.save(&raw mut CLEAN);
        }
    }

    #[inline]
    unsafe fn save(&self, area: *mut FpuState) {
        unsafe { asm!("fxsave [{}]", in(reg) area, options(nostack, preserves_flags)) };
    }

    #[inline]
    unsafe fn restore(&self, area: *const FpuState) {
        unsafe { asm!("fxrstor [{}]", in(reg) area, options(nostack, readonly, preserves_flags)) };
    }

    fn clean(&self) -> FpuState {
        unsafe { (&raw const CLEAN).read() }
    }
}
