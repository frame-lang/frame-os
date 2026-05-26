// user/src/frame_systems.rs
//
// Pulls in the Rust framec generates from the reused `.frs` sources (written to
// OUT_DIR by build.rs) and exposes the systems to the ring-3 program. Mirrors
// `kernel/src/frame_systems.rs`: framec's generated code refers to `String`,
// `Vec`, and `Box` unqualified (it expects the std prelude), so we re-export
// them from `alloc` here — the generated `mod _parser_framec { use super::*; }`
// wrapper picks them up via its glob import. (`Rc`, `BTreeMap`, `format!`, and
// `vec!` are used fully-qualified or imported by the wrapper itself.)
//
// This is the crux of B4 Step 4b: the *same* `frame/parser.frs` the hosted
// shell compiles also compiles here for `x86_64-unknown-none`, unchanged.

pub use alloc::boxed::Box;
pub use alloc::string::{String, ToString};
pub use alloc::vec::Vec;

include!(concat!(env!("OUT_DIR"), "/parser.rs"));

// One background-job table entry for the IshJobs FSM (S10). The FSM declares its
// domain field as `Vec<JobEntry>`; Frame treats `JobEntry` as an opaque type and
// passes it through verbatim, so it must resolve in the generated module's scope.
// The generated `mod _ishjobs_framec { use super::*; }` wrapper picks this up via
// its glob import (same mechanism as String/Vec/Box above). Clone is required
// because snapshot() hands ish a clone of the table to iterate.
#[derive(Clone)]
pub struct JobEntry {
    /// POSIX-job-spec id (1, 2, 3, ...), assigned by the FSM on launch_bg.
    pub id: u32,
    /// The child's process id, as returned by fork().
    pub pid: u64,
    /// The command line as typed (for `jobs` / "Done" reporting).
    pub cmd: String,
    /// Set once the child has been reaped via the non-blocking sweep.
    pub done: bool,
    /// Set while the job is job-control stopped (SIGTSTP/SIGSTOP); cleared on
    /// resume (bg/fg). Mutually exclusive with `done` in practice.
    pub stopped: bool,
    /// The child's exit code, valid once `done` is true.
    pub code: i32,
}

include!(concat!(env!("OUT_DIR"), "/ish_jobs.rs"));
