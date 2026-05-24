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
// Syscall ABI: 1 = exit, 12 = write(fd, buf, len) (fd 1 = console, atomic line).

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

/// Write `buf` to fd as a SINGLE syscall (#12). For the console (fd 1) the
/// kernel emits the whole buffer in one syscall — atomically, with no
/// preemption point between bytes — so a line can't be split mid-way by a
/// concurrent process on the shared console (unlike byte-at-a-time write_char).
/// That determinism is what `coexec`'s `argv[1]=Z` assertion relies on.
fn write(fd: u64, buf: &[u8]) {
    unsafe { syscall3(12, fd, buf.as_ptr() as u64, buf.len() as u64) };
}

/// Append the decimal of `n` to `buf` at `*len`, advancing `*len`.
fn put_u64(buf: &mut [u8], len: &mut usize, mut n: u64) {
    if n == 0 {
        buf[*len] = b'0';
        *len += 1;
        return;
    }
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    while n > 0 {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    while i < tmp.len() {
        buf[*len] = tmp[i];
        *len += 1;
        i += 1;
    }
}

/// Append the literal bytes `s` to `buf` at `*len`.
fn put(buf: &mut [u8], len: &mut usize, s: &[u8]) {
    for &b in s {
        buf[*len] = b;
        *len += 1;
    }
}

fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

#[no_mangle]
extern "C" fn argtest_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    // Each line is built in a buffer and emitted with one write() so it prints
    // atomically (see `write` above) — critical under concurrent exec (coexec).
    let mut line = [0u8; 128];
    let mut n = 0usize;
    put(&mut line, &mut n, b"argtest: argc=");
    put_u64(&mut line, &mut n, argc);
    put(&mut line, &mut n, b"\n");
    write(1, &line[..n]);

    let mut i = 0u64;
    while i < argc {
        // argv[i] lives at sp[1 + i].
        let arg = unsafe { *sp.add(1 + i as usize) } as *const u8;
        let mut n = 0usize;
        put(&mut line, &mut n, b"  argv[");
        put_u64(&mut line, &mut n, i);
        put(&mut line, &mut n, b"]=");
        // Copy the NUL-terminated argv string (bounded so a long arg can't
        // overflow the line buffer; leave room for the trailing newline).
        let mut q = arg;
        unsafe {
            while *q != 0 && n < line.len() - 1 {
                line[n] = *q;
                n += 1;
                q = q.add(1);
            }
        }
        line[n] = b'\n';
        n += 1;
        write(1, &line[..n]);
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
