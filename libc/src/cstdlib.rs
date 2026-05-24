//! C `<stdlib.h>` functions tcc needs beyond malloc/free/calloc/realloc/exit
//! (B11-3c): integer parsing (atoi, strtol/strtoul/strtoull), abort, getenv,
//! and qsort. Plain mechanics — no Frame system.

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
