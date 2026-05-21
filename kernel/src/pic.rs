// kernel/src/pic.rs
//
// 8259A Programmable Interrupt Controller (B1 Step 3).
//
// The legacy PIC powers up mapping IRQs 0..15 onto interrupt vectors that
// collide with the CPU's exception vectors (0..31). We remap the master to
// 0x20 (32) and the slave to 0x28 (40) so IRQ0 (the PIT timer) arrives on
// vector 32, clear of the exceptions. Then we mask everything except IRQ0.
//
// We use the legacy 8259 + PIT path (not the APIC) deliberately — it's the
// classic minimal route to "a periodic interrupt fires," per the B1 plan.

use crate::io::{inb, outb};

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const PIC_EOI: u8 = 0x20;

/// Master PIC vector offset. IRQ0 (PIT) → vector 32.
pub const PIC1_OFFSET: u8 = 0x20;

/// Remap the PICs to vectors 0x20/0x28 and mask all IRQs except IRQ0.
pub fn remap() {
    // ICW1: begin init, expect ICW4.
    outb(PIC1_CMD, 0x11);
    outb(PIC2_CMD, 0x11);
    // ICW2: vector offsets — master → 0x20, slave → 0x28.
    outb(PIC1_DATA, PIC1_OFFSET);
    outb(PIC2_DATA, 0x28);
    // ICW3: wiring (slave is on master IRQ2).
    outb(PIC1_DATA, 0x04);
    outb(PIC2_DATA, 0x02);
    // ICW4: 8086/88 mode.
    outb(PIC1_DATA, 0x01);
    outb(PIC2_DATA, 0x01);
    // Masks: unmask only IRQ0 on the master; mask all of the slave.
    outb(PIC1_DATA, 0xFE); // 1111_1110 → IRQ0 enabled
    outb(PIC2_DATA, 0xFF);
}

/// Send end-of-interrupt for a master-PIC IRQ (IRQ0..7), e.g. the timer.
pub fn eoi_master() {
    outb(PIC1_CMD, PIC_EOI);
}

/// Unmask a slave-PIC IRQ (IRQ8..15), e.g. virtio-blk on IRQ11 (B4). Clears
/// the IRQ's mask bit on the slave and also unmasks the master's cascade line
/// (IRQ2), through which the slave is wired.
pub fn unmask_slave_irq(irq: u8) {
    let bit = irq - 8;
    let slave = inb(PIC2_DATA) & !(1 << bit);
    outb(PIC2_DATA, slave);
    let master = inb(PIC1_DATA) & !(1 << 2); // cascade
    outb(PIC1_DATA, master);
}

/// Send end-of-interrupt for a slave-PIC IRQ (IRQ8..15): the slave first, then
/// the master (the cascade line must also be acknowledged).
pub fn eoi_slave() {
    outb(PIC2_CMD, PIC_EOI);
    outb(PIC1_CMD, PIC_EOI);
}
