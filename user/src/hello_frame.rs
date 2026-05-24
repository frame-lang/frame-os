// user/src/hello_frame.rs
//
// The Hello Frame system (frame/hello.frs → framec → OUT_DIR/hello.rs) for the
// `fhello` capstone bin. framec's generated module refers to `String`/`Vec`/
// `Box` unqualified (via its `use super::*`), so re-export them from `alloc`.
// The very same hello.frs is *also* transpiled to C (staged at /fhello.c and
// built by the on-device tcc) — one Frame source, both languages.
pub use alloc::boxed::Box;
pub use alloc::string::{String, ToString};
pub use alloc::vec::Vec;

include!(concat!(env!("OUT_DIR"), "/hello.rs"));
