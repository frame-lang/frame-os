// Frame OS user program "head" (S4) — print the first 10 lines of a file.
// `head <file>`. Reads the file into a fixed buffer (larger files truncated).
// Disk-only: the shell runs `/bin/head`.
//
// Syscall ABI: 1 = exit, 5 = open, 6 = read, 7 = close, 12 = write.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp",
    "  and rsp, -16",
    "  call head_main",
    "  ud2",
);

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
fn write(fd: u64, b: &[u8]) {
    unsafe { syscall3(12, fd, b.as_ptr() as u64, b.len() as u64) };
}
fn open_read(p: &[u8]) -> u64 {
    unsafe { syscall3(5, p.as_ptr() as u64, p.len() as u64, 0) }
}
fn read(fd: u64, b: &mut [u8]) -> u64 {
    unsafe { syscall3(6, fd, b.as_mut_ptr() as u64, b.len() as u64) }
}
fn close(fd: u64) {
    unsafe { syscall3(7, fd, 0, 0) };
}
fn exit(c: u64) -> ! {
    unsafe { syscall3(1, c, 0, 0) };
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

const HEAD_LINES: usize = 10;

#[no_mangle]
extern "C" fn head_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    if argc < 2 {
        write(1, b"head: usage: head <file>\n");
        exit(1);
    }
    let path = cstr(unsafe { *sp.add(2) } as *const u8);
    let fd = open_read(path);
    if fd == u64::MAX {
        write(1, b"head: cannot open '");
        write(1, path);
        write(1, b"'\n");
        exit(1);
    }
    let mut data = [0u8; 8192];
    let mut total = 0usize;
    loop {
        if total >= data.len() {
            break;
        }
        let n = read(fd, &mut data[total..]);
        if n == 0 || n == u64::MAX {
            break;
        }
        total += n as usize;
    }
    close(fd);
    // Emit up to HEAD_LINES newline-terminated lines (plus a trailing partial).
    let mut emitted = 0usize;
    let mut start = 0usize;
    for i in 0..total {
        if data[i] == b'\n' {
            write(1, &data[start..=i]);
            start = i + 1;
            emitted += 1;
            if emitted == HEAD_LINES {
                break;
            }
        }
    }
    if emitted < HEAD_LINES && start < total {
        write(1, &data[start..total]);
        write(1, b"\n");
    }
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
