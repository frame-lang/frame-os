// Frame OS user program "cp" (S3) — copy a file. `cp <src> <dst>` opens src for
// reading and dst for writing (create/truncate), then streams bytes through a
// buffer. Paths resolve against the shell's cwd. Disk-only: the shell runs
// `/bin/cp`.
//
// Syscall ABI: 1 = exit, 5 = open(path, len, flags), 6 = read(fd, buf, len),
//              7 = close(fd), 12 = write(fd, buf, len).

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp",
    "  and rsp, -16",
    "  call cp_main",
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

fn write(fd: u64, buf: &[u8]) -> u64 {
    unsafe { syscall3(12, fd, buf.as_ptr() as u64, buf.len() as u64) }
}
fn open_read(path: &[u8]) -> u64 {
    unsafe { syscall3(5, path.as_ptr() as u64, path.len() as u64, 0) }
}
fn open_write(path: &[u8]) -> u64 {
    unsafe { syscall3(5, path.as_ptr() as u64, path.len() as u64, 1) }
}
fn read(fd: u64, buf: &mut [u8]) -> u64 {
    unsafe { syscall3(6, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
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
fn cstr(p: *const u8) -> &'static [u8] {
    let mut n = 0;
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        core::slice::from_raw_parts(p, n)
    }
}
fn err(msg: &[u8]) -> ! {
    write(1, msg);
    exit(1);
}

#[no_mangle]
extern "C" fn cp_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    if argc < 3 {
        err(b"cp: usage: cp <src> <dst>\n");
    }
    let src = cstr(unsafe { *sp.add(2) } as *const u8);
    let dst = cstr(unsafe { *sp.add(3) } as *const u8);

    let rfd = open_read(src);
    if rfd == u64::MAX {
        err(b"cp: cannot open source\n");
    }
    let wfd = open_write(dst);
    if wfd == u64::MAX {
        err(b"cp: cannot create destination\n");
    }

    let mut buf = [0u8; 512];
    loop {
        let n = read(rfd, &mut buf);
        if n == 0 || n == u64::MAX {
            break;
        }
        let n = n as usize;
        if write(wfd, &buf[..n]) != n as u64 {
            close(rfd);
            close(wfd);
            err(b"cp: write error\n");
        }
    }
    close(rfd);
    close(wfd);
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
