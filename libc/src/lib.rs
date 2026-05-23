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

use core::arch::{asm, global_asm};

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

/// Terminate the process with status `code` (POSIX `_exit`). Never returns.
pub fn exit(code: i32) -> ! {
    unsafe { syscall3(1, code as u64, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// Length of a NUL-terminated C string (POSIX `strlen`).
///
/// # Safety
/// `s` must point at a NUL-terminated byte string.
pub unsafe fn strlen(s: *const u8) -> usize {
    let mut n = 0;
    while *s.add(n) != 0 {
        n += 1;
    }
    n
}
