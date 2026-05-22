// kernel/src/lapic.rs
//
// Local APIC (B7) — per-core interrupt controller + timer. The legacy 8259
// PIC/PIT only interrupt the BSP; for the *application processors* to be
// preempted (so each core runs a real, time-sliced thread) each core programs
// its own LAPIC timer. The LAPIC registers live in an MMIO page at a fixed
// physical address (0xFEE00000) that is **per-core**: every core accessing that
// address hits *its own* LAPIC, so one mapping + the same code runs on every
// core and is core-local.
//
// xAPIC (MMIO) mode — we do not request Limine's X2APIC. The single register
// page is mapped uncached (like the xHCI BAR), in the kernel address space the
// APs share.

use crate::{frames, paging};
use core::ptr::{read_volatile, write_volatile};

const LAPIC_PHYS: u64 = 0xFEE0_0000;

// Register offsets (bytes from the LAPIC base).
const SVR: usize = 0xF0; // Spurious Interrupt Vector Register
const EOI: usize = 0xB0; // End Of Interrupt
const LVT_TIMER: usize = 0x320; // LVT Timer entry
const TIMER_INITIAL: usize = 0x380; // Initial Count
const TIMER_DIVIDE: usize = 0x3E0; // Divide Configuration

const SVR_ENABLE: u32 = 1 << 8; // APIC software enable
const SPURIOUS_VECTOR: u32 = 0xFF; // vector for spurious interrupts
const TIMER_PERIODIC: u32 = 1 << 17; // LVT timer mode = periodic
const DIVIDE_BY_16: u32 = 0x3; // Divide Config encoding for ÷16

/// The IDT vector the LAPIC timer fires on each core (see `interrupts.rs`).
pub const TIMER_VECTOR: u32 = 0x40;

/// Initial count for the periodic timer (÷16). Sized so the timer fires every
/// few-to-tens of milliseconds under QEMU — frequent enough to preempt an AP's
/// busy loop many times in a short window, without flooding.
const TIMER_INITIAL_COUNT: u32 = 1_000_000;

static mut LAPIC_BASE: u64 = 0; // mapped virtual address (0 until `map`)

/// MMIO mapping flags: writable + cache-disable (PCD) + write-through (PWT).
const MMIO_FLAGS: u64 = paging::WRITABLE | (1 << 4) | (1 << 3);

/// Map the LAPIC register page into the (shared) kernel address space. Called
/// once by the BSP before the APs touch their LAPICs.
pub fn map() {
    let va = frames::phys_to_virt(LAPIC_PHYS) as u64;
    unsafe {
        paging::map(va, LAPIC_PHYS, MMIO_FLAGS);
        (&raw mut LAPIC_BASE).write(va);
    }
}

fn reg(off: usize) -> *mut u32 {
    let base = unsafe { (&raw const LAPIC_BASE).read() };
    (base as usize + off) as *mut u32
}
fn read(off: usize) -> u32 {
    unsafe { read_volatile(reg(off)) }
}
fn write(off: usize, val: u32) {
    unsafe { write_volatile(reg(off), val) }
}

/// Enable this core's LAPIC and start its periodic timer on `TIMER_VECTOR`.
/// Run by each core (AP) after it has loaded the IDT.
pub fn init_this_cpu() {
    // Software-enable the LAPIC + set the spurious vector.
    write(SVR, read(SVR) | SVR_ENABLE | SPURIOUS_VECTOR);
    // Periodic timer: divide, mode, vector, then arm with the initial count.
    write(TIMER_DIVIDE, DIVIDE_BY_16);
    write(LVT_TIMER, TIMER_VECTOR | TIMER_PERIODIC);
    write(TIMER_INITIAL, TIMER_INITIAL_COUNT);
}

/// Signal End Of Interrupt to the LAPIC (the timer ISR calls this).
pub fn eoi() {
    write(EOI, 0);
}

// Interrupt Command Register (the IPI send registers).
const ICR_LOW: usize = 0x300;
const ICR_HIGH: usize = 0x310;
const ICR_ASSERT: u32 = 1 << 14; // level = assert
const ICR_ALL_BUT_SELF: u32 = 0b11 << 18; // destination shorthand

/// Send a fixed inter-processor interrupt on `vector` to **all cores except this
/// one** (the "all excluding self" destination shorthand). Used for TLB
/// shootdown (B7 Step 5): the initiator IPIs the other cores to flush a stale
/// translation. The high (destination) word is ignored for the shorthand; we
/// write it first, then the low word, which triggers the send.
pub fn send_ipi_all_but_self(vector: u32) {
    write(ICR_HIGH, 0);
    write(ICR_LOW, vector | ICR_ASSERT | ICR_ALL_BUT_SELF);
}
