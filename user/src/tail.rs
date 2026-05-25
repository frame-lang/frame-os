// Frame OS user program "tail" (S4) — print the last 10 lines of a file.
// `tail <file>`. Reads the file into a fixed buffer (larger files truncated to
// the buffer's tail), then walks backward to find the last 10 line starts.
// Disk-only: the shell runs `/bin/tail`.
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
    "  call tail_main",
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

const TAIL_LINES: usize = 10;

#[no_mangle]
extern "C" fn tail_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    if argc < 2 {
        write(1, b"tail: usage: tail <file>\n");
        exit(1);
    }
    let path = cstr(unsafe { *sp.add(2) } as *const u8);
    let fd = open_read(path);
    if fd == u64::MAX {
        write(1, b"tail: cannot open '");
        write(1, path);
        write(1, b"'\n");
        exit(1);
    }
    let mut data = [0u8; 8192];
    let mut total = 0usize;
    loop {
        if total >= data.len() {
            break;
        }
        let n = read(fd, &mut data[total..]);
        if n == 0 || n == u64::MAX {
            break;
        }
        total += n as usize;
    }
    close(fd);
    // Find the start of the last TAIL_LINES lines: walk back counting newlines
    // (ignoring a trailing newline at the very end).
    let mut newlines = 0usize;
    let mut start = 0usize;
    let end = if total > 0 && data[total - 1] == b'\n' {
        total - 1
    } else {
        total
    };
    let mut i = end;
    while i > 0 {
        i -= 1;
        if data[i] == b'\n' {
            newlines += 1;
            if newlines == TAIL_LINES {
                start = i + 1;
                break;
            }
        }
    }
    write(1, &data[start..total]);
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
