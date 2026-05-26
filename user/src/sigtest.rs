// Frame OS user program "sigtest" (S10 2b) — exercise signal handlers.
//
// Installs a SIGTERM (15) handler via sigaction (#30), then loops on time()
// until the handler fires. When the shell sends SIGTERM (`kill %<job>`), the
// kernel delivers it at the next syscall boundary by entering the handler
// (instead of the default terminate). The handler prints, sets a flag, and
// returns — its return address is the restorer stub below, which invokes
// sigreturn (#31); the kernel then restores the interrupted context and the
// loop resumes, sees the flag, and exits cleanly. So a handled SIGTERM does NOT
// kill the process — proving the trampoline + sigreturn round-trip works.
//
// Syscall ABI: 1 = exit, 12 = write, 18 = time, 30 = sigaction, 31 = sigreturn.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

// crt0 + the restorer trampoline. _start just calls main. sig_restorer is the
// SA_RESTORER stub the kernel pushes as the handler's return address: when the
// handler returns, it lands here and invokes sigreturn (#31), which never
// returns (the kernel resumes the interrupted context instead).
global_asm!(
    ".global _start",
    "_start:",
    "  call sigtest_main",
    "  ud2",
    ".global sig_restorer",
    "sig_restorer:",
    "  mov rax, 31",
    "  syscall",
    "  ud2",
);

extern "C" {
    fn sig_restorer();
}

static CAUGHT: AtomicBool = AtomicBool::new(false);

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
/// sigaction(sig, handler, restorer) → previous handler.
fn sigaction(sig: u64, handler: u64, restorer: u64) -> u64 {
    unsafe { syscall3(30, sig, handler, restorer) }
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

// SIGTERM handler. Runs in user space on the interrupted stack; prints, records
// that it fired, and returns (into sig_restorer → sigreturn).
extern "C" fn on_sigterm(_sig: u64) {
    write(1, b"handler: caught SIGTERM\n");
    CAUGHT.store(true, Ordering::SeqCst);
}

#[no_mangle]
extern "C" fn sigtest_main() -> ! {
    write(1, b"sigtest: ready\n");
    let handler = on_sigterm as extern "C" fn(u64) as *const () as u64;
    let restorer = sig_restorer as *const () as u64;
    sigaction(15, handler, restorer); // SIGTERM
    loop {
        // A syscall gives the pending signal a delivery boundary; on return the
        // kernel may have entered the handler and sigreturn'd back to here.
        let _ = time();
        if CAUGHT.load(Ordering::SeqCst) {
            write(1, b"sigtest: resumed after handler, exiting\n");
            exit(0);
        }
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
