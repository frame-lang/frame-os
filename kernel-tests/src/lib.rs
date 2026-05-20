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

    /// Return a copy of everything captured on this thread so far.
    pub fn captured() -> String {
        CAPTURED.with(|c| c.borrow().clone())
    }

    /// Reset the capture buffer for this thread.
    pub fn clear() {
        CAPTURED.with(|c| c.borrow_mut().clear());
    }
}

// Pull in the framec-generated Kernel. `pub use _kernel_framec::*;` at the
// end of the generated file re-exports `Kernel` at this crate's root.
include!(concat!(env!("OUT_DIR"), "/kernel.rs"));
