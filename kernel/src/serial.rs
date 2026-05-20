// kernel/src/serial.rs
//
// COM1 (16550 UART at port 0x3F8) serial output.
//
// At B0 we don't initialize the UART — QEMU exposes a pre-configured
// serial port on COM1 and accepts byte writes without any setup. Real
// hardware needs baud rate / FIFO / line-control setup, which B0 Step 3's
// `SerialDriver` Frame system will handle (replacing the direct port-IO
// here with a state machine that owns the UART).
//
// The functions expose a *safe* API even though they perform port I/O
// (which is `unsafe` at the instruction level). Writing the COM1 data
// register is sound — it has no memory-safety consequence — so wrapping
// the `out` in a safe function is the right boundary. This matters
// because the Frame-generated `Kernel` code calls `serial::writeln(...)`
// from safe context.

const COM1_DATA: u16 = 0x3F8;

/// Write a single byte to COM1.
pub fn write_byte(b: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") COM1_DATA,
            in("al") b,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Write a string to COM1, byte by byte. The string is emitted as its
/// UTF-8 bytes; a serial terminal renders them as text.
pub fn write_str(s: &str) {
    for b in s.bytes() {
        write_byte(b);
    }
}

/// Write a string followed by a newline.
pub fn writeln(s: &str) {
    write_str(s);
    write_byte(b'\n');
}

/// Write a u32 in decimal. Used by the panic handler, which can't rely on
/// `format!`/`alloc` being usable in every panic context.
pub fn write_u32_decimal(mut n: u32) {
    if n == 0 {
        write_byte(b'0');
        return;
    }
    // Build digits in reverse onto a small stack buffer (u32 max is 10
    // decimal digits), then emit in order.
    let mut buf = [0u8; 10];
    let mut i = 0;
    while n > 0 {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        write_byte(buf[i]);
    }
}
