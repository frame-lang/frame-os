//! frame-os-libc (B10): a minimal C/POSIX-ish runtime for Frame OS user
//! programs, built on the syscall ABI (B8/B9). This is the "C side" a future
//! tcc-compiled program (B11) links against; for now Rust user programs use it
//! through the same `extern "C"` surface, exercising exactly the path C will
//! take — crt0 calls `main`, `main` calls libc functions, libc makes syscalls.
//!
//! B10-1: crt0 + syscall thunks + console output + `exit` + `strlen`. Later
//! steps add `malloc` (over `brk`), buffered stdio + `printf` (with the `FILE*`
//! lifecycle + format-spec scanner Frame systems), and file streams.

#![no_std]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::arch::{asm, global_asm};

mod cfloat;
mod cstdlib;
mod cstring;
mod frame_systems;
mod malloc;
mod posix;
mod printf;
mod setjmp;
mod stdio;
pub use malloc::{calloc, free, malloc, realloc};
pub use printf::{print_fmt, vformat, Arg};
pub use stdio::{
    clearerr, fclose, feof, ferror, fflush, fgetc, fgets, fopen, fprintf_args, fputc, fputs, fread,
    fwrite, getchar, putchar, puts, stderr, stdin, stdout, FileStream, FILE,
};

// The Frame-generated code (the printf scanner) and the printf engine use Rust
// `alloc` (Vec/String/Rc/BTreeMap), so frame-libc registers its own global
// allocator — backed by the very `malloc`/`free` above (over `brk`). A program
// that links frame-libc thus gets a working heap for both C `malloc` and Rust
// `alloc`. `malloc` returns 16-aligned blocks, which covers every alignment the
// generated/engine code requests; a larger request would fail loudly (null).
struct LibcAlloc;

unsafe impl GlobalAlloc for LibcAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= 16 {
            malloc(layout.size())
        } else {
            core::ptr::null_mut()
        }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        free(ptr);
    }
}

#[global_allocator]
static GLOBAL: LibcAlloc = LibcAlloc;

// crt0 — the program entry. At process start `rsp` points at the System V
// x86-64 initial stack the kernel built (argc, argv[], NULL, envp[], NULL,
// auxv) — see the kernel's `exec_argv` path (B9-2). Hand that pointer to
// `__libc_start` in rdi, 16-aligning the stack for the SysV call. This is the
// crt0 the `argtest` program hand-rolled in B9-2, now owned by the libc — every
// program that links frame-libc gets a real `_start` for free and just writes
// `main`.
global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp", // arg0 = &argc (the initial stack)
    "  and rsp, -16", // ABI: 16-align before the call
    "  call __libc_start",
    "  ud2", // __libc_start never returns
);

extern "C" {
    /// The program's entry point, C-style. Provided by the linked program.
    fn main(argc: i32, argv: *const *const u8, envp: *const *const u8) -> i32;
}

/// Rust half of crt0: parse the initial stack into `argc`/`argv`/`envp`, call
/// `main`, then `exit` with its return value. Never returns.
///
/// # Safety
/// Called only by the `_start` asm shim with `sp` pointing at a valid SysV
/// initial stack (the kernel guarantees this layout on program entry).
#[no_mangle]
unsafe extern "C" fn __libc_start(sp: *const usize) -> ! {
    let argc = *sp as i32;
    let argv = sp.add(1) as *const *const u8;
    // envp begins just past argv's NULL terminator: sp[1 + argc + 1].
    let envp = sp.add(1 + argc as usize + 1) as *const *const u8;
    // Make the C `stdin`/`stdout`/`stderr` FILE* globals valid before `main`.
    stdio::init_std_streams();
    let code = main(argc, argv, envp);
    exit(code);
}

// --- syscalls -------------------------------------------------------------

#[inline(always)]
unsafe fn syscall3(num: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        inlateout("rax") num => ret,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        out("rcx") _,
        out("r11") _,
        options(nostack),
    );
    ret
}

/// Write `buf` to file descriptor `fd` (POSIX `write`). fd 1 (stdout) and 2
/// (stderr) go to the console — the kernel has no console fds yet, so we route
/// them through `write_char` (syscall #0); any other fd is a filesystem file
/// (syscall #12). Returns the number of bytes written.
pub fn write(fd: i32, buf: &[u8]) -> usize {
    if fd == 1 || fd == 2 {
        for &b in buf {
            unsafe { syscall3(0, b as u64, 0, 0) };
        }
        buf.len()
    } else {
        unsafe { syscall3(12, fd as u64, buf.as_ptr() as u64, buf.len() as u64) as usize }
    }
}

/// Set the program break to `new_end` (0 = query), returning the resulting
/// break (B9-1 syscall #10). The libc's heap (`malloc`) is the sole user of
/// `brk` in a process, so it owns the heap region above `USER_HEAP_BASE`.
pub(crate) fn sys_brk(new_end: u64) -> u64 {
    unsafe { syscall3(10, new_end, 0, 0) }
}

/// open(path, flags) → fd, or None on failure (B9-3 syscall #5; flag bit0 = write).
pub(crate) fn sys_open(path: &[u8], write: bool) -> Option<i32> {
    let r = unsafe {
        syscall3(
            5,
            path.as_ptr() as u64,
            path.len() as u64,
            if write { 1 } else { 0 },
        )
    };
    if r == u64::MAX {
        None
    } else {
        Some(r as i32)
    }
}

/// read(fd, buf) → bytes read, 0 at EOF (syscall #6).
pub(crate) fn sys_read(fd: i32, buf: &mut [u8]) -> usize {
    unsafe { syscall3(6, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) as usize }
}

/// close(fd) (syscall #7).
pub(crate) fn sys_close(fd: i32) {
    unsafe { syscall3(7, fd as u64, 0, 0) };
}

/// lseek(fd, offset, whence) → new offset, or u64::MAX on error (syscall #13).
pub(crate) fn sys_lseek(fd: i32, offset: i64, whence: i32) -> u64 {
    unsafe { syscall3(13, fd as u64, offset as u64, whence as u64) }
}

/// read_line(buf) → bytes read for the console (blocks until a line; B8 syscall
/// #9). Backs `stdin` refills, where the console has no plain readable fd.
pub(crate) fn sys_read_line(buf: &mut [u8]) -> usize {
    unsafe { syscall3(9, buf.as_mut_ptr() as u64, buf.len() as u64, 0) as usize }
}

/// Terminate the process with status `code` (POSIX `exit`). Never returns.
#[no_mangle]
pub extern "C" fn exit(code: i32) -> ! {
    unsafe { syscall3(1, code as u64, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// Length of a NUL-terminated C string (POSIX `strlen`).
///
/// # Safety
/// `s` must point at a NUL-terminated byte string.
#[no_mangle]
pub unsafe extern "C" fn strlen(s: *const u8) -> usize {
    let mut n = 0;
    while *s.add(n) != 0 {
        n += 1;
    }
    n
}

// frame-libc owns the panic handler: a program linking it (a Rust bin like
// `cmain`, or a C program linking the staticlib) gets one without defining its
// own. panic=abort on this target, so this just exits — no unwinding.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    exit(127)
}
