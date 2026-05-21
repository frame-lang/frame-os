// kernel-tests/src/lib.rs
//
// Host-target build of the kernel's `Kernel` Frame system, wired to a
// capturing `serial` module so behavioral tests can assert on what the
// HSM's actions print.
//
// The generated `Kernel` actions call `serial::writeln(...)` /
// `serial::write_str(...)`. The generated module is wrapped in
// `mod _kernel_framec { use super::*; ... }`, so its glob import pulls in
// whatever names are visible here — including the `serial` module below.
// On the host, `String`/`Vec`/`Box`/`ToString` come from the std prelude
// (already in scope everywhere), so unlike the no_std kernel crate we
// don't re-export them from `alloc`.

/// Capturing serial sink for tests. Mirrors the public API of the
/// kernel's real `crate::serial` (COM1 port I/O) but appends to a
/// thread-local buffer instead.
///
/// Thread-local (not a global) so tests — which libtest runs each on its
/// own thread — are isolated from each other. Each test should call
/// `serial::clear()` before constructing a `Kernel` to start from a known
/// state, then `serial::captured()` to read what the HSM printed.
pub mod serial {
    use std::cell::RefCell;

    thread_local! {
        static CAPTURED: RefCell<String> = const { RefCell::new(String::new()) };
    }

    /// Host stand-in for the 16550 init sequence. No UART on the host, so
    /// this is a no-op — SerialDriver's $Uninitialized → $Ready transition
    /// still happens; we just don't program nonexistent hardware. (If a
    /// test ever needs to assert init ran, capture a marker here.)
    pub fn init_uart() {}

    /// Append a single byte (interpreted as an ASCII/Latin-1 char). The
    /// kernel's panic handler uses this for the `:` separator and digits.
    pub fn write_byte(b: u8) {
        CAPTURED.with(|c| c.borrow_mut().push(b as char));
    }

    pub fn write_str(s: &str) {
        CAPTURED.with(|c| c.borrow_mut().push_str(s));
    }

    pub fn writeln(s: &str) {
        CAPTURED.with(|c| {
            let mut buf = c.borrow_mut();
            buf.push_str(s);
            buf.push('\n');
        });
    }

    pub fn write_u32_decimal(n: u32) {
        CAPTURED.with(|c| c.borrow_mut().push_str(&n.to_string()));
    }

    /// Append a u64 as 16 hex digits (matches the kernel's serial).
    pub fn write_hex_u64(n: u64) {
        CAPTURED.with(|c| c.borrow_mut().push_str(&format!("{n:016x}")));
    }

    /// Return a copy of everything captured on this thread so far.
    pub fn captured() -> String {
        CAPTURED.with(|c| c.borrow().clone())
    }

    /// Reset the capture buffer for this thread.
    pub fn clear() {
        CAPTURED.with(|c| c.borrow_mut().clear());
    }
}

/// Host test-double for the kernel's `vm` module. The generated
/// `PageFaultHandler` actions call `crate::vm::{is_lazy_region, lazy_map}`;
/// in the kernel those touch real page tables, here they're controllable
/// thread-locals so behavioral tests can drive each classification path.
/// Thread-local (libtest runs each test on its own thread) so concurrent
/// tests don't clobber each other's settings.
pub mod vm {
    use core::cell::Cell;

    thread_local! {
        static LAZY: Cell<bool> = const { Cell::new(false) };
        static MAP_OK: Cell<bool> = const { Cell::new(true) };
    }

    /// Set whether the next `is_lazy_region` reports the address as lazy.
    pub fn set_lazy(b: bool) {
        LAZY.with(|c| c.set(b));
    }

    /// Set whether the next `lazy_map` succeeds (false simulates OOM).
    pub fn set_map_ok(b: bool) {
        MAP_OK.with(|c| c.set(b));
    }

    pub fn is_lazy_region(_addr: u64) -> bool {
        LAZY.with(|c| c.get())
    }

    pub fn lazy_map(_addr: u64) -> bool {
        MAP_OK.with(|c| c.get())
    }
}

