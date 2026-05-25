// Frame OS user program "rmdir" (S7) — remove empty directories.
// `rmdir <path>...` removes each via the rmdir syscall (#25); paths resolve
// against the shell's cwd. Fails (and reports) if the target isn't an empty
// directory. Disk-only: the shell runs it as `/bin/rmdir`.
//
// Syscall ABI: 1 = exit, 12 = write(fd, buf, len), 25 = rmdir(path, len).

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp",
    "  and rsp, -16",
    "  call rmdir_main",
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

fn write(fd: u64, buf: &[u8]) {
    unsafe { syscall3(12, fd, buf.as_ptr() as u64, buf.len() as u64) };
}
fn rmdir(path: &[u8]) -> u64 {
    unsafe { syscall3(25, path.as_ptr() as u64, path.len() as u64, 0) }
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
extern "C" fn rmdir_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    if argc < 2 {
        write(1, b"rmdir: usage: rmdir <path>...\n");
        exit(1);
    }
    let mut status = 0u64;
    let mut i = 1u64;
    while i < argc {
        let p = unsafe { *sp.add(1 + i as usize) } as *const u8;
        let path = unsafe { core::slice::from_raw_parts(p, cstr_len(p)) };
        if rmdir(path) == u64::MAX {
            write(1, b"rmdir: cannot remove '");
            write(1, path);
            write(1, b"'\n");
            status = 1;
        }
        i += 1;
    }
    exit(status);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
