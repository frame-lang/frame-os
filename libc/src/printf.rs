//! frame-libc printf engine (B10-3a, +B11-1 C-variadic, +B11-3c float).
//!
//! Frame owns the *parsing state machine* (`PrintfScan`: which char means what,
//! by mode — now incl. length modifiers + precision); native owns the *bytes*
//! (number/float → string, padding). Arguments arrive either as an explicit
//! `&[Arg]` slice (Rust front end) or, for the C `printf`/`fprintf`/`snprintf`/
//! `sprintf`/`vsnprintf` family, through a SysV `va_list` cursor (`VaArgs`) that
//! reads integer/pointer args from the general-purpose register/stack area and
//! floating args from the SSE area — so `%f`/`%e`/`%g` work (their args are
//! passed in xmm0-7, which the naked trampolines below spill).

use alloc::vec::Vec;

use crate::frame_systems::{PfDir, PrintfScan};
use crate::{strlen, write};

/// A printf argument — the stable-Rust stand-in for a C vararg.
pub enum Arg {
    Int(i64),
    UInt(u64),
    Float(f64),
    /// A NUL-terminated C string.
    Str(*const u8),
    Char(u8),
    Ptr(usize),
}

/// Format `fmt` with `args` (the Rust front end) into a byte buffer.
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
                prec,
                conv,
                ..
            } => {
                let arg = args.get(ai);
                ai += 1;
                render_conv(&mut out, *conv, *zero, *left, *width as usize, *prec, arg);
            }
        }
    }
    out
}

/// printf to stdout (fd 1), Rust front end.
pub fn print_fmt(fmt: &str, args: &[Arg]) {
    let s = vformat(fmt, args);
    write(1, &s);
}

// --- C-variadic va_list cursor (B11-1 integer, B11-3c float) ---------------
//
// Unified over two sources, both laid out like a SysV `va_list`:
//   - our naked trampolines, which spill the GP vararg registers (8-byte slots)
//     and xmm0-7 (16-byte slots, matching the ABI's SSE save-area stride);
//   - a real C `va_list` (`reg_save_area` + offsets + `overflow_arg_area`).
// GP and FP args draw from separate register pools (per the ABI) but share the
// stack overflow once their registers are exhausted.

pub(crate) struct VaArgs {
    gp: *const u8,
    gp_off: usize,
    gp_max: usize,
    fp: *const u8,
    fp_off: usize,
    fp_max: usize,
    ov: *const u8,
    ov_off: usize,
}

impl VaArgs {
    /// From a naked trampoline's spill: `ngp` general-purpose vararg slots
    /// (8 bytes each) at `gp`, 8 SSE slots (16 bytes each) at `fp`, stack
    /// overflow at `ov`.
    pub(crate) fn from_spill(gp: *const u8, ngp: usize, fp: *const u8, ov: *const u8) -> VaArgs {
        VaArgs {
            gp,
            gp_off: 0,
            gp_max: ngp * 8,
            fp,
            fp_off: 0,
            fp_max: 8 * 16,
            ov,
            ov_off: 0,
        }
    }

    /// From a real SysV `va_list`: the 176-byte register save area (6×8 GP then
    /// 8×16 SSE), the caller's current gp/fp offsets, and the overflow area.
    pub(crate) fn from_valist(
        reg_save: *const u8,
        gp_offset: usize,
        fp_offset: usize,
        ov: *const u8,
    ) -> VaArgs {
        VaArgs {
            gp: reg_save,
            gp_off: gp_offset,
            gp_max: 48,
            fp: reg_save,
            fp_off: fp_offset,
            fp_max: 176,
            ov,
            ov_off: 0,
        }
    }

    /// Next integer/pointer argument (8-byte slot).
    fn next_gp(&mut self) -> u64 {
        unsafe {
            if self.gp_off < self.gp_max {
                let v = (self.gp.add(self.gp_off) as *const u64).read_unaligned();
                self.gp_off += 8;
                v
            } else {
                let v = (self.ov.add(self.ov_off) as *const u64).read_unaligned();
                self.ov_off += 8;
                v
            }
        }
    }

