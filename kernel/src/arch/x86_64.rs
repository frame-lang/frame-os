// kernel/src/arch/x86_64.rs
//
// The x86_64 HAL implementation root (B-HAL.1). Each submodule here provides
// the *mechanism* behind one `hal.rs` trait for x86_64 — the hand-written
// `asm!` (port I/O, MSRs, CR3, IDT/GDT, context switch) that the audit in
// docs/plans/b_hal.md catalogues, relocated behind the seam.
//
// This module re-exports, per concern, the concrete device type the HAL
// accessor returns (`ConsoleDevice`, …) plus the accessor itself. `hal.rs`
// names only these neutral aliases, so an AArch64 sibling exposing the same
// alias names drops in without touching the kernel above the HAL.
//
// B-HAL.1 lands the first concern (`Console`); the rest of the coupling map
// (Cpu / Mmu / Irq / Timer / Clock / PerCpu / Fpu / Context / Boot /
// SyscallEntry) is added here as each seam is extracted.

pub mod serial;

/// The console device type the `hal::console()` accessor returns on x86_64.
pub type ConsoleDevice = serial::Uart;

pub use serial::console;
