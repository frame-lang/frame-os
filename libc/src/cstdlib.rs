//! C `<stdlib.h>` functions tcc needs beyond malloc/free/calloc/realloc/exit
//! (B11-3c): integer parsing (atoi, strtol/strtoul/strtoull), abort, getenv,
//! and qsort. Plain mechanics — no Frame system.

use core::arch::naked_asm;
use core::ffi::c_char;

use crate::exit;

#[inline]
unsafe fn byte_at(s: *const c_char, i: usize) -> u8 {
    *s.add(i) as u8
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 11 | 12)
}

fn digit_val(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u32),
        b'a'..=b'z' => Some((b - b'a' + 10) as u32),
        b'A'..=b'Z' => Some((b - b'A' + 10) as u32),
        _ => None,
    }
}

/// Shared unsigned parser for the strtoul/strtoull family: skip leading
/// whitespace + an optional sign, auto-detect base (0 → 0x/0o-octal/decimal),
/// accumulate digits, and report the end pointer. Returns (magnitude, negative).
unsafe fn parse_uint(s: *const c_char, end: *mut *mut c_char, mut base: i32) -> (u64, bool) {
    let mut i = 0usize;
    while is_space(byte_at(s, i)) {
        i += 1;
    }
    let mut neg = false;
    match byte_at(s, i) {
        b'+' => i += 1,
        b'-' => {
            neg = true;
            i += 1;
        }
        _ => {}
    }
    // Base detection / prefix skipping.
    if (base == 0 || base == 16)
        && byte_at(s, i) == b'0'
        && (byte_at(s, i + 1) == b'x' || byte_at(s, i + 1) == b'X')
    {
        base = 16;
        i += 2;
    } else if base == 0 && byte_at(s, i) == b'0' {
        base = 8;
        i += 1;
    } else if base == 0 {
        base = 10;
    }
    let mut val: u64 = 0;
    let mut any = false;
    loop {
        match digit_val(byte_at(s, i)) {
            Some(d) if (d as i32) < base => {
                val = val.wrapping_mul(base as u64).wrapping_add(d as u64);
                any = true;
                i += 1;
            }
            _ => break,
        }
    }
    if !end.is_null() {
        // No conversion → endptr is the original string (i back to 0). Standard.
        let consumed = if any { i } else { 0 };
        *end = s.add(consumed) as *mut c_char;
    }
    (val, neg)
}

/// `strtoull(s, end, base)` — unsigned long long.
#[no_mangle]
pub unsafe extern "C" fn strtoull(s: *const c_char, end: *mut *mut c_char, base: i32) -> u64 {
    let (v, neg) = parse_uint(s, end, base);
    if neg {
        (!v).wrapping_add(1)
    } else {
        v
    }
}

/// `strtoul(s, end, base)` — unsigned long (64-bit on this target).
#[no_mangle]
pub unsafe extern "C" fn strtoul(s: *const c_char, end: *mut *mut c_char, base: i32) -> u64 {
    strtoull(s, end, base)
}

/// `strtol(s, end, base)` — signed long.
#[no_mangle]
pub unsafe extern "C" fn strtol(s: *const c_char, end: *mut *mut c_char, base: i32) -> i64 {
    let (v, neg) = parse_uint(s, end, base);
    if neg {
        (v as i64).wrapping_neg()
    } else {
        v as i64
    }
}

/// `atoi(s)` — decimal int, no error reporting (standard).
#[no_mangle]
pub unsafe extern "C" fn atoi(s: *const c_char) -> i32 {
    strtol(s, core::ptr::null_mut(), 10) as i32
}

/// `abort()` — abnormal termination. Frame OS has no signals; exit(134) mirrors
/// the conventional 128 + SIGABRT(6) status. Never returns.
#[no_mangle]
pub extern "C" fn abort() -> ! {
    exit(134)
}

/// `getenv(name)` — Frame OS has no environment yet, so always "not set".
#[no_mangle]
pub extern "C" fn getenv(_name: *const c_char) -> *mut c_char {
    core::ptr::null_mut()
}

/// `qsort(base, n, size, cmp)` — sort `n` elements of `size` bytes in place.
/// Shellsort (Ciura gaps): no recursion (tcc's user stack is small) and no
/// temp allocation, swapping elements byte-wise.
#[no_mangle]
pub unsafe extern "C" fn qsort(
    base: *mut u8,
    n: usize,
    size: usize,
    cmp: extern "C" fn(*const u8, *const u8) -> i32,
) {
    if n < 2 || size == 0 {
        return;
    }
    let elem = |i: usize| base.add(i * size);
    let swap = |i: usize, j: usize| {
        let (a, b) = (elem(i), elem(j));
        for k in 0..size {
            core::ptr::swap(a.add(k), b.add(k));
        }
    };
    const GAPS: [usize; 8] = [701, 301, 132, 57, 23, 10, 4, 1];
    for &gap in GAPS.iter() {
        if gap >= n {
            continue;
        }
        let mut i = gap;
        while i < n {
            let mut j = i;
            while j >= gap && cmp(elem(j - gap), elem(j)) > 0 {
                swap(j - gap, j);
                j -= gap;
            }
            i += 1;
        }
    }
}

