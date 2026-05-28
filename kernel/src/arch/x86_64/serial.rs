// kernel/src/arch/x86_64/serial.rs
//
// The x86_64 implementation of `hal::Console`: COM1 (16550 UART at port
// 0x3F8) port mechanics — raw port I/O, the 16550 init sequence, THRE-polled
// byte transmission, polled RX (B-HAL.1).
//
// This is the *mechanism*, relocated behind the HAL seam. The lifecycle on top
// of it — "you must init before writing" — is modeled by the SerialDriver
// Frame system (frame/serial_driver.frs), whose actions call the arch-agnostic
// `serial.rs` facade, which forwards here through `hal::console()`. The
// arch-agnostic text layer (write_str / writeln / write_hex_u64 /
// write_u32_decimal) also lives in `serial.rs` and is shared by every arch.
//
// Two consumers reach this directly (via the facade) with raw writes, bypassing
// the SerialDriver state machine, and that's deliberate:
//   - Early boot ($InitMemory..$InitConsole in the Kernel HSM) prints before
//     the console driver is up — the "bootstrap console" (cf. Linux
//     earlyprintk).
//   - The panic handler prints from an emergency context where the driver may
//     be in any state; it must not depend on driver liveness.
//
// COM1 register access performs port I/O (unsafe at the instruction level) but
// is sound — no memory-safety consequence — so the trait methods expose a
// *safe* boundary, letting the Frame-generated SerialDriver actions call into
// it from safe context.

use crate::hal::Console;

// COM1 register offsets from the base port.
const COM1_BASE: u16 = 0x3F8;
const COM1_DATA: u16 = COM1_BASE; // DLAB=0: RX/TX buffer; DLAB=1: divisor low
const COM1_INT_ENABLE: u16 = COM1_BASE + 1; // DLAB=0: interrupt enable; DLAB=1: divisor high
const COM1_FIFO_CTRL: u16 = COM1_BASE + 2; // FIFO control
const COM1_LINE_CTRL: u16 = COM1_BASE + 3; // line control (incl. DLAB bit)
const COM1_MODEM_CTRL: u16 = COM1_BASE + 4; // modem control
const COM1_LINE_STATUS: u16 = COM1_BASE + 5; // line status (THRE etc.)

/// THRE (Transmitter Holding Register Empty) bit in the Line Status
/// Register. Set when the UART can accept another byte to transmit.
const LSR_THRE: u8 = 0x20;
/// Data Ready bit in the Line Status Register: a received byte is waiting (B8).
const LSR_DATA_READY: u8 = 0x01;
/// Interrupt Enable Register: received-data-available interrupt (B8).
#[cfg(feature = "interactive")]
const IER_RX_AVAIL: u8 = 0x01;
/// Modem Control Register OUT2: gates the UART's IRQ line to the PIC (B8).
#[cfg(feature = "interactive")]
const MCR_OUT2: u8 = 0x08;

/// Write a byte to an x86 I/O port.
fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Read a byte from an x86 I/O port.
fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") val,
            in("dx") port,
            options(nomem, nostack, preserves_flags),
        );
    }
    val
}

/// The COM1 16550 UART. A zero-sized handle over fixed I/O ports — the HAL's
/// x86_64 `Console` device.
pub struct Uart;

static UART: Uart = Uart;

/// The x86_64 console device (the one COM1 UART in this binary).
pub fn console() -> &'static Uart {
    &UART
}

impl Console for Uart {
    /// Program the 16550 UART for polled, interrupt-free operation: 115200
    /// baud, 8 data bits, no parity, 1 stop bit, FIFOs enabled. Interrupts
    /// stay disabled (no OUT2) because B0 has no IDT wired for serial — this
    /// is polled-mode TX. Correct for real 16550 hardware; QEMU accepts the
    /// sequence and is lenient about it.
    ///
    /// Called by SerialDriver's `init()` action (its $Uninitialized → $Ready
    /// transition). Idempotent in practice, but the SerialDriver state machine
    /// is what guarantees it runs exactly once before any driver write.
    fn init(&self) {
        outb(COM1_INT_ENABLE, 0x00); // disable all UART interrupts
        outb(COM1_LINE_CTRL, 0x80); // DLAB on: next two writes set the divisor
        outb(COM1_DATA, 0x01); // divisor low  = 1 -> 115200 baud
        outb(COM1_INT_ENABLE, 0x00); // divisor high = 0
        outb(COM1_LINE_CTRL, 0x03); // DLAB off: 8 bits, no parity, 1 stop
        outb(COM1_FIFO_CTRL, 0xC7); // enable + clear FIFOs, 14-byte trigger
        outb(COM1_MODEM_CTRL, 0x03); // DTR + RTS asserted, OUT2 clear (no IRQ)
    }

    /// Write a single byte to COM1, waiting for the transmit holding register
    /// to be empty first (polled TX). On QEMU the THRE bit is essentially
    /// always set, so the wait is a no-op; on real hardware it paces writes
    /// to the UART's transmit rate.
    fn write_byte(&self, b: u8) {
        while inb(COM1_LINE_STATUS) & LSR_THRE == 0 {
            core::hint::spin_loop();
        }
        outb(COM1_DATA, b);
    }

    /// Read one received byte if the UART has data waiting (polled RX), else
    /// `None` (B8). The RX interrupt handler drains the FIFO by calling this
    /// in a loop.
    fn rx_byte(&self) -> Option<u8> {
        if inb(COM1_LINE_STATUS) & LSR_DATA_READY != 0 {
            Some(inb(COM1_DATA))
        } else {
            None
        }
    }

    /// Enable the COM1 received-data-available interrupt (delivered as IRQ4)
    /// and route the UART's IRQ line to the PIC by asserting OUT2 (B8). TX
    /// stays polled. Call after the IDT + PIC are up — this is what makes the
    /// console interactive.
    #[cfg(feature = "interactive")]
    fn enable_rx_interrupt(&self) {
        outb(COM1_INT_ENABLE, IER_RX_AVAIL);
        outb(COM1_MODEM_CTRL, 0x03 | MCR_OUT2); // DTR + RTS + OUT2
    }
}
