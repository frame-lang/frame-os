// libc/src/frame_systems.rs
//
// Pulls in the Rust framec generates from frame-libc's `.frs` sources (written
// to OUT_DIR by build.rs) and the native types they reference. Mirrors the
// kernel/user `frame_systems.rs`: the generated `mod _printfscan_framec { use
// super::*; }` wrapper resolves `String`/`Vec`/`PfDir`/`is_conv` through this
// module's items, so they must be in scope here.

pub use alloc::boxed::Box;
pub use alloc::string::{String, ToString};
pub use alloc::vec::Vec;

/// One element of a parsed printf format string (B10-3a). The `PrintfScan` FSM
/// accumulates these in its domain; the native engine in `printf.rs` walks them,
/// copying `Lit`s and formatting one argument per `Conv`.
#[derive(Clone)]
pub enum PfDir {
    /// A literal character to copy to the output verbatim.
    Lit(char),
    /// A conversion spec: format the next argument as `conv`, padded to `width`
    /// (left-justified if `left`, zero-filled if `zero` and not left).
    Conv {
        zero: bool,
        left: bool,
        width: u32,
        conv: char,
    },
}

/// Whether `c` is a conversion specifier frame-libc's printf understands.
pub fn is_conv(c: char) -> bool {
    matches!(c, 'd' | 'i' | 'u' | 'x' | 'X' | 'c' | 's' | 'p')
}

include!(concat!(env!("OUT_DIR"), "/printf_scan.rs"));
// OpenFile (B10-3b): the *same* FSM the kernel's VFS uses (frame/open_file.frs),
// reused here to gate a FILE*'s read/write mode — one source, two targets.
include!(concat!(env!("OUT_DIR"), "/open_file.rs"));
