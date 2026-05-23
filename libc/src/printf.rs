//! frame-libc printf engine (B10-3a): drive the `PrintfScan` Frame FSM to parse
//! a format string, then render each directive natively.
//!
//! The split is the recurring one — Frame owns the *parsing state machine*
//! (which char means what, by mode), native owns the *bytes* (number→string
//! conversion, padding). Until B11 wires the C-variadic ABI, arguments arrive as
//! an explicit `&[Arg]` slice (the Rust-friendly front end); the scanner +
//! conversion code below is exactly what the variadic `printf(fmt, ...)` shim
//! will call once it can read the SysV register-save area.

use alloc::vec::Vec;

use crate::frame_systems::{PfDir, PrintfScan};
use crate::{strlen, write};

/// A printf argument — the stable-Rust stand-in for a C vararg. The engine
/// consumes one per conversion, in order.
pub enum Arg {
    Int(i64),
    UInt(u64),
    /// A NUL-terminated C string.
    Str(*const u8),
    Char(u8),
    Ptr(usize),
}

/// Format `fmt` with `args` into a freshly allocated byte buffer. Drives the
/// `PrintfScan` FSM to parse the format, then renders each directive.
pub fn vformat(fmt: &str, args: &[Arg]) -> Vec<u8> {
    let mut scan = PrintfScan::__create();
    for c in fmt.chars() {
        scan.consume(c);
    }
    scan.finalize();
    let dirs = scan.directives();

    let mut out = Vec::new();
    let mut ai = 0usize;
    for d in &dirs {
        match d {
            PfDir::Lit(c) => push_char(&mut out, *c),
            PfDir::Conv {
                zero,
                left,
                width,
                conv,
            } => {
                let arg = args.get(ai);
                ai += 1;
                render_conv(&mut out, *conv, *zero, *left, *width as usize, arg);
            }
        }
    }
    out
}

/// printf to stdout (fd 1). The Rust-friendly front end; the C-ABI variadic
/// `printf(fmt, ...)` lands at B11 with tcc.
pub fn print_fmt(fmt: &str, args: &[Arg]) {
    let s = vformat(fmt, args);
    write(1, &s);
}

// --- rendering -------------------------------------------------------------

fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

fn uint_of(arg: Option<&Arg>) -> u64 {
    match arg {
        Some(Arg::UInt(n)) => *n,
        Some(Arg::Int(n)) => *n as u64,
        Some(Arg::Ptr(p)) => *p as u64,
        _ => 0,
    }
}

/// Format `v` in `base` (10/16) into `buf` from the right; return the digits.
fn fmt_uint(mut v: u64, base: u64, upper: bool, buf: &mut [u8]) -> &[u8] {
    let digits: &[u8] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut i = buf.len();
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    } else {
        while v > 0 {
            i -= 1;
            buf[i] = digits[(v % base) as usize];
            v /= base;
        }
    }
    &buf[i..]
}

/// Format signed `v` as decimal (handles `i64::MIN` via `unsigned_abs`).
fn fmt_int(v: i64, buf: &mut [u8]) -> &[u8] {
    let mut m = v.unsigned_abs();
    let mut i = buf.len();
    if m == 0 {
        i -= 1;
        buf[i] = b'0';
    } else {
        while m > 0 {
            i -= 1;
            buf[i] = b'0' + (m % 10) as u8;
            m /= 10;
        }
    }
    if v < 0 {
        i -= 1;
        buf[i] = b'-';
    }
    &buf[i..]
}

/// Emit `body` padded to `width`: spaces on the right if `left`, else zeros
/// (`zero`, after any leading sign) or spaces on the left.
fn pad_and_emit(out: &mut Vec<u8>, body: &[u8], left: bool, zero: bool, width: usize) {
    let pad = width.saturating_sub(body.len());
    if pad == 0 {
        out.extend_from_slice(body);
    } else if left {
        out.extend_from_slice(body);
        out.resize(out.len() + pad, b' ');
    } else if zero {
        if body.first() == Some(&b'-') {
            out.push(b'-');
            out.resize(out.len() + pad, b'0');
            out.extend_from_slice(&body[1..]);
        } else {
            out.resize(out.len() + pad, b'0');
            out.extend_from_slice(body);
        }
    } else {
        out.resize(out.len() + pad, b' ');
        out.extend_from_slice(body);
    }
}

fn render_conv(
    out: &mut Vec<u8>,
    conv: char,
    zero: bool,
    left: bool,
    width: usize,
    arg: Option<&Arg>,
) {
    let mut tmp = [0u8; 32];
    match conv {
        'd' | 'i' => {
            let v = match arg {
                Some(Arg::Int(n)) => *n,
                Some(Arg::UInt(n)) => *n as i64,
                _ => 0,
            };
            let body = fmt_int(v, &mut tmp);
            pad_and_emit(out, body, left, zero, width);
        }
        'u' => {
            let body = fmt_uint(uint_of(arg), 10, false, &mut tmp);
            pad_and_emit(out, body, left, zero, width);
        }
        'x' => {
            let body = fmt_uint(uint_of(arg), 16, false, &mut tmp);
            pad_and_emit(out, body, left, zero, width);
        }
        'X' => {
            let body = fmt_uint(uint_of(arg), 16, true, &mut tmp);
            pad_and_emit(out, body, left, zero, width);
        }
        'p' => {
            // Pointers: a "0x" prefix then lowercase hex (no padding, like C).
            out.extend_from_slice(b"0x");
            let body = fmt_uint(uint_of(arg), 16, false, &mut tmp);
            out.extend_from_slice(body);
        }
        'c' => {
            let b = match arg {
                Some(Arg::Char(b)) => *b,
                Some(Arg::Int(n)) => *n as u8,
                _ => 0,
            };
            pad_and_emit(out, &[b], left, false, width);
        }
        's' => {
            let p = match arg {
                Some(Arg::Str(p)) => *p,
                _ => core::ptr::null(),
            };
            let s: &[u8] = if p.is_null() {
                b"(null)"
            } else {
                unsafe { core::slice::from_raw_parts(p, strlen(p)) }
            };
            pad_and_emit(out, s, left, false, width);
        }
        _ => {}
    }
}
