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

pub mod context;
pub mod cpu;
pub mod fpu;
pub mod paging;
pub mod percpu;
pub mod rtc;
pub mod serial;

/// The console device type the `hal::console()` accessor returns on x86_64.
pub type ConsoleDevice = serial::Uart;
/// The cooperative context-switch type the `hal::context()` accessor returns on x86_64.
pub type ContextDevice = context::X86Context;
/// The CPU control surface type the `hal::cpu()` accessor returns on x86_64.
pub type CpuDevice = cpu::X86Cpu;
/// The wall-clock device type the `hal::clock()` accessor returns on x86_64.
pub type ClockDevice = rtc::CmosRtc;
/// The FPU control surface type the `hal::fpu()` accessor returns on x86_64.
pub type FpuDevice = fpu::X86Fpu;
/// The MMU type the `hal::mmu()` accessor returns on x86_64.
pub type MmuDevice = paging::X86Mmu;
/// The per-CPU base register type the `hal::per_cpu()` accessor returns on x86_64.
pub type PerCpuDevice = percpu::X86PerCpu;

pub use context::context;
pub use cpu::cpu;
pub use fpu::{fpu, FpuState};
pub use paging::mmu;
pub use percpu::per_cpu;
pub use rtc::clock;
pub use serial::console;