/// Host test-doubles for the native modules the Kernel HSM's init phases
/// call. On the host there's no hardware to program, so each is a no-op —
/// the boot chain still runs to `$Running` in the behavioral tests. Same
/// "shared `.frs`, different native actions per target" pattern as `serial`
/// and `vm`.
pub mod frames {
    pub fn init() {}
}

pub mod interrupts {
    pub fn init() {}
}

pub mod pic {
    pub fn remap() {}
}

pub mod pit {
    pub fn init(_hz: u32) {}
}

/// Host test-double for the kernel's `usermode` module. The
/// `SyscallDispatcher` actions call `crate::usermode::{is_known_syscall,
/// perform_syscall}`; here they're simple deterministic stubs (no ring-3 /
/// longjmp) so behavioral tests can drive the validate / execute / reject
/// paths. `perform_syscall` echoes `a0` so a test can assert the value
/// flowed through `$Executing`.
pub mod usermode {
    pub fn is_known_syscall(num: u64) -> bool {
        num < 2
    }

    pub fn perform_syscall(_num: u64, a0: u64, _a1: u64) -> u64 {
        a0
    }
}

/// Host test-double for the kernel's `elf` module. The `ElfLoader` actions
/// call `crate::elf::{read_header, validate_header, map_segments, build_stack,
/// cleanup, entry_va, stack_top}`. Here the *header parsing* is real (so
/// corrupt / truncated ELF tests are meaningful), but mapping is stubbed
/// (`map_segments`/`build_stack` succeed without touching paging). A test sets
/// the input with `prepare(&BYTES)`, then constructs an `ElfLoader`.
pub mod elf {
    use std::cell::Cell;

    thread_local! {
        static BYTES: Cell<&'static [u8]> = const { Cell::new(&[]) };
        static ENTRY: Cell<u64> = const { Cell::new(0) };
        static PHOFF: Cell<u64> = const { Cell::new(0) };
        static PHENTSIZE: Cell<u16> = const { Cell::new(0) };
        static PHNUM: Cell<u16> = const { Cell::new(0) };
        static STACK_TOP: Cell<u64> = const { Cell::new(0) };
    }

    fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
        let s = b.get(o..o + 2)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    fn rd_u64(b: &[u8], o: usize) -> Option<u64> {
        let s = b.get(o..o + 8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Some(u64::from_le_bytes(a))
    }

    /// Set the ELF image for the load that follows (test-only entry point).
    pub fn prepare(bytes: &'static [u8]) {
        BYTES.with(|c| c.set(bytes));
        ENTRY.with(|c| c.set(0));
        STACK_TOP.with(|c| c.set(0));
    }

    pub fn read_header() -> bool {
        BYTES.with(|c| {
            let b = c.get();
            match (rd_u64(b, 24), rd_u64(b, 32), rd_u16(b, 54), rd_u16(b, 56)) {
                (Some(entry), Some(phoff), Some(phentsize), Some(phnum)) => {
                    ENTRY.with(|e| e.set(entry));
                    PHOFF.with(|e| e.set(phoff));
                    PHENTSIZE.with(|e| e.set(phentsize));
                    PHNUM.with(|e| e.set(phnum));
                    true
                }
                _ => false,
            }
        })
    }

    pub fn validate_header() -> bool {
        BYTES.with(|c| {
            let b = c.get();
            if b.len() < 64 || &b[0..4] != b"\x7fELF" || b[4] != 2 || b[5] != 1 {
                return false;
            }
            if !matches!((rd_u16(b, 16), rd_u16(b, 18)), (Some(2), Some(0x3E))) {
                return false;
            }
            let ph_end = PHOFF.with(|e| e.get())
                + PHNUM.with(|e| e.get()) as u64 * PHENTSIZE.with(|e| e.get()) as u64;
            (ph_end as usize) <= b.len() && PHENTSIZE.with(|e| e.get()) >= 56
        })
    }

    // Mapping is stubbed on the host (no paging). Both succeed.
    pub fn map_segments() -> bool {
        true
    }

    pub fn build_stack() -> bool {
        STACK_TOP.with(|c| c.set(0x2000_0000 + 4096 - 16));
        true
    }

    pub fn entry_va() -> u64 {
        ENTRY.with(|c| c.get())
    }

    pub fn stack_top() -> u64 {
        STACK_TOP.with(|c| c.get())
    }

    pub fn cleanup() {}
}

/// Host test-double for the kernel's `net` module. The generated `ArpResolver`
/// actions call `crate::net::{arp_send_request, arp_arm_timer, arp_on_failed}`;
/// in the kernel those build/send Ethernet frames + arm the retransmit
/// deadline, here they record call counts in thread-locals so behavioral tests
/// can assert "one request + one timer armed per attempt" and "failed after the
/// retry cap." Thread-local (libtest runs each test on its own thread).
pub mod net {
    use std::cell::Cell;

