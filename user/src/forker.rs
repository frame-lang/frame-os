// Frame OS user program "forker" (B3 Step 5b).
//
// Demonstrates concurrent user processes: it `fork`s, then parent and child
// each print their own character several times (with a spin between prints so
// the timer preempts and interleaves them). Both then exit. Two user processes
// running concurrently in separate address spaces — the payoff of the
// multitasking core.
//
// Syscall ABI: rax = number (0 write_char, 1 exit, 2 fork), args in rdi.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[inline(always)]
unsafe fn syscall1(num: u64, a0: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        inlateout("rax") num => ret,
        in("rdi") a0,
        out("rcx") _,
        out("r11") _,
        options(nostack),
    );
    ret
}

fn write_char(b: u8) {
    unsafe {
        syscall1(0, b as u64);
    }
}

fn fork() -> u64 {
    unsafe { syscall1(2, 0) }
}

fn exit(code: u64) -> ! {
    unsafe {
        syscall1(1, code);
    }
    loop {
        core::hint::spin_loop();
    }
}

// Spin long enough that the 100 Hz timer preempts mid-loop and interleaves the
// parent and child (under TCG the timer fires far slower than wall-clock).
fn pace() {
    for _ in 0..40_000u64 {
        core::hint::spin_loop();
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // fork() returns 0 in the child, the child's pid in the parent.
    let ch = if fork() == 0 { b'C' } else { b'P' };
    let mut i = 0;
    while i < 6 {
        write_char(ch);
        pace();
        i += 1;
    }
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
