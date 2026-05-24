//! POSIX/`<unistd.h>`/`<fcntl.h>` surface tcc needs (B11-3c): the fd-I/O wrappers
//! over the Frame OS syscalls (open/read/close/lseek/unlink), `errno`, and the
//! real wall-clock surface (time/gettimeofday/localtime) backed by the kernel's
//! CMOS RTC (syscall #18) — tcc reads these while preprocessing `__DATE__`/
//! `__TIME__`. The remaining entries are stubs for features Frame OS doesn't
//! have yet (execvp/mprotect): they only need to *link* — tcc's compile-to-file
//! path never calls them (they belong to the `-run` JIT).

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

// --- time (real wall clock via the CMOS RTC, syscall #18) ------------------

/// `time(t)` — current Unix epoch seconds (UTC) from the kernel's CMOS RTC. If
/// `t` is non-NULL the value is also stored through it. tcc calls this (and
/// `localtime`) while preprocessing `__DATE__`/`__TIME__`, so a compiled
/// program now carries the real build date.
#[no_mangle]
pub unsafe extern "C" fn time(t: *mut i64) -> i64 {
    let now = crate::sys_time() as i64;
    if !t.is_null() {
        *t = now;
    }
    now
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

/// Inverse of the kernel's `days_from_civil`: days-since-epoch → (year, month
/// [1,12], day [1,31]) in the proleptic Gregorian calendar (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11] (Mar=0 … Feb=11)
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days since 1970-01-01 for a civil date (mirrors the kernel's helper); used to
/// derive `tm_yday`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// `localtime` returns a pointer into a single static buffer (the C contract —
// callers must copy before the next call). frame-libc is single-threaded per
// process, so a plain static matches the standard.
static mut LOCALTIME_BUF: Tm = Tm {
    tm_sec: 0,
    tm_min: 0,
    tm_hour: 0,
    tm_mday: 1,
    tm_mon: 0,
    tm_year: 70,
    tm_wday: 0,
    tm_yday: 0,
    tm_isdst: 0,
};

/// `localtime(t)` — break `*t` (Unix epoch seconds) into a `struct tm`. Frame OS
/// has no timezone, so this is effectively `gmtime`: the RTC is treated as UTC.
#[no_mangle]
pub unsafe extern "C" fn localtime(t: *const i64) -> *mut Tm {
    let secs = if t.is_null() { 0 } else { *t };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, mon, mday) = civil_from_days(days);
    let yday = days - days_from_civil(year, 1, 1);
    // 1970-01-01 was a Thursday (wday 4); wday is 0=Sunday.
    let wday = (days + 4).rem_euclid(7);

    let tm = &raw mut LOCALTIME_BUF;
    (*tm).tm_sec = (rem % 60) as i32;
    (*tm).tm_min = ((rem / 60) % 60) as i32;
    (*tm).tm_hour = (rem / 3600) as i32;
    (*tm).tm_mday = mday as i32;
    (*tm).tm_mon = (mon - 1) as i32;
    (*tm).tm_year = (year - 1900) as i32;
    (*tm).tm_wday = wday as i32;
    (*tm).tm_yday = yday as i32;
    (*tm).tm_isdst = 0;
    tm
}

#[repr(C)]
pub struct Timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// `gettimeofday(tv, tz)` — fills `tv` with the RTC time (whole seconds; the RTC
/// has no sub-second resolution, so `tv_usec` is 0). Returns 0.
#[no_mangle]
pub unsafe extern "C" fn gettimeofday(tv: *mut Timeval, _tz: *mut core::ffi::c_void) -> i32 {
    if !tv.is_null() {
        (*tv).tv_sec = crate::sys_time() as i64;
        (*tv).tv_usec = 0;
    }
    0
}
