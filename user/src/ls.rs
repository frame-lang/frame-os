// Frame OS user program "ls" (S1) — list a directory on the Frame OS
// filesystem. The kernel's readdir syscall (#21) fills a buffer with the
// directory's entry names (NUL-separated); we print one per line. With no
// argument it lists the current directory (".", which the kernel resolves
// against the process cwd); otherwise argv[1] is the directory path.
//
// Like argtest, `_start` is an asm shim handing the SysV initial stack to
// `ls_main` so we can read argv. Disk-only: the shell runs it as `/bin/ls`.
//
// Syscall ABI: 1 = exit, 12 = write(fd, buf, len), 21 = readdir(path, len,
//              buf, buflen) → bytes of NUL-separated names, or u64::MAX.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

// At process start `rsp` points at argc (16-aligned). Hand it to `ls_main`.
global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp",
    "  and rsp, -16",
    "  call ls_main",
    "  ud2",
);

#[inline(always)]
unsafe fn syscall3(num: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
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

#[inline(always)]
unsafe fn syscall4(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => ret,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        in("r10") a3, // 4th syscall arg goes in r10 (rcx is clobbered by `syscall`)
        out("rcx") _,
        out("r11") _,
        options(nostack),
    );
    ret
}

/// Write `buf` to fd in one syscall (#12) — fd 1 is the console, emitted as an
/// atomic line so concurrent processes can't split it.
fn write(fd: u64, buf: &[u8]) {
    unsafe { syscall3(12, fd, buf.as_ptr() as u64, buf.len() as u64) };
}
fn print(s: &[u8]) {
    write(1, s);
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// readdir(path) → number of bytes of NUL-separated names in `out`, or
/// u64::MAX if `path` isn't a directory.
fn readdir(path: &[u8], out: &mut [u8]) -> u64 {
    unsafe {
        syscall4(
            21,
            path.as_ptr() as u64,
            path.len() as u64,
            out.as_mut_ptr() as u64,
            out.len() as u64,
        )
    }
}

/// Length of the NUL-terminated C string at argv pointer `p`.
fn cstr_len(p: *const u8) -> usize {
    let mut n = 0;
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
    }
    n
}

#[no_mangle]
extern "C" fn ls_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    // Directory to list: argv[1] if given, else "." (the cwd).
    let path: &[u8] = if argc >= 2 {
        let p = unsafe { *sp.add(2) } as *const u8;
        unsafe { core::slice::from_raw_parts(p, cstr_len(p)) }
    } else {
        b"."
    };

    let mut buf = [0u8; 4096];
    let n = readdir(path, &mut buf);
    if n == u64::MAX {
        print(b"ls: cannot read directory: ");
        print(path);
        print(b"\n");
        exit(1);
    }

    // Print each NUL-separated name on its own line.
    let names = &buf[..n as usize];
    for name in names.split(|&c| c == 0) {
        if name.is_empty() {
            continue;
        }
        print(name);
        print(b"\n");
    }
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
