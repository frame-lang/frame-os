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
    /// (left-justified if `left`, zero-filled if `zero` and not left). `prec` is
    /// the precision (`-1` = none): min digits for integers, max length for `%s`.
    /// `long_arg` is set by an `l`/`ll` length modifier (read a 64-bit argument).
    Conv {
        zero: bool,
        left: bool,
        width: u32,
        prec: i32,
        long_arg: bool,
        conv: char,
    },
}

/// Whether `c` is a conversion specifier frame-libc's printf understands —
/// integers (`d i u x X o`), float (`f F e E g G`), and `c`/`s`/`p`. The float
/// args arrive in SSE registers; the variadic trampolines spill xmm0-7 and the
/// engine reads them (B11-3c).
pub fn is_conv(c: char) -> bool {
    matches!(
        c,
        'd' | 'i' | 'u' | 'x' | 'X' | 'o' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'c' | 's' | 'p'
    )
}

/// Whether `c` is a printf length-modifier char (consumed, not emitted). `l`/`ll`
/// widen the argument to 64-bit (tracked separately); the rest are no-ops on this
/// target where the relevant types are already their natural width.
pub fn is_length_mod(c: char) -> bool {
    matches!(c, 'l' | 'h' | 'z' | 'j' | 't' | 'L')
}

include!(concat!(env!("OUT_DIR"), "/printf_scan.rs"));
// OpenFile (B10-3b): the *same* FSM the kernel's VFS uses (frame/open_file.frs),
// reused here to gate a FILE*'s read/write mode — one source, two targets.
include!(concat!(env!("OUT_DIR"), "/open_file.rs"));
