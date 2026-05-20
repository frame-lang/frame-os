// kernel/src/frame_systems.rs
//
// Pulls in the Rust code framec generates from frame/kernel.frs (written
// to OUT_DIR by build.rs) and makes the `Kernel` system available to the
// rest of the crate.
//
// Mirror of shell/src/frame_systems.rs, with one extra wrinkle: framec's
// generated code refers to `String`, `Vec`, `Box`, and `to_string`
// unqualified (it expects them from the std prelude). The kernel is
// no_std, so those names aren't automatically in scope. We re-export them
// from `alloc` here so the generated `mod _kernel_framec { use super::*; }`
// wrapper picks them up via its glob import.
//
// (`Rc`, `format!`, and `vec!` don't need re-exporting: the generated
// code uses fully-qualified `alloc::rc::Rc` and the wrapper module
// imports `alloc::{vec, format}` itself.)

extern crate alloc;
pub use alloc::boxed::Box;
pub use alloc::string::{String, ToString};
pub use alloc::vec::Vec;

// The generated `Kernel` and `SerialDriver` actions call `serial::*`
// (writeln / write_str / write_byte / init_uart). The glob import in each
// generated module resolves `serial` through this private `use`.
use crate::serial;

// SerialDriver first: the Kernel holds a `SerialDriver` in its domain
// (`console: SerialDriver = @@SerialDriver()`). Rust items are
// order-independent within a module, but generating the dependency first
// keeps the include order matching the dependency direction.
include!(concat!(env!("OUT_DIR"), "/serial_driver.rs"));
include!(concat!(env!("OUT_DIR"), "/kernel.rs"));
