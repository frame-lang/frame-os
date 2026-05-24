// Frame OS user program "coexec" — concurrent exec regression test.
//
// Two processes exec *from disk concurrently*, exercising the per-exec scratch
// buffers (the fix for the shared `ELF_BUF`/`ARGV_BUF` race). `exec` does a
// blocking virtio read that yields, so the parent forks two children that both
// reach `exec_argv` and block on disk reads with overlapping windows; the
// scheduler interleaves them. With shared scratch statics the first child's ELF
// (or argv) would be overwritten by the second's mid-read, so the loader would
// map the wrong program. With per-exec heap buffers, each loads its own image:
//
//   child A: exec_argv(["/bin/hello"])        -> becomes hello,   exit 42
//   child B: exec_argv(["/bin/argtest", "Z"]) -> becomes argtest, prints argv[1]=Z
//
// The parent reaps both and prints "coexec: all done". The smoke test asserts
// argtest's "argv[1]=Z" appears — which is only possible if child B's ELF *and*
// its argv survived child A's concurrent exec uncorrupted (a swap or a clobber
// would lose it) — plus the parent's completion marker and no kernel fault.
//
// Syscall ABI: 0 write_char, 1 exit, 2 fork, 4 wait, 11 exec_argv.

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
    unsafe {
        syscall1(0, b as u64);
    }
}

fn print(s: &[u8]) {
    for &b in s {
        write_char(b);
    }
}

fn fork() -> u64 {
    unsafe { syscall1(2, 0) }
}

fn wait() -> u64 {
    unsafe { syscall1(4, 0) }
}

/// exec_argv(buf, len, argc): `buf` is `argc` NUL-terminated strings, argv[0]=path.
fn exec_argv(buf: &[u8], argc: u64) -> u64 {
    unsafe { syscall3(11, buf.as_ptr() as u64, buf.len() as u64, argc) }
}

fn exit(code: u64) -> ! {
    unsafe {
        syscall1(1, code);
    }
    loop {
        core::hint::spin_loop();
    }
}

// Packed argv buffers (argc NUL-terminated strings, argv[0] = the program path).
const ARGV_HELLO: &[u8] = b"/bin/hello\0";
const ARGV_ARGTEST: &[u8] = b"/bin/argtest\0Z\0";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Child A: becomes /bin/hello.
    if fork() == 0 {
        exec_argv(ARGV_HELLO, 1);
        exit(201); // only reached if exec failed
    }
    // Child B: becomes /bin/argtest with one argument ("Z").
    if fork() == 0 {
        exec_argv(ARGV_ARGTEST, 2);
        exit(202); // only reached if exec failed
    }
    // Parent: reap both children, then report completion.
    let _ = wait();
    let _ = wait();
    print(b"coexec: all done\n");
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
