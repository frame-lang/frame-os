//! C `<string.h>` functions tcc needs beyond the compiler-builtin
//! memcpy/memset/memcmp/memmove + the hand-written `strlen` in lib.rs (B11-3c).
//! Plain byte mechanics — no lifecycle, no Frame system. NUL-terminated input
//! is the caller's contract (the C ABI); these mirror the standard semantics.

use core::ffi::c_char;

#[inline]
unsafe fn cstr_len(mut s: *const c_char) -> usize {
    let mut n = 0;
    while *s != 0 {
        s = s.add(1);
        n += 1;
    }
    n
}

/// `strcmp(a, b)` — sign of the first differing byte (unsigned), 0 if equal.
#[no_mangle]
pub unsafe extern "C" fn strcmp(a: *const c_char, b: *const c_char) -> i32 {
    let mut i = 0;
    loop {
        let ca = *a.add(i) as u8;
        let cb = *b.add(i) as u8;
        if ca != cb {
            return ca as i32 - cb as i32;
        }
        if ca == 0 {
            return 0;
        }
        i += 1;
    }
}

/// `strncmp(a, b, n)` — like strcmp but compares at most `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn strncmp(a: *const c_char, b: *const c_char, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        let ca = *a.add(i) as u8;
        let cb = *b.add(i) as u8;
        if ca != cb {
            return ca as i32 - cb as i32;
        }
        if ca == 0 {
            return 0;
        }
        i += 1;
    }
    0
}

/// `strcpy(dst, src)` — copy `src` (incl. its NUL) into `dst`; returns `dst`.
#[no_mangle]
pub unsafe extern "C" fn strcpy(dst: *mut c_char, src: *const c_char) -> *mut c_char {
    let mut i = 0;
    loop {
        let c = *src.add(i);
        *dst.add(i) = c;
        if c == 0 {
            break;
        }
        i += 1;
    }
    dst
}

/// `strncpy(dst, src, n)` — copy up to `n` bytes; NUL-pad if `src` is shorter.
#[no_mangle]
pub unsafe extern "C" fn strncpy(dst: *mut c_char, src: *const c_char, n: usize) -> *mut c_char {
    let mut i = 0;
    while i < n && *src.add(i) != 0 {
        *dst.add(i) = *src.add(i);
        i += 1;
    }
    while i < n {
        *dst.add(i) = 0;
        i += 1;
    }
    dst
}

/// `strcat(dst, src)` — append `src` to the end of `dst`; returns `dst`.
#[no_mangle]
pub unsafe extern "C" fn strcat(dst: *mut c_char, src: *const c_char) -> *mut c_char {
    let end = cstr_len(dst);
    strcpy(dst.add(end), src);
    dst
}

/// `strchr(s, c)` — first occurrence of byte `c` (the NUL is matchable), or NULL.
#[no_mangle]
pub unsafe extern "C" fn strchr(s: *const c_char, c: i32) -> *mut c_char {
    let target = c as u8 as c_char;
    let mut i = 0;
    loop {
        let cur = *s.add(i);
        if cur == target {
            return s.add(i) as *mut c_char;
        }
        if cur == 0 {
            return core::ptr::null_mut();
        }
        i += 1;
    }
}

/// `strrchr(s, c)` — last occurrence of byte `c`, or NULL.
#[no_mangle]
pub unsafe extern "C" fn strrchr(s: *const c_char, c: i32) -> *mut c_char {
    let target = c as u8 as c_char;
    let mut found: *mut c_char = core::ptr::null_mut();
    let mut i = 0;
    loop {
        let cur = *s.add(i);
        if cur == target {
            found = s.add(i) as *mut c_char;
        }
        if cur == 0 {
            return found;
        }
        i += 1;
    }
}

/// `strstr(haystack, needle)` — first occurrence of `needle`, or NULL. An empty
/// needle matches at the start (standard).
#[no_mangle]
pub unsafe extern "C" fn strstr(haystack: *const c_char, needle: *const c_char) -> *mut c_char {
    let nlen = cstr_len(needle);
    if nlen == 0 {
        return haystack as *mut c_char;
    }
    let hlen = cstr_len(haystack);
    if nlen > hlen {
        return core::ptr::null_mut();
    }
    let mut i = 0;
    while i + nlen <= hlen {
        let mut j = 0;
        while j < nlen && *haystack.add(i + j) == *needle.add(j) {
            j += 1;
        }
        if j == nlen {
            return haystack.add(i) as *mut c_char;
        }
        i += 1;
    }
    core::ptr::null_mut()
}
