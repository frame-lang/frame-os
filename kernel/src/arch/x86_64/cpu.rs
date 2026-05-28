// kernel/src/arch/x86_64/cpu.rs
//
// The x86_64 implementation of `hal::Cpu`: maskable-interrupt control (sti /
// cli), the interrupt-enable state (RFLAGS.IF), and halt (hlt) — the CPU
// control primitives the kernel's critical sections and idle loops are built on
// (B-HAL.1).
//
// These are the *mechanism*, relocated behind the HAL seam. The arch-agnostic
// utilities on top of them — the `without_interrupts` critical section, the
// `enable`/`disable`/`wait_for_interrupt` named wrappers — live in
// `interrupts.rs` and call through `hal::cpu()`. The IRQ-safe `SpinLock`
// (spin.rs) calls these primitives directly (it sits below the interrupts
// module, and it's the hot path), so the methods are `#[inline]`: in release
// each collapses to the single instruction it always was.
//
// Note: the PAUSE spin-loop hint is *not* a HAL primitive — `core::hint::
// spin_loop()` already abstracts it portably (PAUSE on x86, YIELD on ARM).

use crate::hal::Cpu;

/// The x86_64 CPU control surface. A zero-sized handle — the HAL's `Cpu` device.
pub struct X86Cpu;

static CPU: X86Cpu = X86Cpu;

/// The x86_64 CPU (the executing core's control surface).
pub fn cpu() -> &'static X86Cpu {
    &CPU
}

impl Cpu for X86Cpu {
    #[inline]
    fn enable_irqs(&self) {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
    }

    #[inline]
    fn disable_irqs(&self) {
        unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
    }

    #[inline]
    fn irqs_enabled(&self) -> bool {
        // Bit 9 of RFLAGS is IF (interrupt-enable). `pushfq` writes the stack,
        // so `nostack` must NOT be set; `nomem` is correct (no other memory).
        let flags: u64;
        unsafe {
            core::arch::asm!("pushfq; pop {}", out(reg) flags, options(nomem, preserves_flags));
        }
        flags & (1 << 9) != 0
    }

    #[inline]
    fn halt(&self) {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }

    #[inline]
    fn enable_irqs_and_halt(&self) {
        // `sti` takes effect only after the *next* instruction, so this pair
        // atomically enables-then-halts with no wake-losing window: an IRQ that
        // arrives between the two cannot be missed.
        unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
    }
}
