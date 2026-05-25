// Frame OS user program "date" (S4) — print the wall-clock date/time. Reads the
// CMOS RTC via the time syscall (#18) and formats it as "YYYY-MM-DD HH:MM:SS"
// (UTC), using Howard Hinnant's civil_from_days to break the epoch down — the
// same math frame-libc's localtime uses. Disk-only: the shell runs `/bin/date`.
//
// Syscall ABI: 1 = exit, 12 = write(fd, buf, len), 18 = time() → epoch seconds.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

// date takes no args, but still 16-align rsp before the call (ABI; guards
// against a stray SSE store on a misaligned stack).
global_asm!(
    ".global _start",
    "_start:",
    "  and rsp, -16",
    "  call date_main",
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
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// days-since-epoch → (year, month [1,12], day [1,31]) — civil_from_days.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Append `v` to `buf` at `*n` as `width` zero-padded decimal digits.
fn put_pad(buf: &mut [u8], n: &mut usize, v: i64, width: usize) {
    let mut tmp = [0u8; 20];
    let mut len = 0;
    let mut x = v;
    if x == 0 {
        tmp[len] = b'0';
        len += 1;
    }
    while x > 0 {
        tmp[len] = b'0' + (x % 10) as u8;
        len += 1;
        x /= 10;
    }
    while len < width {
        tmp[len] = b'0';
        len += 1;
    }
    while len > 0 {
        len -= 1;
        buf[*n] = tmp[len];
        *n += 1;
    }
}

#[no_mangle]
extern "C" fn date_main() -> ! {
    let secs = unsafe { syscall3(18, 0, 0, 0) } as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, mon, mday) = civil_from_days(days);

    let mut out = [0u8; 32];
    let mut n = 0;
    put_pad(&mut out, &mut n, year, 4);
    out[n] = b'-';
    n += 1;
    put_pad(&mut out, &mut n, mon, 2);
    out[n] = b'-';
    n += 1;
    put_pad(&mut out, &mut n, mday, 2);
    out[n] = b' ';
    n += 1;
    put_pad(&mut out, &mut n, rem / 3600, 2);
    out[n] = b':';
    n += 1;
    put_pad(&mut out, &mut n, (rem / 60) % 60, 2);
    out[n] = b':';
    n += 1;
    put_pad(&mut out, &mut n, rem % 60, 2);
    out[n] = b'\n';
    n += 1;
    write(1, &out[..n]);
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
