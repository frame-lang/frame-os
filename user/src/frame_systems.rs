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
//
// This module is shared FSM glue: `mod frame_systems` is included by BOTH `ish`
// (uses Parser + IshJobs + JobEntry) and `frameshell` (uses Parser only). Each
// binary therefore drags in the other's subset, so per-bin some items are
// legitimately unused (e.g. JobEntry::cmd and the generated `_ishjobs_framec`
// glob in the frameshell build). Allow dead_code / unused_imports module-wide
// rather than chase per-bin false positives.
#![allow(dead_code, unused_imports)]

pub use alloc::boxed::Box;
pub use alloc::string::{String, ToString};
pub use alloc::vec::Vec;

include!(concat!(env!("OUT_DIR"), "/parser.rs"));

// Pipeline (M3a): the shared command-line grammar FSM. Declares TokenKind +
// Command in its native prolog; consumes the Parser's typed tokens. Included
// after parser.rs so the generated module sees `Token` (parser's prolog type)
// via its `use super::*` glob. The SAME frame/pipeline.frs the hosted shell
// compiles — one FSM source, both targets.
include!(concat!(env!("OUT_DIR"), "/pipeline.rs"));

// (IshJobs + its JobEntry were retired at M4.3b — ish's job table is now the
// shared JobControl FSM, compiled in ish.rs's job_fsm module over the
// SyscallProcessBackend. frame_systems is left with the parser+pipeline systems
// shared across ish + frameshell.)
