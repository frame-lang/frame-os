// shell/src/lib.rs
//
// Library crate for frame-os-shell. The binary at src/main.rs is a thin
// wrapper; the actual logic lives here and in the generated Frame code.
//
// Exposing this as a library makes the Frame systems available to:
//   - The binary main loop
//   - Integration tests in tests/
//   - Unit tests in #[cfg(test)] blocks
//
// The Frame-generated code is included via the frame_systems module.

pub mod builtin;
pub mod frame_systems;

pub use builtin::Builtin;
pub use frame_systems::{Parser, Shell};
