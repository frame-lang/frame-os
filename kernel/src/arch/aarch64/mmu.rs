// kernel/src/arch/aarch64/mmu.rs
//
// Minimal AArch64 MMU bring-up (B-HAL.3.4). The skeleton's job here is the
// *mechanism*: stand up translation tables, program MAIR/TCR/TTBR0, and flip
// SCTLR_EL1.M so the kernel runs translated. The full `hal::Mmu` trait impl
// (map/unmap, address-space lifecycle) needs the frame allocator + a process
// model, which arrive on AArch64 in B-HAL.4/.5; this is the substrate they sit
// on.
//
// We build an identity map (VA == PA) with a single L1 table of 1 GiB block
// descriptors, using a 39-bit VA (T0SZ=25, 4 KiB granule → the initial lookup
// is at L1):
//   L1[0]  [0x0000_0000, 0x4000_0000)  Device-nGnRnE  (flash, GIC, PL011 @0x0900_0000)
//   L1[1]  [0x4000_0000, 0x8000_0000)  Normal WB cacheable  (RAM: kernel, stack, DTB)
// Because the map is identity, the PC, SP, PL011 MMIO, and the DTB all keep the
// same addresses across the M=0→1 transition — so the console must keep working
// immediately after enable, which is exactly what the smoke check asserts.

use core::arch::asm;

/// One translation table: 512 × 8-byte descriptors, 4 KiB, 4 KiB-aligned.
#[repr(C, align(4096))]
struct Table([u64; 512]);

static mut L1: Table = Table([0; 512]);

// Block/page descriptor attribute bits (L1/L2 block).
const VALID_BLOCK: u64 = 0b01; // bits[1:0] = 0b01 → valid block descriptor
const ATTRIDX_NORMAL: u64 = 1 << 2; // AttrIndx = MAIR attr1 (Normal WB); attr0 (Device) = 0
/// AP[2:1] permission bits. AP[1] = 1 (bit 6) → EL0 access allowed (R/W); AP[2]
/// stays 0 → read+write (not read-only). Set on the *EL0 alias* block for the
/// B-HAL.5.0 user-mode demo (see below). Setting AP[1]=1 on the *kernel* block
/// would make EL1 instruction fetch fault with permission abort at level 1 on
/// QEMU's cortex-a72 (confirmed empirically: ESR EC=0x21 IFSC=0x0d), so the
/// design uses a *separate* L1 entry aliased to the same RAM PA — EL0 enters
/// through the alias VA, EL1 keeps using the kernel block. (A production design
/// would do per-page L3 table walks with proper AP per page; the alias is the
/// minimum that exercises EL0 + SVC without that scaffolding.)
const AP_EL0: u64 = 1 << 6;
const SH_INNER: u64 = 0b11 << 8; // inner shareable (for Normal memory)
const AF: u64 = 1 << 10; // Access Flag (else first access faults)

/// VA offset for the EL0 alias of RAM: the user-mode demo runs at
/// `kernel_va + EL0_ALIAS_OFFSET`. The alias is L1[2] = [2 GiB, 3 GiB) mapped
/// to the same PA range [1 GiB, 2 GiB) as the kernel block at L1[1].
pub const EL0_ALIAS_OFFSET: u64 = 0x4000_0000;

// MAIR_EL1 attributes: attr0 = Device-nGnRnE (0x00, implicit), attr1 = Normal WB
// RW-allocate (0xFF) in byte[1].
const MAIR: u64 = 0xFF << 8;

/// Build the identity map, program the system registers, and enable the MMU.
///
/// # Safety
/// Call once, on the boot CPU at EL1, before relying on translated execution.
/// Establishes an identity map covering the running code, stack, PL011, and DTB.
pub unsafe fn enable() {
    let l1 = (&raw mut L1) as *mut u64;
    // [0, 1 GiB): device memory — covers the PL011 and the GIC.
    l1.add(0).write(AF | VALID_BLOCK);
    // [1 GiB, 2 GiB): normal cacheable RAM (output address 0x4000_0000). The
    // kernel runs here, AP unchanged from B-HAL.3.4 (EL1 R/W, no EL0 access).
    l1.add(1)
        .write(0x4000_0000 | ATTRIDX_NORMAL | SH_INNER | AF | VALID_BLOCK);
    // [2 GiB, 3 GiB): the EL0 alias of the same RAM PA (B-HAL.5.0). AP=01
    // permits EL0 R/W and exec; UXN/PXN stay 0. EL0 enters through this VA
    // for the user-mode demo, EL1 still uses L1[1] for its own fetches.
    l1.add(2)
        .write(0x4000_0000 | ATTRIDX_NORMAL | SH_INNER | AF | AP_EL0 | VALID_BLOCK);

    // TCR_EL1: T0SZ=25 (39-bit VA) | IRGN0/ORGN0 = WB-WA cacheable walks |
    // SH0 = inner shareable | TG0 = 4 KiB (bits[15:14]=0) | EPD1 = disable the
    // TTBR1 (high-half) walk | IPS = 40-bit PA.
    let tcr: u64 = 25 | (1 << 8) | (1 << 10) | (0b11 << 12) | (1 << 23) | (0b010 << 32);

    asm!(
        "msr mair_el1, {mair}",
        "msr tcr_el1, {tcr}",
        "msr ttbr0_el1, {ttbr}",
        "dsb ish",
        "tlbi vmalle1",
        "dsb ish",
        "isb",
        mair = in(reg) MAIR,
        tcr = in(reg) tcr,
        ttbr = in(reg) l1 as u64,
        options(nostack),
    );

    // Enable the MMU (SCTLR_EL1.M = 1). Caches are left as-is for the skeleton;
    // because the map is identity, the next instruction fetch + the stack stay
    // valid across the transition.
    let mut sctlr: u64;
    asm!("mrs {0}, sctlr_el1", out(reg) sctlr, options(nostack));
    sctlr |= 1; // M
    asm!("msr sctlr_el1, {0}", "isb", in(reg) sctlr, options(nostack));
}
