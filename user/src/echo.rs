// Frame OS user program "echo" (S3) — print its arguments separated by spaces,
// followed by a newline. The classic. Disk-only: the shell runs it as
// `/bin/echo`. (ish has no echo builtin, so this is the echo you get.)
//
// Syscall ABI: 1 = exit, 12 = write(fd, buf, len).

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp",
    "  and rsp, -16",
    "  call echo_main",
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
fn cstr_len(p: *const u8) -> usize {
    let mut n = 0;
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
    }
    n
}

#[no_mangle]
extern "C" fn echo_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    let mut i = 1u64;
    while i < argc {
        if i > 1 {
            write(1, b" ");
        }
        let p = unsafe { *sp.add(1 + i as usize) } as *const u8;
        write(1, unsafe { core::slice::from_raw_parts(p, cstr_len(p)) });
        i += 1;
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
