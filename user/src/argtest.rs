// Frame OS user program "argtest" (B9-2).
//
// Proves that command-line arguments reach a program. The shell `exec_argv`s a
// command line; the kernel lays a System V initial stack (argc, argv[], envp
// NULL, auxv AT_NULL) onto the new program's stack with `rsp` pointing at argc.
// A normal Rust `extern "C" fn _start` can't see that stack, so `_start` here is
// a tiny asm shim that hands the entry `rsp` to `argtest_main`, which walks
// argc / argv and echoes them back. Disk-only: the shell loads it from
// `/bin/argtest`; it is not baked into the kernel.
//
// Syscall ABI: 0 = write_char, 1 = exit.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

// Entry shim: at process start `rsp` points at `argc` (16-aligned). Hand that
// pointer to `argtest_main` in rdi, keeping the stack 16-aligned for the call.
global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp", // arg0 = &argc (the SysV stack the kernel built)
    "  and rsp, -16", // ABI: 16-align before the call
    "  call argtest_main",
    "  ud2", // argtest_main never returns
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
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// Print the NUL-terminated string at `p` (a pointer the kernel put in argv[]).
fn print_cstr(p: *const u8) {
    let mut q = p;
    loop {
        let c = unsafe { *q };
        if c == 0 {
            break;
        }
        write_char(c);
        q = unsafe { q.add(1) };
    }
}

#[no_mangle]
extern "C" fn argtest_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    print(b"argtest: argc=");
    print_u64(argc);
    write_char(b'\n');
    let mut i = 0u64;
    while i < argc {
        // argv[i] lives at sp[1 + i].
        let arg = unsafe { *sp.add(1 + i as usize) } as *const u8;
        print(b"  argv[");
        print_u64(i);
        print(b"]=");
        print_cstr(arg);
        write_char(b'\n');
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
