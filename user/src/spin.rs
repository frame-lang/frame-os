// Frame OS user program "spin" (S10) — a long-lived process for signal testing.
//
// Prints one "spin: alive" line, then loops forever doing a cheap syscall
// (time, #18) with a short busy delay between iterations. It never exits on its
// own — it's meant to be killed: `spin &` backgrounds it, `kill %1` (or
// `kill <pid>`) sends SIGTERM, and the kernel delivers the signal the next time
// spin returns from its time() syscall, terminating it (default action). The
// syscall in the loop is what gives the signal a delivery boundary; a pure
// busy-loop with no syscalls would only take a signal at timer preemption,
// which the syscall-boundary delivery path (S10 2a) doesn't cover.
//
// Syscall ABI: 1 = exit, 12 = write, 18 = time.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(
    ".global _start",
    "_start:",
    "  mov rdi, rsp",
    "  and rsp, -16",
    "  call spin_main",
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
fn time() -> u64 {
    unsafe { syscall3(18, 0, 0, 0) }
}

#[no_mangle]
extern "C" fn spin_main(_sp: *const u64) -> ! {
    write(1, b"spin: alive\n");
    loop {
        // A cheap syscall gives any pending signal a delivery boundary; the
        // busy spin keeps us off a tight syscall storm without ever blocking.
        let _ = time();
        for _ in 0..300_000 {
            core::hint::spin_loop();
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