// --- sscanf (B11-3c) -------------------------------------------------------
//
// All sscanf args are *pointers* (where to store results), which travel in the
// general-purpose registers/stack, so no SSE spill is needed — even `%f` stores
// through a `float*`. A naked trampoline spills the 4 GP vararg regs (rdx,rcx,
// r8,r9; str/fmt in rdi/rsi); the impl walks the format, matching whitespace +
// literals and converting `%d`/`%u`/`%x`/`%c`/`%s`. (tcc uses only `%d`.)

struct ArgPtrs {
    gp: *const u64,
    gp_idx: usize,
    gp_max: usize,
    ov: *const u64,
    ov_idx: usize,
}
impl ArgPtrs {
    unsafe fn next(&mut self) -> *mut u8 {
        let v = if self.gp_idx < self.gp_max {
            let v = *self.gp.add(self.gp_idx);
            self.gp_idx += 1;
            v
        } else {
            let v = *self.ov.add(self.ov_idx);
            self.ov_idx += 1;
            v
        };
        v as *mut u8
    }
}

unsafe fn skip_ws(s: *const c_char, i: &mut usize) {
    while is_space(byte_at(s, *i)) {
        *i += 1;
    }
}

extern "C" fn vsscanf_impl(
    s: *const c_char,
    fmt: *const c_char,
    gp: *const u64,
    ov: *const u64,
) -> i32 {
    let mut args = ArgPtrs {
        gp,
        gp_idx: 0,
        gp_max: 4,
        ov,
        ov_idx: 0,
    };
    let mut count = 0i32;
    let mut si = 0usize; // cursor in the input string
    let mut fi = 0usize; // cursor in the format
    unsafe {
        loop {
            let fc = byte_at(fmt, fi);
            if fc == 0 {
                break;
            }
            if is_space(fc) {
                fi += 1;
                skip_ws(s, &mut si);
                continue;
            }
            if fc != b'%' {
                // Literal: must match the input.
                if byte_at(s, si) != fc {
                    break;
                }
                si += 1;
                fi += 1;
                continue;
            }
            // Conversion.
            fi += 1;
            let conv = byte_at(fmt, fi);
            fi += 1;
            match conv {
                b'd' | b'u' | b'x' => {
                    skip_ws(s, &mut si);
                    let base = if conv == b'x' { 16 } else { 10 };
                    let mut neg = false;
                    if conv == b'd' && (byte_at(s, si) == b'+' || byte_at(s, si) == b'-') {
                        neg = byte_at(s, si) == b'-';
                        si += 1;
                    }
                    let mut val: u64 = 0;
                    let mut any = false;
                    loop {
                        match digit_val(byte_at(s, si)) {
                            Some(d) if (d as i32) < base => {
                                val = val.wrapping_mul(base as u64).wrapping_add(d as u64);
                                any = true;
                                si += 1;
                            }
                            _ => break,
                        }
                    }
                    if !any {
                        break;
                    }
                    let p = args.next() as *mut i32;
                    *p = if neg { (val as i64).wrapping_neg() as i32 } else { val as i32 };
                    count += 1;
                }
                b'c' => {
                    let c = byte_at(s, si);
                    if c == 0 {
                        break;
                    }
                    si += 1;
                    let p = args.next();
                    *p = c;
                    count += 1;
                }
                b's' => {
                    skip_ws(s, &mut si);
                    let p = args.next();
                    let mut j = 0usize;
                    loop {
                        let c = byte_at(s, si);
                        if c == 0 || is_space(c) {
                            break;
                        }
                        *p.add(j) = c;
                        si += 1;
                        j += 1;
                    }
                    if j == 0 {
                        break;
                    }
                    *p.add(j) = 0;
                    count += 1;
                }
                b'%' => {
                    if byte_at(s, si) != b'%' {
                        break;
                    }
                    si += 1;
                }
                _ => break, // unsupported conversion
            }
        }
    }
    count
}

/// C `sscanf(str, fmt, ...)`. All conversion args are pointers, so only the GP
/// vararg registers need spilling (rdx,rcx,r8,r9; str/fmt in rdi/rsi).
#[no_mangle]
#[unsafe(naked)]
pub unsafe extern "C" fn sscanf(_s: *const c_char, _fmt: *const c_char) -> i32 {
    naked_asm!(
        "push r9", "push r8", "push rcx", "push rdx", // GP area (32)
        "mov rdx, rsp",        // arg3 = GP area [rdx,rcx,r8,r9]
        "lea rcx, [rsp + 40]", // arg4 = overflow (32 pushed + 8 ret)
        "sub rsp, 8",          // 16-align the call
        "call {f}",
        "add rsp, 8",
        "add rsp, 32",
        "ret",
        f = sym vsscanf_impl,
    );
}
