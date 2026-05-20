// kernel/src/io.rs
//
// x86 port I/O primitive shared by the PIC and PIT drivers. Safe wrapper: a
// port write has no memory-safety consequence, so the unsafe `out`
// instruction is confined here behind a safe API. (serial.rs predates this
// and keeps its own copy scoped to COM1.) `inb` will be added when a driver
// first needs to read a port.

use core::arch::asm;

/// Write a byte to an x86 I/O port.
pub fn outb(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}
