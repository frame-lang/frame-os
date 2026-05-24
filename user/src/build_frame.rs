// user/src/build_frame.rs
//
// The BuildDriver Frame system (frame/builddriver.frs → framec → OUT_DIR) for
// the `build` program (B11-3e). Separate from the shared `frame_systems.rs`
// (which the shells use for `Parser`) because the generated code calls
// `crate::actions::*` — functions only the `build` bin defines. framec's output
// refers to `String`/`Vec`/`Box` unqualified, so re-export them from `alloc`
// for the generated `use super::*` wrapper.
pub use alloc::boxed::Box;
pub use alloc::string::{String, ToString};
pub use alloc::vec::Vec;

include!(concat!(env!("OUT_DIR"), "/builddriver.rs"));
