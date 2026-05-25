// Frame OS user program "touch" (S3) — create empty files (or open-existing).
// `touch <path>...` opens each for write (flag bit0 = 1 → create) and closes it.
// Paths resolve against the shell's cwd. Disk-only: the shell runs `/bin/touch`.
//
// Note: open-for-write truncates an existing file (the kernel has no O_APPEND
// yet), so unlike POSIX touch this empties an existing file rather than just
// bumping its mtime — fine for the create-a-file use the shell needs today.
//
// Syscall ABI: 1 = exit, 5 = open(path, len, flags), 7 = close, 12 = write.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp",
    "  and rsp, -16",
    "  call touch_main",
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

fn write(fd: u64, buf: &[u8]) {
    unsafe { syscall3(12, fd, buf.as_ptr() as u64, buf.len() as u64) };
}
/// open for write (flags bit0 = 1: create/truncate) → fd, or u64::MAX.
fn open_write(path: &[u8]) -> u64 {
    unsafe { syscall3(5, path.as_ptr() as u64, path.len() as u64, 1) }
}
fn close(fd: u64) {
    unsafe { syscall3(7, fd, 0, 0) };
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}
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
extern "C" fn touch_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    if argc < 2 {
        write(1, b"touch: usage: touch <path>...\n");
        exit(1);
    }
    let mut status = 0u64;
    let mut i = 1u64;
    while i < argc {
        let p = unsafe { *sp.add(1 + i as usize) } as *const u8;
        let path = unsafe { core::slice::from_raw_parts(p, cstr_len(p)) };
        let fd = open_write(path);
        if fd == u64::MAX {
            write(1, b"touch: cannot create '");
            write(1, path);
            write(1, b"'\n");
            status = 1;
        } else {
            close(fd);
        }
        i += 1;
    }
    exit(status);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
