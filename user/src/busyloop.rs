// Frame OS user program "busyloop" (S10 2d) — a CPU-bound process that makes NO
// syscalls after its banner. Unlike `spin` (which loops on time()), this never
// reaches a syscall boundary on its own, so the ONLY way to signal it is the
// timer-path delivery: the kernel, preempting it, redirects it to the signal
// trampoline (terminate) or marks it Stopped (Ctrl-Z). It exists to prove that
// path — a `kill` of this process must still work.
//
// Syscall ABI: 12 = write. (No exit here — it's meant to be killed; if it ever
// returns from the loop, the crt0 `ud2` traps.)

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

global_asm!(
    ".global _start",
    "_start:",
    "  and rsp, -16",
    "  call busyloop_main",
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

#[no_mangle]
extern "C" fn busyloop_main() -> ! {
    write(1, b"busyloop: running\n");
    // Pure CPU loop — NO syscalls. Only timer-path signal delivery can end this.
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
