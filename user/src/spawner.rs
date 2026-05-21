// Frame OS user program "spawner" (B3 Step 5c).
//
// The canonical fork+exec pattern (what a shell does to launch a program):
// fork a child, and in the child `exec` a different program. Here the child
// execs `hello` (program id 0) — it becomes hello, prints "hello from ELF",
// and exits — while the parent prints 'S' and exits.
//
// Syscall ABI: rax = number (0 write_char, 1 exit, 2 fork, 3 exec), args in rdi.

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

fn exec(prog_id: u64) -> u64 {
    unsafe { syscall1(3, prog_id) }
}

fn exit(code: u64) -> ! {
    unsafe {
        syscall1(1, code);
    }
    loop {
        core::hint::spin_loop();
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    if fork() == 0 {
        // Child: replace ourselves with `hello` (program id 0). exec only
        // returns if it failed.
        exec(0);
        exit(1);
    } else {
        // Parent: print a marker and exit.
        write_char(b'S');
        write_char(b'\n');
        exit(0);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
