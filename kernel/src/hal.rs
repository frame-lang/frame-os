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

/// Wall-clock time. x86_64 reads the CMOS RTC; a future AArch64 port reads the
/// RPi firmware mailbox / an RTC chip. The decode of the source format into
/// epoch seconds is the arch impl's concern, so the trait is just the result.
pub trait Clock {
    /// Current wall-clock time as Unix epoch seconds (UTC).
    fn epoch_secs(&self) -> u64;
}

/// The wall-clock source for this build's target architecture (build-time
/// selected, concrete type — no vtable). Callers bring the method into scope
/// with `use crate::hal::Clock`.
pub fn clock() -> &'static imp::ClockDevice {
    imp::clock()
}

/// The per-thread FPU/SSE save area for this build's target architecture. It's
/// arch-specific (x86_64: the 512-byte FXSAVE image; AArch64: the NEON/FP `Q`
/// regs + FPSR/FPCR), and the scheduler embeds one per thread — so the HAL
/// re-exports the concrete type here rather than the kernel naming an arch
/// module. `FpuState::zeroed()` is `const` for use in static save-area arrays.
pub use imp::FpuState;

/// FPU/SSE register-file management: per-core enable + initialize, and
/// save/restore of the register file across context switches.
///
/// x86_64 implements these over CR0/CR4 + `fxsave`/`fxrstor`; a future AArch64
/// port over the FP/NEON enable bits + the `Q`-register save area.
pub trait Fpu {
    /// Enable the FPU/SSE on the calling core and initialize it, capturing the
    /// clean template for new threads. Call once per core before the scheduler
    /// runs. Idempotent.
    fn init(&self);

    /// Save the live FPU/SSE register file into `area`.
    ///
    /// # Safety
    /// `area` must point at a writable, correctly-aligned [`FpuState`].
    unsafe fn save(&self, area: *mut FpuState);

    /// Restore the live FPU/SSE register file from `area`.
    ///
    /// # Safety
    /// `area` must point at a valid saved image (from [`Fpu::save`] or
    /// [`Fpu::clean`]).
    unsafe fn restore(&self, area: *const FpuState);

    /// A copy of the clean (post-init) FPU template — the initial state for a
    /// freshly spawned thread or an `exec`'d image.
    fn clean(&self) -> FpuState;
}

/// The FPU control surface for this build's target architecture (build-time
/// selected, concrete type — no vtable). Callers bring the methods into scope
/// with `use crate::hal::Fpu`.
pub fn fpu() -> &'static imp::FpuDevice {
    imp::fpu()
}

/// Arch-neutral page mapping attributes. Each arch's `Mmu` translates these to
/// its own page-table bits (x86_64: WRITABLE→bit1, USER→bit2, DEVICE→PCD|PWT;
/// AArch64: AP/UXN/the device memory-attribute index). `PRESENT`/valid is
/// implied by the act of mapping, so it isn't a flag here.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MapFlags(u8);

impl MapFlags {
    /// The page is writable.
    pub const WRITABLE: MapFlags = MapFlags(1 << 0);
    /// The page is user-accessible (ring 3).
    pub const USER: MapFlags = MapFlags(1 << 1);
    /// Device / MMIO memory: uncached (x86_64: PCD|PWT). Use for memory-mapped
    /// registers, which must not be cached.
    pub const DEVICE: MapFlags = MapFlags(1 << 2);

    /// Whether all bits of `other` are set in `self`.
    pub const fn contains(self, other: MapFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union of two flag sets — `const` for use in `const` MMIO attributes
    /// (where the `|` operator's trait method isn't callable).
    pub const fn union(self, other: MapFlags) -> MapFlags {
        MapFlags(self.0 | other.0)
    }
}

impl core::ops::BitOr for MapFlags {
    type Output = MapFlags;
    fn bitor(self, rhs: MapFlags) -> MapFlags {
        self.union(rhs)
    }
}

impl core::ops::BitOrAssign for MapFlags {
    fn bitor_assign(&mut self, rhs: MapFlags) {
        self.0 |= rhs.0;
    }
}

/// The memory-management unit: virtual→physical mapping, address-space
/// lifecycle, and TLB maintenance.
///
/// x86_64 implements this over 4-level PML4 page tables + CR3 + `invlpg`; a
/// future AArch64 port over its translation tables + TTBR0/1 + `tlbi`. An
/// address space is identified by an opaque `u64` handle (x86_64: the PML4
/// physical address; AArch64: a TTBR value). The page-table *format* and bit
/// layout are the arch impl's concern; callers pass arch-neutral [`MapFlags`].
pub trait Mmu {
    /// Physical handle of the active address space (its root table).
    fn current_address_space(&self) -> u64;