    /// Next floating argument's bits (16-byte SSE slot; 8-byte stack slot once
    /// the SSE registers are exhausted).
    fn next_fp(&mut self) -> u64 {
        unsafe {
            if self.fp_off < self.fp_max {
                let v = (self.fp.add(self.fp_off) as *const u64).read_unaligned();
                self.fp_off += 16;
                v
            } else {
                let v = (self.ov.add(self.ov_off) as *const u64).read_unaligned();
                self.ov_off += 8;
                v
            }
        }
    }
}

/// Format a NUL-terminated C `fmt`, pulling one argument per conversion from
/// `va`, into a byte buffer. Honors length modifiers (`%ld` reads 64-bit) and
/// floating conversions (`%f`/`%e`/`%g` read from the SSE area).
pub(crate) fn vformat_va(fmt: *const u8, va: &mut VaArgs) -> Vec<u8> {
    let mut scan = PrintfScan::__create();
    let mut p = fmt;
    unsafe {
        while *p != 0 {
            scan.consume(*p as char);
            p = p.add(1);
        }
    }
    scan.finalize();
    let dirs = scan.directives();

    let mut out = Vec::new();
    for d in &dirs {
        match d {
            PfDir::Lit(c) => push_char(&mut out, *c),
            PfDir::Conv {
                zero,
                left,
                width,
                prec,
                long_arg,
                star_width,
                star_prec,
                conv,
            } => {
                // `*` width / precision read an `int` arg first, in C order:
                // width, then precision, then the value. A negative `*` width
                // means left-justify with its magnitude; a negative `*`
                // precision is treated as if omitted.
                let mut eff_left = *left;
                let mut eff_width = *width as usize;
                if *star_width {
                    let w = va.next_gp() as u32 as i32;
                    if w < 0 {
                        eff_left = true;
                        eff_width = w.unsigned_abs() as usize;
                    } else {
                        eff_width = w as usize;
                    }
                }
                let mut eff_prec = *prec;
                if *star_prec {
                    let p = va.next_gp() as u32 as i32;
                    eff_prec = if p < 0 { -1 } else { p };
                }
                let arg = match conv {
                    'd' | 'i' => Arg::Int(if *long_arg {
                        va.next_gp() as i64
                    } else {
                        va.next_gp() as u32 as i32 as i64
                    }),
                    'u' | 'x' | 'X' | 'o' => Arg::UInt(if *long_arg {
                        va.next_gp()
                    } else {
                        va.next_gp() as u32 as u64
                    }),
                    'f' | 'F' | 'e' | 'E' | 'g' | 'G' => Arg::Float(f64::from_bits(va.next_fp())),
                    'c' => Arg::Char(va.next_gp() as u8),
                    's' => Arg::Str(va.next_gp() as *const u8),
                    'p' => Arg::Ptr(va.next_gp() as usize),
                    _ => continue, // unknown: consume nothing
                };
                render_conv(&mut out, *conv, *zero, eff_left, eff_width, eff_prec, Some(&arg));
            }
        }
    }
    out
}

/// Copy `bytes` into the C `buf` of capacity `n` (NUL-terminated, truncated like
/// C99 `snprintf`); returns the length that *would* have been written.
fn emit_to_buf(buf: *mut u8, n: usize, bytes: &[u8]) -> i32 {
    if !buf.is_null() && n > 0 {
        let take = bytes.len().min(n - 1);
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, take);
            *buf.add(take) = 0;
        }
    }
    bytes.len() as i32
}

// --- C entry points: trampolines spill GP + SSE, then call the impl ---------

extern "C" fn vprintf_impl(fmt: *const u8, gp: *const u8, fp: *const u8, ov: *const u8) -> i32 {
    let mut va = VaArgs::from_spill(gp, 5, fp, ov);
    let bytes = vformat_va(fmt, &mut va);
    write(1, &bytes);
    bytes.len() as i32
}

extern "C" fn vsnprintf_impl(
    buf: *mut u8,
    n: usize,
    fmt: *const u8,
    gp: *const u8,
    fp: *const u8,
    ov: *const u8,
) -> i32 {
    let mut va = VaArgs::from_spill(gp, 3, fp, ov);
    let bytes = vformat_va(fmt, &mut va);
    emit_to_buf(buf, n, &bytes)
}

