//! Floating-point: the `%f`/`%e`/`%g` formatter the printf engine calls, plus
//! `strtod`/`strtof` parsing and `ldexp` (B11-3c). All `f64`/`f32` work is pure
//! Rust — only 80-bit `long double` (`strtold`) needs a C shim (see
//! `libc/csrc/strtold.c`), because Rust has no f80 type.
//!
//! `core` (no_std) gives us f64 arithmetic + `is_nan`/`is_infinite`/`to_bits`
//! but NOT `abs`/`floor`/`round`/`pow` (those are std/libm), so those are done
//! by bit-twiddling the sign + integer truncation via `as u64`. The conversions
//! are correct for the magnitudes a compiler emits (diagnostics, constants), not
//! guaranteed last-bit IEEE rounding — see the roadmap tech-debt note.

use alloc::vec::Vec;
use core::ffi::c_char;

/// |x| without std's `f64::abs`: clear the sign bit.
fn fabs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & 0x7FFF_FFFF_FFFF_FFFF)
}

/// 10^e as f64 (loop; over/underflows to inf/0 as IEEE requires).
fn pow10(mut e: i32) -> f64 {
    let mut r = 1.0f64;
    while e > 0 {
        r *= 10.0;
        e -= 1;
    }
    while e < 0 {
        r *= 0.1;
        e += 1;
    }
    r
}

/// `ldexp(x, exp)` — x · 2^exp. (Exact via repeated ×2 / ×0.5 until over/under-
/// flow, which is the correct IEEE result.)
#[no_mangle]
pub extern "C" fn ldexp(mut x: f64, mut exp: i32) -> f64 {
    while exp > 0 {
        x *= 2.0;
        exp -= 1;
    }
    while exp < 0 {
        x *= 0.5;
        exp += 1;
    }
    x
}

/// Push `n`'s decimal digits (no sign) to `out`.
fn push_u64(out: &mut Vec<u8>, mut n: u64) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    while n > 0 {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    out.extend_from_slice(&tmp[i..]);
}

/// `%f`: fixed-point with `p` fraction digits. `v` is non-negative and finite.
fn fmt_fixed(out: &mut Vec<u8>, v: f64, p: usize) {
    let p = p.min(18); // 10^p must fit a u64
    let scale = 10u64.pow(p as u32);
    let ip = v as u64; // integer part (truncated toward zero)
    let frac = v - ip as f64;
    let mut scaled = (frac * scale as f64 + 0.5) as u64; // rounded fraction
    let mut ip = ip;
    if scaled >= scale {
        // rounding carried into the integer part
        ip += 1;
        scaled -= scale;
    }
    push_u64(out, ip);
    if p > 0 {
        out.push(b'.');
        // zero-pad the fraction to exactly p digits
        let start = out.len();
        push_u64(out, scaled);
        let got = out.len() - start;
        if got < p {
            // insert leading zeros
            let pad = p - got;
            let mut frac_digits = out.split_off(start);
            out.resize(out.len() + pad, b'0');
            out.append(&mut frac_digits);
        }
    }
}

/// `%e`: scientific `d.ddde±XX`. `v` is non-negative and finite.
fn fmt_sci(out: &mut Vec<u8>, v: f64, p: usize, upper: bool) {
    let mut mant = v;
    let mut e10 = 0i32;
    if mant != 0.0 {
        while mant >= 10.0 {
            mant *= 0.1;
            e10 += 1;
        }
        while mant < 1.0 {
            mant *= 10.0;
            e10 -= 1;
        }
    }
    // Round the mantissa to p fraction digits; a carry may renormalize.
    let scale = 10u64.pow(p.min(18) as u32);
    let mut digits = (mant * scale as f64 + 0.5) as u64; // p+1 significant digits
    if digits >= 10 * scale {
        digits /= 10;
        e10 += 1;
    }
    let lead = digits / scale;
    let frac = digits % scale;
    push_u64(out, lead);
    if p > 0 {
        out.push(b'.');
        let start = out.len();
        push_u64(out, frac);
        let got = out.len() - start;
        if got < p {
            let pad = p - got;
            let mut frac_digits = out.split_off(start);
            out.resize(out.len() + pad, b'0');
            out.append(&mut frac_digits);
        }
    }
    out.push(if upper { b'E' } else { b'e' });
    out.push(if e10 < 0 { b'-' } else { b'+' });
    let ae = e10.unsigned_abs();
    if ae < 10 {
        out.push(b'0');
    }
    push_u64(out, ae as u64);
}