    /// Map `virt` → `phys` with `flags` in the address space `space`. The
    /// present/valid bit is added automatically. 4 KiB pages only.
    ///
    /// # Safety
    /// Mutates the given address space; `space` and `phys` must be valid frames.
    unsafe fn map_in(&self, space: u64, virt: u64, phys: u64, flags: MapFlags);

    /// Map `virt` → `phys` with `flags` in the active address space.
    ///
    /// # Safety
    /// As [`Mmu::map_in`], on the active space.
    unsafe fn map(&self, virt: u64, phys: u64, flags: MapFlags);

    /// Remove the mapping for `virt` in the active space (flushes its TLB entry).
    ///
    /// # Safety
    /// Changes the active address space.
    unsafe fn unmap(&self, virt: u64);

    /// Translate `virt` to its physical address in the active space, or `None`.
    fn translate(&self, virt: u64) -> Option<u64>;

    /// Build a fresh address space: empty user half, kernel half shared with
    /// the current space. Returns its handle.
    ///
    /// # Safety
    /// Allocates a frame; safe to switch to while the shared kernel half is
    /// valid.
    unsafe fn new_address_space(&self) -> u64;

    /// Build a child address space that eager-copies `parent`'s user space
    /// (kernel half shared). Returns the child handle.
    ///
    /// # Safety
    /// `parent` must be the active space. Allocates frames.
    unsafe fn fork_address_space(&self, parent: u64) -> u64;

    /// Free an address space's user half (leaf frames + user page tables + the
    /// root). The shared kernel half is left intact.
    ///
    /// # Safety
    /// `space` must not be the active address space, and must have a private
    /// user half (from `new_address_space`/`fork_address_space`).
    unsafe fn free_address_space(&self, space: u64);

    /// Switch the active address space (loads its root table; flushes the TLB).
    ///
    /// # Safety
    /// `space` must map the currently-executing code, the stack, and the HHDM.
    unsafe fn switch_address_space(&self, space: u64);
}

/// The MMU for this build's target architecture (build-time selected, concrete
/// type — no vtable). Callers bring the methods into scope with
/// `use crate::hal::Mmu`.
pub fn mmu() -> &'static imp::MmuDevice {
    imp::mmu()
}

/// The per-core base register: the standard "find this core's state in one
/// access" mechanism. x86_64 points the GS base at this core's per-CPU block
/// (IA32_GS_BASE MSR); a future AArch64 port uses TPIDR_EL1. The per-CPU data
/// blocks themselves are arch-agnostic and live in `percpu.rs`, which sits on
/// this trait.
///
/// (The trait is named `PerCpu` but the per-core *data* struct is also called
/// `PerCpu` in `percpu.rs`; callers import this trait anonymously —
/// `use crate::hal::PerCpu as _;` — to bring its methods into scope without the
/// name clashing.)
pub trait PerCpu {
    /// Point this core's per-CPU base register at `base`, which must address a
    /// per-CPU block whose first `u32` field is the core index.
    ///
    /// # Safety
    /// `base` must remain valid for the lifetime of this core.
    unsafe fn set_base(&self, base: u64);

    /// This core's index — the first `u32` of the per-CPU block, read through
    /// the base register. Valid only after [`PerCpu::set_base`] on this core.
    fn this_cpu_index(&self) -> u32;
}

/// The per-core base register for this build's target architecture (build-time
/// selected, concrete type — no vtable).
pub fn per_cpu() -> &'static imp::PerCpuDevice {
    imp::per_cpu()
}