    thread_local! {
        static REQUESTS: Cell<u32> = const { Cell::new(0) };
        static ARMS: Cell<u32> = const { Cell::new(0) };
        static FAILED: Cell<bool> = const { Cell::new(false) };
    }

    pub fn arp_send_request() {
        REQUESTS.with(|c| c.set(c.get() + 1));
    }
    pub fn arp_arm_timer() {
        ARMS.with(|c| c.set(c.get() + 1));
    }
    pub fn arp_on_failed() {
        FAILED.with(|c| c.set(true));
    }

    // Test inspectors.
    pub fn requests_sent() -> u32 {
        REQUESTS.with(|c| c.get())
    }
    pub fn timers_armed() -> u32 {
        ARMS.with(|c| c.get())
    }
    pub fn failed() -> bool {
        FAILED.with(|c| c.get())
    }
    pub fn reset() {
        REQUESTS.with(|c| c.set(0));
        ARMS.with(|c| c.set(0));
        FAILED.with(|c| c.set(false));
    }
}

// Pull in the framec-generated systems. Each generated file ends with
// `pub use _<name>_framec::*;`, re-exporting the system type at this crate's
// root. SerialDriver first (Kernel holds one in its domain). Task and
// Scheduler (B1) are independent — the native scheduler composes them with
// a ready-queue; the Frame systems don't reference each other.
include!(concat!(env!("OUT_DIR"), "/serial_driver.rs"));
include!(concat!(env!("OUT_DIR"), "/kernel.rs"));
include!(concat!(env!("OUT_DIR"), "/task.rs"));
include!(concat!(env!("OUT_DIR"), "/scheduler.rs"));
include!(concat!(env!("OUT_DIR"), "/page_fault_handler.rs"));
include!(concat!(env!("OUT_DIR"), "/syscall_dispatcher.rs"));
// Process before ProcessTable: ProcessTable's domain holds Vec<Process> and
// instantiates @@Process, so the Process type must be in scope first.
include!(concat!(env!("OUT_DIR"), "/process.rs"));
include!(concat!(env!("OUT_DIR"), "/process_table.rs"));
// ElfLoader (B3 Step 4): the load-phase FSM. Actions call crate::elf::* (the
// host double above does real header parsing, stubs the mapping).
include!(concat!(env!("OUT_DIR"), "/elf_loader.rs"));
// BlockRequest (B4 Step 1): I/O request lifecycle. Pure (no native deps).
include!(concat!(env!("OUT_DIR"), "/block_request.rs"));
// Mount (B4 Step 2): filesystem mount/unmount lifecycle. Pure (no native deps).
include!(concat!(env!("OUT_DIR"), "/mount.rs"));
// OpenFile (B4 Step 3): per-fd access-mode lifecycle. Pure (no native deps).
include!(concat!(env!("OUT_DIR"), "/open_file.rs"));
// ArpResolver (B5 Step 2a): one IPv4→MAC resolution's lifecycle. Actions call
// crate::net::* (the host double above counts requests/arms/failure).
include!(concat!(env!("OUT_DIR"), "/arp_resolver.rs"));