extern "C" fn vsprintf_impl(
    buf: *mut u8,
    fmt: *const u8,
    gp: *const u8,
    fp: *const u8,
    ov: *const u8,
) -> i32 {
    let mut va = VaArgs::from_spill(gp, 4, fp, ov);
    let bytes = vformat_va(fmt, &mut va);
    emit_to_buf(buf, usize::MAX, &bytes)
}

/// C `printf(fmt, ...)`. Spill the 5 GP vararg regs (rsi,rdx,rcx,r8,r9; fmt in
/// rdi) + xmm0-7, then call the impl with pointers to the GP area, the SSE area,
/// and the stack overflow. 5 GP pushes (40) + 128 SSE from a post-call rsp leave
/// rsp 16-aligned.
#[no_mangle]
#[unsafe(naked)]
pub unsafe extern "C" fn printf(_fmt: *const u8) -> i32 {
    core::arch::naked_asm!(
        "push r9", "push r8", "push rcx", "push rdx", "push rsi", // GP area (40)
        "sub rsp, 128",                                            // SSE area (8*16)
        "movups [rsp + 0], xmm0", "movups [rsp + 16], xmm1",
        "movups [rsp + 32], xmm2", "movups [rsp + 48], xmm3",
        "movups [rsp + 64], xmm4", "movups [rsp + 80], xmm5",
        "movups [rsp + 96], xmm6", "movups [rsp + 112], xmm7",
        "lea rsi, [rsp + 128]",  // arg1 = GP area
        "mov rdx, rsp",          // arg2 = SSE area
        "lea rcx, [rsp + 176]",  // arg3 = overflow (128 SSE + 40 GP + 8 ret)
        "call {f}",
        "add rsp, 168",
        "ret",
        f = sym vprintf_impl,
    );
}

/// C `snprintf(buf, n, fmt, ...)`. buf/n/fmt in rdi/rsi/rdx; spill the 3 GP
/// vararg regs (rcx,r8,r9) + xmm0-7.
#[no_mangle]
#[unsafe(naked)]
pub unsafe extern "C" fn snprintf(_buf: *mut u8, _n: usize, _fmt: *const u8) -> i32 {
    core::arch::naked_asm!(
        "push r9", "push r8", "push rcx", // GP area (24)
        "sub rsp, 128",
        "movups [rsp + 0], xmm0", "movups [rsp + 16], xmm1",
        "movups [rsp + 32], xmm2", "movups [rsp + 48], xmm3",
        "movups [rsp + 64], xmm4", "movups [rsp + 80], xmm5",
        "movups [rsp + 96], xmm6", "movups [rsp + 112], xmm7",
        "lea rcx, [rsp + 128]", // arg4 = GP area
        "mov r8, rsp",          // arg5 = SSE area
        "lea r9, [rsp + 160]",  // arg6 = overflow (128 + 24 + 8)
        "call {f}",
        "add rsp, 152",
        "ret",
        f = sym vsnprintf_impl,
    );
}

/// C `sprintf(buf, fmt, ...)`. buf/fmt in rdi/rsi; spill the 4 GP vararg regs
/// (rdx,rcx,r8,r9) + xmm0-7. 4 GP pushes (32) + 128 leave rsp ≡ 8 mod 16, so pad.
#[no_mangle]
#[unsafe(naked)]
pub unsafe extern "C" fn sprintf(_buf: *mut u8, _fmt: *const u8) -> i32 {
    core::arch::naked_asm!(
        "push r9", "push r8", "push rcx", "push rdx", // GP area (32)
        "sub rsp, 128",
        "movups [rsp + 0], xmm0", "movups [rsp + 16], xmm1",
        "movups [rsp + 32], xmm2", "movups [rsp + 48], xmm3",
        "movups [rsp + 64], xmm4", "movups [rsp + 80], xmm5",
        "movups [rsp + 96], xmm6", "movups [rsp + 112], xmm7",
        "lea rdx, [rsp + 128]", // arg3 = GP area
        "mov rcx, rsp",         // arg4 = SSE area
        "lea r8, [rsp + 168]",  // arg5 = overflow (128 + 32 + 8)
        "sub rsp, 8",           // 16-align the call
        "call {f}",
        "add rsp, 8",
        "add rsp, 160",
        "ret",
        f = sym vsprintf_impl,
    );
}

