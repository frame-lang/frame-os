//! frame-libc buffered streams: `FILE*` (B10-3b).
//!
//! A `FILE` is a buffered wrapper over a kernel fd. Its read/write *mode* is
//! gated by the **reused `OpenFile` Frame system** — the same FSM the kernel's
//! VFS uses (`frame/open_file.frs`), now driving userspace stream modes too
//! (one source, two targets). The end-of-file / error *status* is two sticky
//! native flags (`feof`/`ferror` query them, `clearerr` resets them): a fixed
//! property plus two booleans, not a lifecycle, so it stays native — the FSM
//! owns the dimension that actually has transitions (the mode gate).
//!
//! The non-variadic stdio entry points are real `extern "C"` (`fopen`, `fwrite`,
//! `fread`, `fputs`, `fputc`, `fflush`, `fclose`, `feof`, `ferror`, `clearerr`).
//! Only the variadic `fprintf(f, fmt, ...)` waits for B11; until then a
//! Rust-friendly `fprintf_args` drives the printf engine into a stream.

use alloc::vec::Vec;

use crate::frame_systems::OpenFile;
use crate::printf::Arg;
use crate::{strlen, sys_close, sys_open, sys_read, vformat, write};

/// A buffered stream. The C ABI passes `*mut FILE`.
pub struct FileStream {
    fd: i32,
    of: OpenFile, // mode gate (reused kernel FSM)
    eof: bool,
    err: bool,
    obuf: Vec<u8>, // pending output
}

/// C's opaque `FILE`.
pub type FILE = FileStream;

const FLUSH_AT: usize = 4096;

impl FileStream {
    fn flush(&mut self) {
        if !self.obuf.is_empty() {
            write(self.fd, &self.obuf);
            self.obuf.clear();
        }
    }
}

/// `fopen(path, mode)` — open `path` for "r" (read) or "w" (write; create +
/// truncate). Returns a `*mut FILE`, or NULL on failure. Only the first mode
/// char is consulted ("rb"/"wb" behave like "r"/"w").
#[no_mangle]
pub unsafe extern "C" fn fopen(path: *const u8, mode: *const u8) -> *mut FILE {
    if path.is_null() || mode.is_null() {
        return core::ptr::null_mut();
    }
    let write_mode = *mode == b'w' || *mode == b'a';
    let path_slice = core::slice::from_raw_parts(path, strlen(path));
    let Some(fd) = sys_open(path_slice, write_mode) else {
        return core::ptr::null_mut();
    };
    let mut of = OpenFile::__create();
    if write_mode {
        of.open_write();
    } else {
        of.open_read();
    }
    let f = alloc::boxed::Box::new(FileStream {
        fd,
        of,
        eof: false,
        err: false,
        obuf: Vec::new(),
    });
    alloc::boxed::Box::into_raw(f)
}

/// `fwrite(ptr, size, nmemb, f)` — buffer `size*nmemb` bytes; returns items
/// written (0 if the stream isn't open for writing — the `OpenFile` gate). The
/// buffer flushes when it grows past `FLUSH_AT` or when `f` is the console.
#[no_mangle]
pub unsafe extern "C" fn fwrite(ptr: *const u8, size: usize, nmemb: usize, f: *mut FILE) -> usize {
    if f.is_null() {
        return 0;
    }
    let s = &mut *f;
    s.of.write(); // gated: no-op unless $Writing
    if !s.of.is_writing() {
        return 0;
    }
    let total = size.saturating_mul(nmemb);
    s.obuf.extend_from_slice(core::slice::from_raw_parts(ptr, total));
    if s.fd <= 2 || s.obuf.len() >= FLUSH_AT {
        s.flush();
    }
    nmemb
}

