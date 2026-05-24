//! POSIX/`<unistd.h>`/`<fcntl.h>` surface tcc needs (B11-3c): the fd-I/O wrappers
//! over the Frame OS syscalls (open/read/close/lseek), plus `errno` and the
//! handful of stubs for features Frame OS doesn't have yet (unlink/execvp/
//! gettimeofday/localtime/time/mprotect). The stubs only need to *link* — tcc's
//! compile-to-file path never calls them (they belong to temp-file cleanup, the
//! `-run` JIT, and `__DATE__`/`__TIME__`, none of which the demo exercises).

use core::ffi::c_char;

use crate::{sys_close, sys_lseek, sys_open, sys_read, sys_unlink};

/// The single global `errno`. frame-libc is single-threaded per process, so a
/// plain global matches the `extern int errno;` the header declares.
#[no_mangle]
pub static mut errno: i32 = 0;

// fcntl flag bits (must match libc/include/fcntl.h).
const O_WRONLY: i32 = 1;
const O_RDWR: i32 = 2;
const O_CREAT: i32 = 0o100;

#[inline]
unsafe fn cstr_len(mut s: *const c_char) -> usize {
    let mut n = 0;
    while *s != 0 {
        s = s.add(1);
        n += 1;
    }
    n
}

/// `open(path, flags, ...)` — declared variadic in C (an optional mode); the
/// mode lands in rdx and is simply ignored here (Frame OS has no permissions).
/// Maps the access mode + O_CREAT to the kernel's write flag.
#[no_mangle]
pub unsafe extern "C" fn open(path: *const c_char, flags: i32) -> i32 {
    let len = cstr_len(path);
    let bytes = core::slice::from_raw_parts(path as *const u8, len);
    let write = (flags & (O_WRONLY | O_RDWR)) != 0 || (flags & O_CREAT) != 0;
    match sys_open(bytes, write) {
        Some(fd) => fd,
        None => -1,
    }
}

/// `read(fd, buf, count)` → bytes read (0 = EOF), -1 on error.
#[no_mangle]
pub unsafe extern "C" fn read(fd: i32, buf: *mut u8, count: usize) -> isize {
    let s = core::slice::from_raw_parts_mut(buf, count);
    sys_read(fd, s) as isize
}

/// `close(fd)` → 0.
#[no_mangle]
pub extern "C" fn close(fd: i32) -> i32 {
    sys_close(fd);
    0
}

/// `lseek(fd, offset, whence)` → new offset, or -1 on error.
#[no_mangle]
pub extern "C" fn lseek(fd: i32, offset: i64, whence: i32) -> i64 {
    let r = sys_lseek(fd, offset, whence);
    if r == u64::MAX {
        -1
    } else {
        r as i64
    }
}

/// `unlink(path)` — delete a file via the kernel (syscall #17, B11-3 follow-up).
/// Returns 0 on success, -1 if the path doesn't resolve to a regular file.
#[no_mangle]
pub unsafe extern "C" fn unlink(path: *const c_char) -> i32 {
    let len = cstr_len(path);
    let bytes = core::slice::from_raw_parts(path as *const u8, len);
    if sys_unlink(bytes) == u64::MAX {
        -1
    } else {
        0
    }
}

/// `remove(path)` — same as unlink for regular files.
#[no_mangle]
pub unsafe extern "C" fn remove(path: *const c_char) -> i32 {
    unlink(path)
}

/// `getcwd(buf, size)` — Frame OS has no per-process cwd; report root "/".
#[no_mangle]
pub unsafe extern "C" fn getcwd(buf: *mut c_char, size: usize) -> *mut c_char {
    if buf.is_null() || size < 2 {
        return core::ptr::null_mut();
    }
    *buf = b'/' as c_char;
    *buf.add(1) = 0;
    buf
}

/// `execvp(file, argv)` — Frame OS's tcc is self-contained (no external `as`/
/// `ld`); never called on the compile-to-file path. Stub: report failure.
#[no_mangle]
pub unsafe extern "C" fn execvp(_file: *const c_char, _argv: *const *const c_char) -> i32 {
    -1
}

/// `mprotect(addr, len, prot)` — only the unused `-run` JIT calls it. Stub OK.
#[no_mangle]
pub extern "C" fn mprotect(_addr: *mut u8, _len: usize, _prot: i32) -> i32 {
    0
}

// --- time (fixed; Frame OS has no wall clock) ------------------------------

const FIXED_TIME: i64 = 0;

/// `time(t)` — returns a fixed epoch (used only for `__DATE__`/`__TIME__`).
#[no_mangle]
pub unsafe extern "C" fn time(t: *mut i64) -> i64 {
    if !t.is_null() {
        *t = FIXED_TIME;
    }
    FIXED_TIME
}

#[repr(C)]
pub struct Tm {
    pub tm_sec: i32,
    pub tm_min: i32,
    pub tm_hour: i32,
    pub tm_mday: i32,
    pub tm_mon: i32,
    pub tm_year: i32,
    pub tm_wday: i32,
    pub tm_yday: i32,
    pub tm_isdst: i32,
}

static mut FIXED_TM: Tm = Tm {
    tm_sec: 0,
    tm_min: 0,
    tm_hour: 0,
    tm_mday: 1,
    tm_mon: 0,
    tm_year: 126, // 2026 - 1900
    tm_wday: 0,
    tm_yday: 0,
    tm_isdst: 0,
};

/// `localtime(t)` — returns a pointer to a fixed broken-down time (2026-01-01).
#[no_mangle]
pub unsafe extern "C" fn localtime(_t: *const i64) -> *mut Tm {
    &raw mut FIXED_TM
}

#[repr(C)]
pub struct Timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// `gettimeofday(tv, tz)` — fills a fixed time; returns 0.
#[no_mangle]
pub unsafe extern "C" fn gettimeofday(tv: *mut Timeval, _tz: *mut core::ffi::c_void) -> i32 {
    if !tv.is_null() {
        (*tv).tv_sec = FIXED_TIME;
        (*tv).tv_usec = 0;
    }
    0
}
