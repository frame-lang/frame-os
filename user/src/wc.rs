// Frame OS user program "wc" (S4) — count lines, words, and bytes of a file.
// `wc <file>` prints "<lines> <words> <bytes> <file>". Reads the file into a
// fixed buffer (files larger than it are truncated — fine for the shell's use).
// Disk-only: the shell runs `/bin/wc`.
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
    "  call wc_main",
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
fn put_u64(buf: &mut [u8], n: &mut usize, mut v: u64) {
    let mut t = [0u8; 20];
    let mut l = 0;
    if v == 0 {
        t[l] = b'0';
        l += 1;
    }
    while v > 0 {
        t[l] = b'0' + (v % 10) as u8;
        l += 1;
        v /= 10;
    }
    while l > 0 {
        l -= 1;
        buf[*n] = t[l];
        *n += 1;
    }
}

#[no_mangle]
extern "C" fn wc_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    // No file argument → read from stdin (fd 0), so `wc < file` works via the
    // shell's input redirection (S5). With a file argument, open it and also
    // echo the name after the counts (classic `wc <file>` output).
    let from_stdin = argc < 2;
    let (fd, path): (u64, &[u8]) = if from_stdin {
        (0, b"")
    } else {
        let p = cstr(unsafe { *sp.add(2) } as *const u8);
        let fd = open_read(p);
        if fd == u64::MAX {
            write(1, b"wc: cannot open '");
            write(1, p);
            write(1, b"'\n");
            exit(1);
        }
        (fd, p)
    };
    let (mut lines, mut words, mut bytes) = (0u64, 0u64, 0u64);
    let mut in_word = false;
    let mut chunk = [0u8; 512];
    loop {
        let n = read(fd, &mut chunk);
        if n == 0 || n == u64::MAX {
            break;
        }
        for &c in &chunk[..n as usize] {
            bytes += 1;
            if c == b'\n' {
                lines += 1;
            }
            let ws = c == b' ' || c == b'\n' || c == b'\t' || c == b'\r';
            if ws {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
        }
    }
    if !from_stdin {
        close(fd);
    }
    let mut out = [0u8; 96];
    let mut n = 0;
    put_u64(&mut out, &mut n, lines);
    out[n] = b' ';
    n += 1;
    put_u64(&mut out, &mut n, words);
    out[n] = b' ';
    n += 1;
    put_u64(&mut out, &mut n, bytes);
    write(1, &out[..n]);
    // With a file argument, append " <name>" (classic wc); reading stdin prints
    // just the counts.
    if !from_stdin {
        write(1, b" ");
        write(1, path);
    }
    write(1, b"\n");
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