/// `fread(ptr, size, nmemb, f)` — read up to `size*nmemb` bytes; returns items
/// read (short / 0 at EOF, which sets the eof indicator). 0 if not open for
/// reading.
#[no_mangle]
pub unsafe extern "C" fn fread(ptr: *mut u8, size: usize, nmemb: usize, f: *mut FILE) -> usize {
    if f.is_null() || size == 0 {
        return 0;
    }
    let s = &mut *f;
    s.of.read(); // gated: no-op unless $Reading
    if !s.of.is_reading() {
        return 0;
    }
    let total = size.saturating_mul(nmemb);
    let dst = core::slice::from_raw_parts_mut(ptr, total);
    let n = sys_read(s.fd, dst);
    if n == 0 {
        s.eof = true;
    }
    n / size
}

/// `fputs(str, f)` — write a NUL-terminated string. Returns 0 on success, EOF
/// (-1) on failure.
#[no_mangle]
pub unsafe extern "C" fn fputs(str: *const u8, f: *mut FILE) -> i32 {
    if str.is_null() {
        return -1;
    }
    let len = strlen(str);
    if fwrite(str, 1, len, f) == len {
        0
    } else {
        -1
    }
}

/// `fputc(c, f)` — write one byte; returns it, or EOF (-1) on failure.
#[no_mangle]
pub unsafe extern "C" fn fputc(c: i32, f: *mut FILE) -> i32 {
    let b = c as u8;
    if fwrite(&b as *const u8, 1, 1, f) == 1 {
        c & 0xff
    } else {
        -1
    }
}

/// `fflush(f)` — write any buffered output to the fd. Returns 0.
#[no_mangle]
pub unsafe extern "C" fn fflush(f: *mut FILE) -> i32 {
    if !f.is_null() {
        (*f).flush();
    }
    0
}

/// `fclose(f)` — flush, close the fd, and free the stream. Returns 0.
#[no_mangle]
pub unsafe extern "C" fn fclose(f: *mut FILE) -> i32 {
    if f.is_null() {
        return -1;
    }
    let mut s = alloc::boxed::Box::from_raw(f);
    s.flush();
    s.of.close();
    if s.fd > 2 {
        sys_close(s.fd); // 0/1/2 are the console, not VFS fds
    }
    0
}

/// `feof(f)` — non-zero once a read has hit end of file.
#[no_mangle]
pub unsafe extern "C" fn feof(f: *mut FILE) -> i32 {
    if !f.is_null() && (*f).eof {
        1
    } else {
        0
    }
}

/// `ferror(f)` — non-zero if the stream's error indicator is set.
#[no_mangle]
pub unsafe extern "C" fn ferror(f: *mut FILE) -> i32 {
    if !f.is_null() && (*f).err {
        1
    } else {
        0
    }
}

/// `clearerr(f)` — reset the eof + error indicators.
#[no_mangle]
pub unsafe extern "C" fn clearerr(f: *mut FILE) {
    if !f.is_null() {
        (*f).eof = false;
        (*f).err = false;
    }
}

// Standard streams (console-backed: fd 1/2). Lazily created; single-threaded.
static mut STDOUT: Option<FileStream> = None;
static mut STDERR: Option<FileStream> = None;

unsafe fn console_stream(slot: *mut Option<FileStream>, fd: i32) -> *mut FILE {
    if (*slot).is_none() {
        let mut of = OpenFile::__create();
        of.open_write();
        *slot = Some(FileStream {
            fd,
            of,
            eof: false,
            err: false,
            obuf: Vec::new(),
        });
    }
    (*slot).as_mut().unwrap() as *mut FILE
}

/// The standard output stream (the console).
pub fn stdout() -> *mut FILE {
    unsafe { console_stream(&raw mut STDOUT, 1) }
}

/// The standard error stream (the console).
pub fn stderr() -> *mut FILE {
    unsafe { console_stream(&raw mut STDERR, 2) }
}

/// `fprintf` into a stream (B10-3b). The Rust-friendly front end driving the
/// printf engine; the C-variadic `fprintf(f, fmt, ...)` lands at B11 with tcc.
pub fn fprintf_args(f: *mut FILE, fmt: &str, args: &[Arg]) {
    let bytes = vformat(fmt, args);
    unsafe { fwrite(bytes.as_ptr(), 1, bytes.len(), f) };
}
