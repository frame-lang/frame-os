// kernel/src/hal.rs
//
// The Hardware Abstraction Layer — the small set of arch *traits* the
// platform-agnostic kernel sits on (B-HAL.1).
//
// The discipline (FSM-owns-logic / native-owns-mechanism, applied at the
// platform level): the Frame FSMs + the pure-logic subsystems call into a
// named trait here; the per-arch *mechanism* lives under `arch/<isa>/` and
// implements the trait. The trait is the seam. There is exactly ONE arch impl
// in any given binary, selected at **build time** by `cfg(target_arch)` — we
// never swap impls at runtime, so the accessors below resolve to a concrete
// type with no dynamic dispatch. Adding AArch64 is `cfg`-selecting a second
// `arch::aarch64` module that implements the same traits; the kernel above the
// HAL does not change.
//
// B-HAL.1 introduces the first (smallest, most-isolated) seam — `Console` —
// to prove the trait shape + the `arch/x86_64/` module layout + the accessor
// pattern before fanning out to Cpu / Mmu / Irq / Timer / … (the coupling map
// in docs/plans/b_hal.md).

// Build-time architecture selection: exactly one `imp` is in scope per target.
// The body of this module stays arch-neutral — it only ever names `imp::*`.
#[cfg(target_arch = "x86_64")]
use crate::arch::x86_64 as imp;

/// The platform console: byte-level I/O over the primary UART.
///
/// x86_64 implements this over the 16550 (COM1); a future AArch64 port
/// implements it over a PL011. The trait is intentionally minimal — only the
/// genuinely arch-specific primitives. The arch-agnostic text layer
/// (`write_str` / `writeln` / `write_hex_u64` / `write_u32_decimal`) lives in
/// `serial.rs`, which sits *on* this trait and is shared by every arch.
pub trait Console {
    /// Program the UART for polled, interrupt-free operation (baud, line
    /// format, FIFO). Must run before any write or output is garbage — the
    /// `SerialDriver` FSM makes that ordering structural.
    fn init(&self);

    /// Write a single byte, waiting for the transmit holding register to be
    /// empty first (polled TX).
    fn write_byte(&self, b: u8);

    /// Read one received byte if the UART has data waiting (polled RX), else
    /// `None`. The RX interrupt handler drains the FIFO by calling this.
    fn rx_byte(&self) -> Option<u8>;

    /// Enable the received-data-available interrupt and route the UART's IRQ
    /// line to the interrupt controller. Call after the IDT/controller are up —
    /// this is what makes the console interactive. TX stays polled.
    #[cfg(feature = "interactive")]
    fn enable_rx_interrupt(&self);
}

/// The console device for this build's target architecture (build-time
/// selected, concrete type — no vtable). Callers bring the methods into scope
/// with `use crate::hal::Console`.
pub fn console() -> &'static imp::ConsoleDevice {
    imp::console()
}

/// CPU control primitives: maskable-interrupt enable/disable, the
/// interrupt-enable state, and halt.
///
/// x86_64 implements these over `sti` / `cli` / `hlt` and RFLAGS.IF; a future
/// AArch64 port implements them over `msr daifclr/daifset`, `wfi`, and DAIF.I.
/// The methods are the hot path of the IRQ-safe spinlock, so the arch impl
/// marks them `#[inline]`. (The PAUSE spin-loop hint is *not* here — it's
/// already portable via `core::hint::spin_loop()`.)
pub trait Cpu {
    /// Enable maskable interrupts.
    fn enable_irqs(&self);

    /// Disable maskable interrupts.
    fn disable_irqs(&self);

    /// Whether maskable interrupts are currently enabled on this core.
    fn irqs_enabled(&self) -> bool;

    /// Halt until the next interrupt (no busy-spin). With interrupts enabled
    /// this wakes on the next IRQ.
    fn halt(&self);

    /// Enable interrupts and halt as one step, with no wake-losing window —
    /// used to yield to the scheduler from an interrupts-off context.
    fn enable_irqs_and_halt(&self);
}

/// The CPU control surface for this build's target architecture (build-time
/// selected, concrete type — no vtable). Callers bring the methods into scope
/// with `use crate::hal::Cpu`.
pub fn cpu() -> &'static imp::CpuDevice {
    imp::cpu()
}
