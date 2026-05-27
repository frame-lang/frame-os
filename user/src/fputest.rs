// Frame OS user program "fputest" (B11-3a).
//
// Proves the scheduler saves/restores the x87/SSE register file across context
// switches. It `fork`s into two processes that run concurrently under the
// preemptive timer; each pins a *distinct* sentinel pattern into xmm0..xmm7,
// spins (the preemption window), then reads the registers back and checks they
// still hold its own sentinels. If the kernel did not save/restore the FPU on a
// switch, the other process's xmm loads would clobber these registers and the
// readback would mismatch. Each process prints PASS/FAIL; the smoke test asserts
// both pass.
//
// Syscall ABI: 0 = write_char, 1 = exit, 2 = fork, 4 = wait.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[inline(always)]
unsafe fn syscall3(num: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    asm!(
        "syscall",
        inlateout("rax") num => ret,
        in("rdi") a0, in("rsi") a1, in("rdx") a2,
        out("rcx") _, out("r11") _,
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
fn print_hex(mut n: u64) {
    print(b"0x");
    let mut shift = 60i32;
    while shift >= 0 {
        let nib = ((n >> shift) & 0xF) as u8;
        write_char(if nib < 10 {
            b'0' + nib
        } else {
            b'a' + nib - 10
        });
        shift -= 4;
    }
    let _ = &mut n;
}
fn fork() -> u64 {
    unsafe { syscall3(2, 0, 0, 0) }
}
fn wait() -> u64 {
    unsafe { syscall3(4, 0, 0, 0) }
}
fn exit(code: u64) -> ! {
    unsafe { syscall3(1, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

const ITERS: u64 = 4000;
const SPIN: u64 = 40_000;

/// Load `sent[0..8]` into xmm0..xmm7, spin `SPIN` iterations (a preemption
/// window with the sentinels live in the SSE registers), then read the eight
/// registers back into `got`. All in one asm block so the compiler can't insert
/// FPU-using code between the load and the readback.
#[inline(never)]
fn load_spin_read(sent: &[u64; 8], got: &mut [u64; 8]) {
    unsafe {
        asm!(
            "movq xmm0, [{s} + 0]",
            "movq xmm1, [{s} + 8]",
            "movq xmm2, [{s} + 16]",
            "movq xmm3, [{s} + 24]",
            "movq xmm4, [{s} + 32]",
            "movq xmm5, [{s} + 40]",
            "movq xmm6, [{s} + 48]",
            "movq xmm7, [{s} + 56]",
            "2:",
            "dec {c}",
            "jnz 2b",
            "movq [{g} + 0], xmm0",
            "movq [{g} + 8], xmm1",
            "movq [{g} + 16], xmm2",
            "movq [{g} + 24], xmm3",
            "movq [{g} + 32], xmm4",
            "movq [{g} + 40], xmm5",
            "movq [{g} + 48], xmm6",
            "movq [{g} + 56], xmm7",
            s = in(reg) sent.as_ptr(),
            g = in(reg) got.as_mut_ptr(),
            c = inout(reg) SPIN => _,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            options(nostack),
        );
    }
}

/// Run the load/spin/readback loop with sentinels derived from `base`; returns
/// `true` if every register held its sentinel on every iteration.
fn run_checks(base: u64) -> bool {
    let mut sent = [0u64; 8];
    let mut i = 0;
    while i < 8 {
        // Distinct per-register, per-process values (odd multiplier spreads bits).
        sent[i] = base ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        i += 1;
    }
    let mut iter = 0;
    while iter < ITERS {
        let mut got = [0u64; 8];
        load_spin_read(&sent, &mut got);
        let mut r = 0;
        while r < 8 {
            if got[r] != sent[r] {
                print(b"  xmm");
                write_char(b'0' + r as u8);
                print(b" want ");
                print_hex(sent[r]);
                print(b" got ");
                print_hex(got[r]);
                print(b"\n");
                return false;
            }
            r += 1;
        }
        iter += 1;
    }
    true
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let pid = fork();
    if pid == 0 {
        // Child: a distinct sentinel base from the parent.
        if run_checks(0x5555_5555_5555_5555) {
            print(b"fputest: child PASS\n");
            exit(0);
        } else {
            print(b"fputest: child FAIL (xmm clobbered across switch)\n");
            exit(1);
        }
    } else {
        let ok = run_checks(0xAAAA_AAAA_AAAA_AAAA);
        let status = wait(); // reap the child
        if ok && status == 0 {
            print(b"fputest: parent PASS; FPU state preserved across preemption\n");
            exit(0);
        } else {
            print(b"fputest: parent FAIL\n");
            exit(2);
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
