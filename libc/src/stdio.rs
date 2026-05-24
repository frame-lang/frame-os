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
use crate::{strlen, sys_close, sys_open, sys_read, sys_read_line, vformat, write};

/// A buffered stream. The C ABI passes `*mut FILE`.
pub struct FileStream {
    fd: i32,
    of: OpenFile, // mode gate (reused kernel FSM)
    eof: bool,
    err: bool,
    obuf: Vec<u8>,  // pending output
    ibuf: Vec<u8>,  // input buffer (drained by fread/fgetc/fgets)
    ipos: usize,    // read cursor within ibuf
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

    /// Refill the input buffer. fd 0 (console stdin) blocks for a whole line via
    /// `read_line`; other fds read a chunk via the file `read` syscall. Returns
    /// false (and sets eof) when there is no more input.
    fn refill(&mut self) -> bool {
        self.ibuf.clear();
        self.ipos = 0;
        let mut tmp = [0u8; 512];
        let n = if self.fd == 0 {
            sys_read_line(&mut tmp)
        } else {
            sys_read(self.fd, &mut tmp)
        };
        if n == 0 {
            self.eof = true;
            return false;
        }
        self.ibuf.extend_from_slice(&tmp[..n]);
        true
    }

    /// Next input byte, or None at EOF.
    fn next_byte(&mut self) -> Option<u8> {
        if self.ipos >= self.ibuf.len() && !self.refill() {
            return None;
        }
        let b = self.ibuf[self.ipos];
        self.ipos += 1;
        Some(b)
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
        ibuf: Vec::new(),
        ipos: 0,
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
    let mut got = 0;
    while got < total {
        if s.ipos >= s.ibuf.len() && !s.refill() {
            break;
        }
        let take = (total - got).min(s.ibuf.len() - s.ipos);
        dst[got..got + take].copy_from_slice(&s.ibuf[s.ipos..s.ipos + take]);
        s.ipos += take;
        got += take;
    }
    got / size
}

/// `fgetc(f)` — next byte as an `int`, or EOF (-1). 0 if not open for reading.
#[no_mangle]
pub unsafe extern "C" fn fgetc(f: *mut FILE) -> i32 {
    if f.is_null() {
        return -1;
    }
    let s = &mut *f;
    s.of.read();
    if !s.of.is_reading() {
        return -1;
    }
    match s.next_byte() {
        Some(b) => b as i32,
        None => -1,
    }
}

/// `fgets(s, n, f)` — read at most `n-1` bytes, stopping after a newline (kept),
/// and NUL-terminate. Returns `s`, or NULL if EOF with nothing read.
#[no_mangle]
pub unsafe extern "C" fn fgets(out: *mut u8, n: i32, f: *mut FILE) -> *mut u8 {
    if out.is_null() || n <= 0 || f.is_null() {
        return core::ptr::null_mut();
    }
    let s = &mut *f;
    s.of.read();
    if !s.of.is_reading() {
        return core::ptr::null_mut();
    }
    let cap = (n - 1) as usize;
    let mut i = 0;
    while i < cap {
        match s.next_byte() {
            Some(b) => {
                *out.add(i) = b;
                i += 1;
                if b == b'\n' {
                    break;
                }
            }
            None => break, // EOF
        }
    }
    if i == 0 {
        return core::ptr::null_mut(); // EOF, nothing read
    }
    *out.add(i) = 0;
    out
}

/// `getchar()` — next byte from stdin, or EOF.
pub fn getchar() -> i32 {
    unsafe { fgetc(stdin()) }
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

/// `puts(s)` — write a string + a trailing newline to stdout (POSIX). Returns a
/// non-negative value on success, EOF (-1) on failure. (gcc lowers a bare
/// `printf("...\n")` to `puts`, so a C program needs this even if it never
/// writes `puts` itself.)
#[no_mangle]
pub unsafe extern "C" fn puts(s: *const u8) -> i32 {
    let f = stdout();
    if fputs(s, f) < 0 {
        return -1;
    }
    fputc(b'\n' as i32, f)
}

/// `putchar(c)` — write one byte to stdout; returns it or EOF.
#[no_mangle]
pub unsafe extern "C" fn putchar(c: i32) -> i32 {
    unsafe { fputc(c, stdout()) }
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

// Standard streams (console-backed). Lazily created; single-threaded. stdin
// (fd 0) is a read stream that refills via `read_line`; stdout/stderr (fd 1/2)
// are write streams flushed per write.
static mut STDIN: Option<FileStream> = None;
static mut STDOUT: Option<FileStream> = None;
static mut STDERR: Option<FileStream> = None;

unsafe fn console_stream(slot: *mut Option<FileStream>, fd: i32, write_mode: bool) -> *mut FILE {
    if (*slot).is_none() {
        let mut of = OpenFile::__create();
        if write_mode {
            of.open_write();
        } else {
            of.open_read();
        }
        *slot = Some(FileStream {
            fd,
            of,
            eof: false,
            err: false,
            obuf: Vec::new(),
            ibuf: Vec::new(),
            ipos: 0,
        });
    }
    (*slot).as_mut().unwrap() as *mut FILE
}

/// The standard input stream (the console; reads block for a line).
pub fn stdin() -> *mut FILE {
    unsafe { console_stream(&raw mut STDIN, 0, false) }
}

/// The standard output stream (the console).
pub fn stdout() -> *mut FILE {
    unsafe { console_stream(&raw mut STDOUT, 1, true) }
}

/// The standard error stream (the console).
pub fn stderr() -> *mut FILE {
    unsafe { console_stream(&raw mut STDERR, 2, true) }
}

// C exposes `stdin`/`stdout`/`stderr` as FILE* lvalues (e.g. `fprintf(stderr,
// ...)`), so frame-libc provides them as globals — distinct from the Rust
// `stdin()`/`stdout()`/`stderr()` accessors above (which Rust callers + the
// internal puts/putchar use). `#[export_name]` gives the C symbol while keeping
// a non-clashing Rust identifier. Filled by `init_std_streams` from crt0 before
// `main`, so the pointers are valid the first time C reads them. (B11-3c)
#[export_name = "stdin"]
pub static mut STDIN_PTR: *mut FILE = core::ptr::null_mut();
#[export_name = "stdout"]
pub static mut STDOUT_PTR: *mut FILE = core::ptr::null_mut();
#[export_name = "stderr"]
pub static mut STDERR_PTR: *mut FILE = core::ptr::null_mut();

/// Initialize the C `stdin`/`stdout`/`stderr` globals. Called once from crt0
/// (`__libc_start`) before `main`.
pub fn init_std_streams() {
    unsafe {
        (&raw mut STDIN_PTR).write(stdin());
        (&raw mut STDOUT_PTR).write(stdout());
        (&raw mut STDERR_PTR).write(stderr());
    }
}

/// `fseek(f, offset, whence)` — flush pending output, discard the read buffer,
/// and `lseek` the underlying fd. Returns 0, or -1 on error.
#[no_mangle]
pub unsafe extern "C" fn fseek(f: *mut FILE, offset: i64, whence: i32) -> i32 {
    if f.is_null() {
        return -1;
    }
    let s = &mut *f;
    s.flush();
    s.ibuf.clear();
    s.ipos = 0;
    s.eof = false;
    if crate::sys_lseek(s.fd, offset, whence) == u64::MAX {
        -1
    } else {
        0
    }
}

/// `ftell(f)` — current file offset: the fd's offset (after flushing pending
/// output), minus any buffered-but-unread input. Returns -1 on error.
#[no_mangle]
pub unsafe extern "C" fn ftell(f: *mut FILE) -> i64 {
    if f.is_null() {
        return -1;
    }
    let s = &mut *f;
    s.flush();
    let pos = crate::sys_lseek(s.fd, 0, 1 /* SEEK_CUR */);
    if pos == u64::MAX {
        return -1;
    }
    pos as i64 - (s.ibuf.len() - s.ipos) as i64
}

/// `fdopen(fd, mode)` — wrap an already-open fd in a buffered `FILE` (the mode
/// only sets the read/write gate; the fd is used as-is). Returns NULL on error.
#[no_mangle]
pub unsafe extern "C" fn fdopen(fd: i32, mode: *const u8) -> *mut FILE {
    if mode.is_null() {
        return core::ptr::null_mut();
    }
    let write_mode = *mode == b'w' || *mode == b'a';
    let mut of = OpenFile::__create();
    if write_mode {
        of.open_write();
    } else {
        of.open_read();
    }
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(FileStream {
        fd,
        of,
        eof: false,
        err: false,
        obuf: Vec::new(),
        ibuf: Vec::new(),
        ipos: 0,
    }))
}

/// `fprintf` into a stream (B10-3b). The Rust-friendly front end alongside the
/// C-ABI variadic `fprintf` below.
pub fn fprintf_args(f: *mut FILE, fmt: &str, args: &[Arg]) {
    let bytes = vformat(fmt, args);
    unsafe { fwrite(bytes.as_ptr(), 1, bytes.len(), f) };
}

/// Rust target of the `fprintf` trampoline: `rdi` = stream, `rsi` = fmt,
/// `rdx` = saved-reg area (rdx,rcx,r8,r9), `rcx` = stack overflow.
extern "C" fn vfprintf_impl(
    f: *mut FILE,
    fmt: *const u8,
    regs: *const u64,
    overflow: *const u64,
) -> i32 {
    let mut va = crate::printf::VaArgs::new(regs, 4, overflow);
    let bytes = crate::printf::vformat_va(fmt, &mut va);
    unsafe { fwrite(bytes.as_ptr(), 1, bytes.len(), f) };
    bytes.len() as i32
}

/// C `fprintf(FILE *f, const char *fmt, ...)`. Naked: stream stays in rdi and
/// fmt in rsi; spill the 4 vararg integer registers (rdx,rcx,r8,r9). Four pushes
/// from a post-`call` rsp leave it ≡8 mod 16, so one extra 8-byte pad aligns the
/// inner call (B11-1).
#[no_mangle]
#[unsafe(naked)]
pub unsafe extern "C" fn fprintf(_f: *mut FILE, _fmt: *const u8) -> i32 {
    core::arch::naked_asm!(
        "push r9",
        "push r8",
        "push rcx",
        "push rdx",
        "mov rdx, rsp",        // arg2 = saved-reg area [rdx,rcx,r8,r9]
        "lea rcx, [rsp + 40]", // arg3 = overflow (32 pushed + 8 return addr)
        "sub rsp, 8",          // 16-align the call
        "call {f}",
        "add rsp, 8",
        "add rsp, 32",         // pop the 4 saved regs
        "ret",
        f = sym vfprintf_impl,
    );
}
