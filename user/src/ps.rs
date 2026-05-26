// Frame OS user program "ps" (S9) — list the live processes. The kernel's ps
// syscall (#27) fills a buffer with packed 12-byte records [pid: u32 LE,
// ppid: u32 LE, state: u32 LE], one per process; we print a header and one row
// per record. State codes map to the classic single letters: 1→R (runnable),
// 2→S (sleeping/blocked, e.g. a shell waiting on a child), 3→Z (zombie/exited,
// not yet reaped). Disk-only: the shell runs `/bin/ps`.
//
// Syscall ABI: 1 = exit, 12 = write(fd, buf, len), 27 = ps(buf, buflen) →
//              bytes of packed records, or u64::MAX if the buffer is too small.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(
    ".global _start",
    "_start:",
    "  and rsp, -16",
    "  call ps_main",
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
fn exit(c: u64) -> ! {
    unsafe { syscall3(1, c, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// Append `v` right-justified in a field of `width` (space-padded) to `buf`.
fn put_u32_pad(buf: &mut [u8], n: &mut usize, v: u32, width: usize) {
    let mut t = [0u8; 10];
    let mut l = 0;
    let mut x = v;
    if x == 0 {
        t[l] = b'0';
        l += 1;
    }
    while x > 0 {
        t[l] = b'0' + (x % 10) as u8;
        l += 1;
        x /= 10;
    }
    for _ in l..width {
        buf[*n] = b' ';
        *n += 1;
    }
    while l > 0 {
        l -= 1;
        buf[*n] = t[l];
        *n += 1;
    }
}

#[no_mangle]
extern "C" fn ps_main() -> ! {
    let mut raw = [0u8; 12 * 8]; // up to MAX_THREADS records
    let got = unsafe { syscall3(27, raw.as_mut_ptr() as u64, raw.len() as u64, 0) };
    if got == u64::MAX {
        write(1, b"ps: snapshot failed\n");
        exit(1);
    }
    let nrec = (got as usize) / 12;
    write(1, b"  PID  PPID STAT\n");
    for i in 0..nrec {
        let o = i * 12;
        let pid = u32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
        let ppid = u32::from_le_bytes([raw[o + 4], raw[o + 5], raw[o + 6], raw[o + 7]]);
        let st = u32::from_le_bytes([raw[o + 8], raw[o + 9], raw[o + 10], raw[o + 11]]);
        let stat = match st {
            1 => b'R',
            2 => b'S',
            3 => b'Z',
            _ => b'?',
        };
        let mut line = [0u8; 32];
        let mut n = 0;
        put_u32_pad(&mut line, &mut n, pid, 5);
        put_u32_pad(&mut line, &mut n, ppid, 6);
        line[n] = b' ';
        n += 1;
        line[n] = stat;
        n += 1;
        line[n] = b'\n';
        n += 1;
        write(1, &line[..n]);
    }
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
