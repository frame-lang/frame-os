// kernel/src/arch/aarch64/serial.rs
//
// The AArch64 implementation of `hal::Console`: the PL011 UART (B-HAL.3.2).
//
// On QEMU's `virt` machine the primary PL011 lives at MMIO 0x0900_0000. This is
// the ARM counterpart of x86's 16550 (arch/x86_64/serial.rs): the arch-agnostic
// `serial.rs` text layer (write_str / writeln / …) forwards here through
// `hal::console()`, so the *same* banner prints on both ISAs with only this one
// `write_byte` of new mechanism.
//
// QEMU's PL011 is usable for output without programming the baud divisor, so
// `init` is a no-op here; `write_byte` polls the TX-FIFO-full flag, `rx_byte`
// the RX-FIFO-empty flag.

use crate::hal::Console;
use core::ptr::{read_volatile, write_volatile};

/// PL011 base on the QEMU `virt` machine.
const UART0: usize = 0x0900_0000;
const UARTDR: usize = 0x00; // data register
const UARTFR: usize = 0x18; // flag register
const UARTCR: usize = 0x30; // control register
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full
const FR_RXFE: u32 = 1 << 4; // receive FIFO empty
const CR_UARTEN: u32 = 1 << 0; // UART enable
const CR_TXE: u32 = 1 << 8; // transmit enable
const CR_RXE: u32 = 1 << 9; // receive enable

fn reg(off: usize) -> *mut u32 {
    (UART0 + off) as *mut u32
}

/// The QEMU `virt` PL011 UART. A zero-sized handle — the HAL's `Console` device.
pub struct Pl011;

static PL011: Pl011 = Pl011;

/// The AArch64 console device (the `virt` PL011).
pub fn console() -> &'static Pl011 {
    &PL011
}

impl Console for Pl011 {
    fn init(&self) {
        // Enable the UART with TX + RX. (QEMU ignores the baud divisor; real
        // PL011 bring-up would also program IBRD/FBRD/LCRH here.)
        unsafe {
            write_volatile(reg(UARTCR), CR_UARTEN | CR_TXE | CR_RXE);
        }
    }

    fn write_byte(&self, b: u8) {
        unsafe {
            while read_volatile(reg(UARTFR)) & FR_TXFF != 0 {
                core::hint::spin_loop();
            }
            write_volatile(reg(UARTDR), b as u32);
        }
    }

    fn rx_byte(&self) -> Option<u8> {
        unsafe {
            if read_volatile(reg(UARTFR)) & FR_RXFE != 0 {
                None
            } else {
                Some(read_volatile(reg(UARTDR)) as u8)
            }
        }
    }
}
