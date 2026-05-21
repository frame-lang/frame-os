// kernel/src/io.rs
//
// x86 port I/O primitives shared by the PIC/PIT drivers, PCI config space
// (32-bit on 0xCF8/0xCFC), and the legacy virtio-blk I/O BAR (byte/word/dword).
// Safe wrappers: a port access has no memory-safety consequence, so the unsafe
// `in`/`out` instructions are confined here behind a safe API. (serial.rs
// predates this and keeps its own copy scoped to COM1.)

use core::arch::asm;

/// Write a byte to an x86 I/O port.
pub fn outb(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

/// Read a byte from an x86 I/O port.
pub fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

/// Write a word (16-bit) to an x86 I/O port.
pub fn outw(port: u16, val: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack, preserves_flags));
    }
}

/// Read a word (16-bit) from an x86 I/O port.
pub fn inw(port: u16) -> u16 {
    let val: u16;
    unsafe {
        asm!("in ax, dx", out("ax") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

/// Write a dword (32-bit) to an x86 I/O port.
pub fn outl(port: u16, val: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags));
    }
}

/// Read a dword (32-bit) from an x86 I/O port.
pub fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe {
        asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}
