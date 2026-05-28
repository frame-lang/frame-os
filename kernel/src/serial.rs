// kernel/src/serial.rs
//
// The arch-agnostic console *text* layer — it sits on the `hal::Console` seam
// (B-HAL.1). The byte-level mechanism (port I/O, the 16550 init sequence, THRE
// polling) now lives behind the HAL in `arch/<isa>/serial.rs`; this module is
// the shared layer on top of it: thin forwarders to `hal::console()` plus the
// formatting helpers (write_str / writeln / write_hex_u64 / write_u32_decimal)
// that every arch reuses unchanged.
//
// Keeping the public `serial::*` API here (rather than pushing every caller to
// `hal::console()`) is deliberate: hundreds of call sites across the kernel
// print through these functions, and the helpers are genuinely arch-neutral —
// they belong in a shared layer, not duplicated per arch or forced through a
// trait. The seam is `hal::Console`; this is the convenience layer over it.
//
// Two consumers use this module directly with raw writes, bypassing the
// SerialDriver state machine, and that's deliberate:
//   - Early boot ($InitMemory..$InitConsole in the Kernel HSM) prints before
//     the console driver is up — the "bootstrap console" (cf. Linux
//     earlyprintk).
//   - The panic handler prints from an emergency context where the driver may
//     be in any state; it must not depend on driver liveness. `hal::console()`
//     returns a `&'static` zero-sized handle that is always valid, so these
//     emergency paths never depend on any initialization having run.

use crate::hal::{self, Console as _};

/// Program the UART (delegates to the active arch console). Called by
/// SerialDriver's `init()` action.
pub fn init_uart() {
    hal::console().init();
}

/// Write a single byte to the console, polling for transmit-ready first.
pub fn write_byte(b: u8) {
    hal::console().write_byte(b);
}

/// Read one received byte if the console has data waiting (polled RX), else
/// `None`. The RX interrupt handler drains the FIFO by calling this in a loop.
pub fn rx_byte() -> Option<u8> {
    hal::console().rx_byte()
}

/// Enable the console's received-data-available interrupt and route its IRQ
/// line to the interrupt controller (B8). Call after the IDT + controller are
/// up — this is what makes the console interactive.
#[cfg(feature = "interactive")]
pub fn enable_rx_interrupt() {
    hal::console().enable_rx_interrupt();
}

/// Write a string to the console, byte by byte (UTF-8 bytes).
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

/// Write a u64 as 16 hex digits (no `0x` prefix). Alloc-free; used by the
/// page-fault handler to report a faulting address.
pub fn write_hex_u64(n: u64) {
    let digits = b"0123456789abcdef";
    let mut shift = 60i32;
    while shift >= 0 {
        let nibble = ((n >> shift) & 0xF) as usize;
        write_byte(digits[nibble]);
        shift -= 4;
    }
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