/// Strip trailing fraction zeros (and a bare trailing '.') in `%g`, within the
/// digit run that starts at `from` (before any exponent).
fn strip_trailing_zeros(out: &mut Vec<u8>, from: usize, end: usize) {
    if !out[from..end].contains(&b'.') {
        return;
    }
    let mut e = end;
    while e > from && out[e - 1] == b'0' {
        e -= 1;
    }
    if e > from && out[e - 1] == b'.' {
        e -= 1;
    }
    out.drain(e..end);
}

/// Format `v` per the `%f`/`%e`/`%g` conversion `conv` with precision `prec`
/// (-1 = the C default of 6). Returns the rendered bytes (sign included).
pub fn format_float(conv: char, v: f64, prec: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let neg = v.is_sign_negative() && !v.is_nan();
    if neg {
        out.push(b'-');
    }
    let upper = conv.is_ascii_uppercase();
    if v.is_nan() {
        out.clear();
        out.extend_from_slice(if upper { b"NAN" } else { b"nan" });
        return out;
    }
    if v.is_infinite() {
        out.extend_from_slice(if upper { b"INF" } else { b"inf" });
        return out;
    }
    let a = fabs(v);
    let p = if prec < 0 { 6 } else { prec as usize };
    match conv {
        'f' | 'F' => fmt_fixed(&mut out, a, p),
        'e' | 'E' => fmt_sci(&mut out, a, p, upper),
        'g' | 'G' => {
            // %g: P significant digits; choose %e or %f, then strip zeros.
            let bigp = if p == 0 { 1 } else { p };
            // decimal exponent of `a`
            let mut e10 = 0i32;
            let mut m = a;
            if m != 0.0 {
                while m >= 10.0 {
                    m *= 0.1;
                    e10 += 1;
                }
                while m < 1.0 {
                    m *= 10.0;
                    e10 -= 1;
                }
            }
            let start = out.len();
            if e10 < -4 || e10 >= bigp as i32 {
                fmt_sci(&mut out, a, bigp - 1, upper);
                // strip zeros only in the mantissa (before 'e'/'E')
                if let Some(epos) = out[start..]
                    .iter()
                    .position(|&c| c == b'e' || c == b'E')
                    .map(|i| start + i)
                {
                    strip_trailing_zeros(&mut out, start, epos);
                }
            } else {
                let fp = (bigp as i32 - 1 - e10).max(0) as usize;
                fmt_fixed(&mut out, a, fp);
                let end = out.len();
                strip_trailing_zeros(&mut out, start, end);
            }
        }
        _ => {}
    }
    out
}

// --- strtod / strtof -------------------------------------------------------

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 11 | 12)
}

/// `strtod(s, end)` — parse a decimal float (sign, integer/fraction, optional
/// `e`/`E` exponent). Sets `*end` past the consumed text (or to `s` if none).
#[no_mangle]
pub unsafe extern "C" fn strtod(s: *const c_char, end: *mut *mut c_char) -> f64 {
    let mut i = 0usize;
    let at = |i: usize| *s.add(i) as u8;
    while is_space(at(i)) {
        i += 1;
    }
    let mut neg = false;
    match at(i) {
        b'+' => i += 1,
        b'-' => {
            neg = true;
            i += 1;
        }
        _ => {}
    }
    let mut val = 0.0f64;
    let mut any = false;
    while at(i).is_ascii_digit() {
        val = val * 10.0 + (at(i) - b'0') as f64;
        any = true;
        i += 1;
    }
    let mut frac_digits = 0i32;
    if at(i) == b'.' {
        i += 1;
        while at(i).is_ascii_digit() {
            val = val * 10.0 + (at(i) - b'0') as f64;
            frac_digits += 1;
            any = true;
            i += 1;
        }
    }
    let mut exp10 = -frac_digits;
    if any && (at(i) == b'e' || at(i) == b'E') {
        let mut j = i + 1;
        let mut eneg = false;
        match at(j) {
            b'+' => j += 1,
            b'-' => {
                eneg = true;
                j += 1;
            }
            _ => {}
        }
        if at(j).is_ascii_digit() {
            let mut e = 0i32;
            while at(j).is_ascii_digit() {
                e = e.saturating_mul(10).saturating_add((at(j) - b'0') as i32);
                j += 1;
            }
            exp10 += if eneg { -e } else { e };
            i = j;
        }
    }
    if !end.is_null() {
        *end = s.add(if any { i } else { 0 }) as *mut c_char;
    }
    let r = val * pow10(exp10);
    if neg {
        -r
    } else {
        r
    }
}

/// `strtof(s, end)` — `strtod` narrowed to `f32`.
#[no_mangle]
pub unsafe extern "C" fn strtof(s: *const c_char, end: *mut *mut c_char) -> f32 {
    strtod(s, end) as f32
}
