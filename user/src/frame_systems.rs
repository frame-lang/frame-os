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
