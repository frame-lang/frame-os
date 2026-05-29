// kernel/src/arch/aarch64.rs
//
// The AArch64 HAL implementation root (B-HAL.3). The ARM counterpart of
// `arch/x86_64.rs`: it provides the *mechanism* behind the `hal.rs` traits for
// AArch64, selected at build time by `cfg(target_arch = "aarch64")`.
//
// B-HAL.3 is a staged bring-up, so only the concerns implemented so far appear
// here: `boot` (the `_start` entry) and `serial` (the PL011 `Console`). The
// remaining HAL concerns (Cpu / Clock / Fpu / Mmu / PerCpu, then the deferred
// Irq / Timer / Context / SyscallEntry) are added in B-HAL.3.4+/.4/.5 — at which
// point `hal.rs`'s currently x86-only accessors gain their aarch64 branch.

pub mod boot;
pub mod fdt;
pub mod serial;

/// The console device type the `hal::console()` accessor returns on aarch64.
pub type ConsoleDevice = serial::Pl011;

pub use serial::console;
