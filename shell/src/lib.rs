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
pub mod exec;
pub mod frame_systems;
pub mod job_summary;
pub mod process_backend;
pub mod shell_env;
pub mod signals;

pub use builtin::Builtin;
pub use frame_systems::{
    Command, CommandKind, Job, JobControl, Parser, Pipeline, Shell, ShellEnv, Token, TokenKind,
};
pub use job_summary::JobSummary;
