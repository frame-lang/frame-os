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
