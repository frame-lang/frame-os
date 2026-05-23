// Frame OS user program "brktest" (B9-1).
//
// Exercises the growable heap. `brk(0)` queries the initial program break;
// `brk(base + 1 MiB)` asks the kernel to grow the heap by a megabyte — far
// beyond the fixed 64 KiB program-image heap a toolchain would otherwise be
// stuck with. The program then writes a pattern across the *entire* new region
// (at u64 stride, so every freshly mapped page is touched) and reads it back,
// proving the kernel demand-mapped real, writable, per-process memory. Prints a
// result line and exits 0 on success (nonzero on a grow failure / mismatch).
//
// Syscall ABI: 0 = write_char, 1 = exit, 10 = brk(new_end) → new break.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

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

fn write_char(b: u8) {
    unsafe { syscall3(0, b as u64, 0, 0) };
}
fn print(s: &[u8]) {
    for &b in s {
        write_char(b);
    }
}
fn print_u64(mut n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if n == 0 {
        write_char(b'0');
        return;
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    print(&buf[i..]);
}
fn brk(new_end: u64) -> u64 {
    unsafe { syscall3(10, new_end, 0, 0) }
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

const GROW: u64 = 1024 * 1024; // 1 MiB
const STEP: u64 = 0x9E37_79B9_7F4A_7C15; // odd multiplier → distinct per-index values

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Query the initial break, then grow the heap by 1 MiB.
    let base = brk(0);
    let new = brk(base + GROW);
    if new < base + GROW {
        print(b"brk: grow FAILED\n");
        exit(1);
    }

    // Write a distinct value to every u64 across the new region (touches every
    // mapped page), then read it all back and verify.
    let words = (GROW / 8) as usize;
    let p = base as *mut u64;
    let mut i = 0usize;
    while i < words {
        unsafe { p.add(i).write_volatile((i as u64).wrapping_mul(STEP)) };
        i += 1;
    }
    let mut ok = true;
    let mut j = 0usize;
    while j < words {
        let got = unsafe { p.add(j).read_volatile() };
        if got != (j as u64).wrapping_mul(STEP) {
            ok = false;
            break;
        }
        j += 1;
    }

    if ok {
        print(b"brk: base 0x");
        // crude hex print of the base, for the log
        let mut shift = 28u32;
        loop {
            let nib = ((base >> shift) & 0xF) as u8;
            write_char(if nib < 10 { b'0' + nib } else { b'a' + nib - 10 });
            if shift == 0 {
                break;
            }
            shift -= 4;
        }
        print(b", grew heap by ");
        print_u64(GROW / 1024);
        print(b" KiB, write/read-back ok\n");
        exit(0);
    } else {
        print(b"brk: VERIFY MISMATCH\n");
        exit(2);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
