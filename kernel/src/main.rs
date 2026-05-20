// kernel/src/main.rs
//
// Frame OS — bare-metal kernel entry point (B0 Step 1).
//
// At Step 1 this is pure native Rust. The kernel:
//   1. Takes Limine's handoff (Limine has set up long mode, paging,
//      a stack, and copied the kernel ELF to its load address).
//   2. Writes a banner to the serial console (COM1, port 0x3F8).
//   3. Halts via `hlt` loop.
//
// No Frame systems yet — B0 Step 2 adds the Kernel HSM. Step 3 replaces
// the inline serial write with the SerialDriver FSM.
//
// Limine boot protocol notes:
//   - We declare `BASE_REVISION` to tell Limine which protocol version
//     we speak. Limine checks this on its first pass over the kernel
//     ELF and refuses to boot if the version doesn't match.
//   - The `#[link_section]` attributes put the boot-info structs in
//     special ELF sections that Limine scans. Without them Limine
//     can't find our protocol declarations and will boot "blind."
//   - `_start` is the kernel entry; Limine calls it after setting up
//     long mode + paging. The function must never return — `loop { hlt }`
//     is the standard "we're done; CPU rests" pattern.
//
// Serial output:
//   - Port 0x3F8 is the standard x86 COM1 port. QEMU exposes it via
//     `-serial stdio` so the kernel's writes appear in the host's
//     terminal where we ran QEMU.
//   - We write the banner byte-by-byte using `out dx, al`. No baud rate
//     setup needed for QEMU; real hardware would need full UART init
//     (B0 Step 3 with the SerialDriver FSM will add that).

#![no_std]
#![no_main]

use core::panic::PanicInfo;

// ---------------------------------------------------------------------------
// Limine boot protocol declarations
// ---------------------------------------------------------------------------

// Base revision: tells Limine which version of its boot protocol we
// support. Revision 3 is the current protocol as of Limine v9.
#[used]
#[link_section = ".requests"]
static BASE_REVISION: limine::BaseRevision = limine::BaseRevision::with_revision(3);

// Markers that delimit the .requests section. Limine looks between these
// to find our protocol-info structs. Placing them in dedicated sections
// keeps the linker from reordering or eliminating them.
#[used]
#[link_section = ".requests_start_marker"]
static REQUESTS_START_MARKER: limine::request::RequestsStartMarker =
    limine::request::RequestsStartMarker::new();

#[used]
#[link_section = ".requests_end_marker"]
static REQUESTS_END_MARKER: limine::request::RequestsEndMarker =
    limine::request::RequestsEndMarker::new();

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Kernel entry. Called by Limine after long mode is set up.
///
/// # Safety
///
/// Called once at kernel startup; never re-entered. The boot environment
/// (page tables, stack, GDT) is set up by Limine before this runs.
#[no_mangle]
unsafe extern "C" fn kmain() -> ! {
    // Verify Limine actually understood our base revision.
    if !BASE_REVISION.is_supported() {
        halt_forever();
    }

    write_serial_str(b"Frame OS kernel \xe2\x80\x94 B0 Step 1\n");
    write_serial_str(b"hello from bare metal\n");
    write_serial_str(b"halting...\n");

    halt_forever();
}

// ---------------------------------------------------------------------------
// Serial output (COM1, 16550 UART at port 0x3F8)
//
// At Step 1 we don't initialize the UART — QEMU exposes a pre-configured
// serial port on COM1 and accepts byte writes without any setup. Real
// hardware would need baud rate, FIFO, line control setup, which Step 3's
// SerialDriver FSM will handle.
// ---------------------------------------------------------------------------

const COM1_DATA: u16 = 0x3F8;

unsafe fn write_serial_byte(b: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") COM1_DATA,
        in("al") b,
        options(nomem, nostack, preserves_flags),
    );
}

unsafe fn write_serial_str(s: &[u8]) {
    for &b in s {
        write_serial_byte(b);
    }
}

// ---------------------------------------------------------------------------
// Halt loop
// ---------------------------------------------------------------------------

fn halt_forever() -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

// ---------------------------------------------------------------------------
// Panic handler
//
// On panic: write the message to serial then halt. The unsafe is for the
// serial port writes; nothing else here is allocator- or FS-dependent so
// it's safe to call from a panic context.
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    unsafe {
        write_serial_str(b"\nKERNEL PANIC: ");
        // PanicInfo's Display would format args + location; we can't use
        // format_args! without an allocator. Just emit the location and
        // a fixed message; full panic info reporting lands at B0 Step 2
        // when the Kernel HSM has its panic handler infrastructure.
        if let Some(loc) = info.location() {
            write_serial_str(loc.file().as_bytes());
            write_serial_byte(b':');
            // Decimal line number — write digits manually since we can't
            // use itoa or alloc.
            write_decimal_u32(loc.line());
        }
        write_serial_str(b"\nhalted.\n");
    }
    halt_forever();
}

unsafe fn write_decimal_u32(mut n: u32) {
    if n == 0 {
        write_serial_byte(b'0');
        return;
    }
    // Build digits in reverse order onto a small stack buffer.
    let mut buf = [0u8; 10];
    let mut i = 0;
    while n > 0 {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    // Emit in correct order.
    while i > 0 {
        i -= 1;
        write_serial_byte(buf[i]);
    }
}
