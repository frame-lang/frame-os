// Frame OS freestanding user program "hello" (B3 Step 4).
//
// The first real user program: no std, no libc, no crt0. It makes raw
// `syscall`s using the Frame OS ABI and exits. The kernel's `ElfLoader` parses
// and maps this binary; the ring-3 demo runs it.
//
// Syscall ABI (Step 1b): rax = number, rdi/rsi = args, return in rax. `syscall`
// clobbers rcx (return RIP) and r11 (saved RFLAGS).
//   0 = write_char(rdi = byte) → serial; returns 1
//   1 = exit(rdi = code)       → never returns

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

fn exit(code: u64) -> ! {
    unsafe {
        syscall1(1, code);
    }
    // exit never returns; satisfy the `-> !` type if the kernel ever did.
    loop {
        core::hint::spin_loop();
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    for &b in b"hello from ELF\n" {
        write_char(b);
    }
    exit(42);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
