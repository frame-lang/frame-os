// user-aarch64-hello/src/main.rs
//
// Minimal aarch64 user-mode hello program (B-HAL.5.2). No std, no libc, no
// linker magic beyond our own linker script. _start runs at EL0 (the kernel
// `eret`s into it after loading the ELF + setting SP_EL0), invokes the
// kernel's syscall API directly via `svc #0`:
//   x8 = 0, x0 = byte → write one byte to the console
//   x8 = 1            → exit
//   x8 = 2            → getticks (returns kernel TICK_COUNT in x0)
//
// Prints "hello from aarch64 ELF\n" then exits. This is the simplest possible
// "real user program" — separately compiled, separately linked, loaded by a
// kernel-side ELF parser, runs at EL0 with no kernel hand-holding.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[inline(always)]
fn write_byte(b: u8) {
    unsafe {
        asm!(
            "mov x8, #0",
            "mov x0, {0:x}",
            "svc #0",
            in(reg) b as u64,
            out("x0") _,
            out("x8") _,
            options(nostack),
        );
    }
}

fn write_str(s: &str) {
    for &b in s.as_bytes() {
        write_byte(b);
    }
}

#[inline(always)]
fn exit() -> ! {
    unsafe {
        asm!("mov x8, #1", "svc #0", options(noreturn));
    }
}

#[no_mangle]
#[link_section = ".text.boot"]
pub extern "C" fn _start() -> ! {
    write_str("hello from aarch64 ELF\n");
    exit();
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit();
}
