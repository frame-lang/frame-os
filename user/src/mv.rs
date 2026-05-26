// Frame OS user program "mv" (S8) — rename/move. `mv <src> <dst>` re-points the
// file or directory `src` to the name `dst` via the rename syscall (#26); paths
// resolve against the shell's cwd. An existing regular-file `dst` is overwritten;
// an existing directory `dst` fails. Disk-only: the shell runs it as `/bin/mv`.
//
// Syscall ABI: 1 = exit, 12 = write(fd, buf, len),
//              26 = rename(src_ptr, src_len, dst_ptr, dst_len) — 4 args.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp",
    "  and rsp, -16",
    "  call mv_main",
    "  ud2",
);

/// 4-argument syscall: rename needs src+dst (each ptr+len). The 4th arg goes in
/// r10 (the kernel reads it via `arg3`), per the Frame OS syscall ABI.
#[inline(always)]
unsafe fn syscall4(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => ret,
        in("rdi") a0, in("rsi") a1, in("rdx") a2, in("r10") a3,
        out("rcx") _, out("r11") _, options(nostack),
    );
    ret
}
#[inline(always)]
unsafe fn syscall3(num: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => ret,
        in("rdi") a0, in("rsi") a1, in("rdx") a2,
        out("rcx") _, out("r11") _, options(nostack),
    );
    ret
}

fn write(fd: u64, buf: &[u8]) {
    unsafe { syscall3(12, fd, buf.as_ptr() as u64, buf.len() as u64) };
}
fn rename(src: &[u8], dst: &[u8]) -> u64 {
    unsafe {
        syscall4(
            26,
            src.as_ptr() as u64,
            src.len() as u64,
            dst.as_ptr() as u64,
            dst.len() as u64,
        )
    }
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}
fn cstr(p: *const u8) -> &'static [u8] {
    let mut n = 0;
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        core::slice::from_raw_parts(p, n)
    }
}

#[no_mangle]
extern "C" fn mv_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    if argc != 3 {
        write(1, b"mv: usage: mv <src> <dst>\n");
        exit(1);
    }
    let src = cstr(unsafe { *sp.add(2) } as *const u8);
    let dst = cstr(unsafe { *sp.add(3) } as *const u8);
    if rename(src, dst) == u64::MAX {
        write(1, b"mv: cannot move '");
        write(1, src);
        write(1, b"' to '");
        write(1, dst);
        write(1, b"'\n");
        exit(1);
    }
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