// A real C `va_list` (`__builtin_va_list` = one `__va_list_tag`).
#[repr(C)]
pub struct VaListTag {
    gp_offset: u32,
    fp_offset: u32,
    overflow_arg_area: *mut u8,
    reg_save_area: *mut u8,
}

/// C `vsnprintf(buf, n, fmt, ap)` — format using a caller-provided `va_list`
/// (the va_list-taking sibling of `snprintf`; tcc's error/format helpers call
/// this). Reads args straight from the caller's register-save + overflow areas.
#[no_mangle]
pub unsafe extern "C" fn vsnprintf(buf: *mut u8, n: usize, fmt: *const u8, ap: *mut VaListTag) -> i32 {
    if ap.is_null() {
        return emit_to_buf(buf, n, b"");
    }
    let a = &*ap;
    let mut va = VaArgs::from_valist(
        a.reg_save_area,
        a.gp_offset as usize,
        a.fp_offset as usize,
        a.overflow_arg_area,
    );
    let bytes = vformat_va(fmt, &mut va);
    emit_to_buf(buf, n, &bytes)
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

fn float_of(arg: Option<&Arg>) -> f64 {
    match arg {
        Some(Arg::Float(f)) => *f,
        _ => 0.0,
    }
}

/// Format `v` in `base` (8/10/16) into `buf` from the right; return the digits.
fn fmt_uint(mut v: u64, base: u64, upper: bool, buf: &mut [u8]) -> usize {
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
    i
}

fn fmt_int(v: i64, buf: &mut [u8]) -> usize {
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
    i
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

/// Apply an integer precision (minimum digit count) to `digits` by left-padding
/// with `0`s, into `dst`. Returns the (possibly grown) byte run.
fn apply_int_prec(digits: &[u8], prec: i32, dst: &mut Vec<u8>) {
    let p = if prec < 0 { 0 } else { prec as usize };
    if digits.len() < p {
        dst.resize(p - digits.len(), b'0');
    }
    dst.extend_from_slice(digits);
}

fn render_conv(
    out: &mut Vec<u8>,
    conv: char,
    zero: bool,
    left: bool,
    width: usize,
    prec: i32,
    arg: Option<&Arg>,
) {
    let mut tmp = [0u8; 32];
    // Precision suppresses the `0` flag for numeric conversions (C rule).
    let zero = zero && prec < 0;
    match conv {
        'd' | 'i' => {
            let v = match arg {
                Some(Arg::Int(n)) => *n,
                Some(Arg::UInt(n)) => *n as i64,
                _ => 0,
            };
            let i = fmt_int(v, &mut tmp);
            if prec < 0 {
                pad_and_emit(out, &tmp[i..], left, zero, width);
            } else {
                let mut body = Vec::new();
                // Keep the sign outside the zero-padding.
                if v < 0 {
                    body.push(b'-');
                    apply_int_prec(&tmp[i + 1..], prec, &mut body);
                } else {
                    apply_int_prec(&tmp[i..], prec, &mut body);
                }
                pad_and_emit(out, &body, left, false, width);
            }
        }
        'u' | 'x' | 'X' | 'o' => {
            let base = if conv == 'o' { 8 } else if conv == 'u' { 10 } else { 16 };
            let i = fmt_uint(uint_of(arg), base, conv == 'X', &mut tmp);
            if prec < 0 {
                pad_and_emit(out, &tmp[i..], left, zero, width);
            } else {
                let mut body = Vec::new();
                apply_int_prec(&tmp[i..], prec, &mut body);
                pad_and_emit(out, &body, left, false, width);
            }
        }
        'p' => {
            out.extend_from_slice(b"0x");
            let i = fmt_uint(uint_of(arg), 16, false, &mut tmp);
            out.extend_from_slice(&tmp[i..]);
        }
        'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
            let body = crate::cfloat::format_float(conv, float_of(arg), prec);
            pad_and_emit(out, &body, left, zero, width);
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
            // Precision caps the number of bytes printed for `%s`.
            let s = if prec >= 0 && (prec as usize) < s.len() {
                &s[..prec as usize]
            } else {
                s
            };
            pad_and_emit(out, s, left, false, width);
        }
        _ => {}
    }
}
