// kernel/src/arch/aarch64/usermode.rs
//
// EL0 (user mode) + SVC syscall path on aarch64 (B-HAL.5.0). The minimum
// end-to-end proof of the user/kernel boundary on a second ISA: kernel drops
// to EL0 running a tiny user routine; the user routine prints "HELLO from
// EL0" byte-by-byte via `svc #0` with x8 = 0 (write); each SVC raises a
// "Lower EL aarch64 Sync" exception (slot 8) that's wired to `svc_stub`; the
// stub saves a full trap frame and calls `rust_svc_handler`, which dispatches
// the SVC by x8 (the syscall number register, x86 SYSV / Linux idiom); the
// write syscall pushes the byte through PL011 and `eret`s back to EL0; the
// final SVC with x8 = 1 (exit) rewrites the saved frame's ELR_EL1/SPSR_EL1
// to redirect the `eret` to a kernel return point at EL1, longjmp-style.
//
// The demo runs once from kmain via `run_el0_demo()`. B-HAL.5.1 expands the
// syscall table; B-HAL.5.2 adds real user-ELF loading; the substrate here is
// what they sit on.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::serial;

/// User stack for the EL0 demo (B-HAL.5.0). Lives in RAM, which the L1 block
/// descriptor marked EL0-accessible (B-HAL.5.0 MMU change). One stack is
/// plenty for the demo (no scheduling at EL0 yet — that's B-HAL.5.2+).
const USER_STACK_SIZE: usize = 8 * 1024;
static mut USER_STACK: [u8; USER_STACK_SIZE] = [0; USER_STACK_SIZE];

/// Count of bytes the user routine successfully wrote via `svc` (proves the
/// SVC path round-tripped, not just that EL0 was entered). The boot context
/// reads this after `run_el0_demo` returns.
pub static USER_BYTES_WRITTEN: AtomicU32 = AtomicU32::new(0);

/// Set by the exit syscall before redirecting the `eret` back to EL1 — lets
/// the boot context confirm the user routine actually called exit (rather
/// than e.g. faulting back out via a different path).
pub static USER_EXITED: AtomicBool = AtomicBool::new(false);

// The EL0 entry routine. Prints "HELLO from EL0\n" byte-by-byte via SVC with
// x8 = 0 (write byte in x0), then *spins long enough for at least one timer
// tick to fire while at EL0* — proves the lower-EL IRQ vector (slot 9) wires
// to irq_stub, the full-frame save/restore round-trips an EL0→EL1→EL0
// transition correctly, and IRQs don't disturb the user routine's logical
// state. After the spin, reads the post-tick count via `getticks` (x8 = 2),
// writes a marker line, then exits via SVC with x8 = 1.
//
// AAPCS scratch registers only — x0/x1/x8 in the body, x19/x20 saved as
// callee-saved across the spin loop (we never `ret`, so the callee-saved
// preservation is for readability rather than ABI conformance).
global_asm!(
    ".section .text",
    ".global aarch64_user_demo_entry",
    "aarch64_user_demo_entry:",
    "  mov x8, #0", // write byte
    "  mov x0, #'H'",
    "  svc #0",
    "  mov x0, #'E'",
    "  svc #0",
    "  mov x0, #'L'",
    "  svc #0",
    "  mov x0, #'L'",
    "  svc #0",
    "  mov x0, #'O'",
    "  svc #0",
    "  mov x0, #' '",
    "  svc #0",
    "  mov x0, #'f'",
    "  svc #0",
    "  mov x0, #'r'",
    "  svc #0",
    "  mov x0, #'o'",
    "  svc #0",
    "  mov x0, #'m'",
    "  svc #0",
    "  mov x0, #' '",
    "  svc #0",
    "  mov x0, #'E'",
    "  svc #0",
    "  mov x0, #'L'",
    "  svc #0",
    "  mov x0, #'0'",
    "  svc #0",
    "  mov x0, #10", // '\n'
    "  svc #0",
    // Spin ~1M iterations: long enough under TCG (10 Hz timer) for at least
    // one tick to land at EL0. The IRQ goes through slot 9 → irq_stub (same
    // one as EL1) → rust_irq_handler → returns the same SP → restore frame
    // → eret back to EL0, continuing the spin. 1_000_000 = 0xF4240 — too
    // wide for a single `mov #imm16`, so build it with movz + movk.
    "  movz x19, #0x4240",
    "  movk x19, #0xf, lsl #16",
    "2:",
    "  subs x19, x19, #1",
    "  b.ne 2b",
    // Print "ticks=" then the tick count digit-by-digit. For the demo we
    // only print a single digit (we expect ~1–4 ticks); 10+ would just stop
    // at '9'+overflow which is fine for the smoke.
    "  mov x8, #0",
    "  mov x0, #'t'",
    "  svc #0",
    "  mov x0, #'i'",
    "  svc #0",
    "  mov x0, #'c'",
    "  svc #0",
    "  mov x0, #'k'",
    "  svc #0",
    "  mov x0, #'s'",
    "  svc #0",
    "  mov x0, #'='",
    "  svc #0",
    "  mov x8, #2", // getticks
    "  svc #0",     // returns count in x0
    "  cmp x0, #9",
    "  b.le 3f",
    "  mov x0, #9", // cap at single digit
    "3:",
    "  add x0, x0, #'0'", // ASCII digit
    "  mov x8, #0",
    "  svc #0",
    "  mov x0, #10", // '\n'
    "  svc #0",
    "  mov x8, #1", // exit
    "  svc #0",
    "1: b 1b",
);

// `enter_el0(entry, user_stack_top)`:
//   1. saves the kernel caller's FP+LR onto SP_EL1 (so `el0_return` can restore
//      them and `ret` to the caller of `enter_el0`);
//   2. sets ELR_EL1 = `entry`, SP_EL0 = `user_stack_top`, SPSR_EL1 = 0
//      (M = 0b00000 = EL0t, all DAIF masks clear);
//   3. `eret` — the CPU transitions to EL0 at `entry` using SP_EL0.
//
// `el0_return` is the symbol the exit syscall sets ELR_EL1 to before its
// stub-driven `eret` — control lands here at EL1, the kernel SP_EL1 is exactly
// where it was at the moment of the original `eret` (the SVC stub's epilog
// restored it), and `ldp + ret` returns to wherever in the kernel called
// `enter_el0`. AAPCS-clean.
global_asm!(
    ".section .text",
    ".global enter_el0",
    "enter_el0:",
    "  stp x29, x30, [sp, #-16]!",
    "  mov x29, sp",
    "  msr elr_el1, x0",
    "  msr sp_el0, x1",
    // SPSR_EL1 = EL0t (M=0) with all DAIF masks clear — EL0 runs with IRQs
    // (and FIQ/SError/Debug) unmasked. The lower-EL IRQ vector (slot 9) was
    // wired to irq_stub in B-HAL.5.1, so a tick taken at EL0 goes through
    // the same full-frame save/restore as a tick taken at EL1 and `eret`s
    // back into the user routine where it left off.
    "  mov x9, #0",
    "  msr spsr_el1, x9",
    "  eret",
    ".global el0_return",
    "el0_return:",
    "  ldp x29, x30, [sp], #16",
    "  ret",
);

extern "C" {
    fn enter_el0(entry: u64, user_stack_top: u64);
    static el0_return: u8;
    static aarch64_user_demo_entry: u8;
}

/// The TrapFrame the svc_stub saves. Same layout as the IRQ stub's; matters
/// here because `rust_svc_handler` modifies fields directly to redirect the
/// `eret` (exit syscall) and to deliver the SVC's return value (x0).
#[repr(C)]
struct TrapFrame {
    gprs: [u64; 31], // x0..x30
    elr: u64,        // ELR_EL1 — `eret` PC
    spsr: u64,       // SPSR_EL1 — `eret` PSTATE
    _pad: u64,       // 16-byte SP alignment
}

const _: () = assert!(core::mem::size_of::<TrapFrame>() == 272);

// ESR_EL1.EC for an SVC from aarch64 (AArch64 reference manual).
const ESR_EC_SVC64: u64 = 0x15;

/// The Rust half of the SVC handler, called by `svc_stub` with the full trap
/// frame saved at `frame_sp`. Returns the SP for the stub to restore on —
/// always the same SP here (no thread switching in the .5.0 demo), but the
/// frame's fields may have been modified:
///   - x0: syscall return value.
///   - For exit: ELR_EL1 = `el0_return`, SPSR_EL1 = EL1h, so the `eret`
///     transitions back to EL1 at the kernel return point.
///
/// # Safety
/// Called only from `svc_stub` with `frame_sp` pointing at a populated
/// `TrapFrame` on the kernel stack.
#[no_mangle]
unsafe extern "C" fn rust_svc_handler(frame_sp: u64) -> u64 {
    let frame = unsafe { &mut *(frame_sp as *mut TrapFrame) };

    let esr: u64;
    unsafe { asm!("mrs {0}, esr_el1", out(reg) esr, options(nomem, nostack)) };
    let ec = esr >> 26;
    if ec != ESR_EC_SVC64 {
        // Not an SVC — unexpected for B-HAL.5.0. Park; B-HAL.5.1 will classify.
        serial::write_str("[el0] unexpected sync exception EC=0x");
        serial::write_hex_u64(ec);
        serial::writeln("");
        loop {
            unsafe { asm!("wfe", options(nomem, nostack)) };
        }
    }

    let syscall_no = frame.gprs[8]; // x8 — the syscall number register
    match syscall_no {
        // write byte (x0 = byte). Push it through PL011. Return value = bytes
        // written (1). ELR/SPSR unchanged → `eret` resumes EL0 right after svc.
        0 => {
            let byte = (frame.gprs[0] & 0xff) as u8;
            serial::write_byte(byte);
            USER_BYTES_WRITTEN.fetch_add(1, Ordering::Relaxed);
            frame.gprs[0] = 1;
        }
        // exit. Redirect the `eret` back to the kernel at `el0_return` running
        // at EL1h with all masks clear — the SVC stub's epilog runs unchanged;
        // the eret lands at el0_return; the `ldp + ret` there returns to the
        // caller of `enter_el0`. SPSR_EL1 = 0x5 = EL1h.
        1 => {
            USER_EXITED.store(true, Ordering::Relaxed);
            frame.elr = (&raw const el0_return) as u64;
            frame.spsr = 0x5;
        }
        // getticks (B-HAL.5.1). Return the kernel's accumulated generic-timer
        // tick count — the same atomic the IRQ handler bumps. Confirms an IRQ
        // taken *at EL0* actually went through the EL0→EL1 transition + the
        // shared rust_irq_handler + back to EL0, since the count rises during
        // the user routine's spin (the demo expects ≥1).
        2 => {
            frame.gprs[0] =
                crate::arch::aarch64::vectors::TICK_COUNT.load(Ordering::Relaxed) as u64;
        }
        // Unknown syscall — return -1 in x0 (so the user sees an error if it
        // checks). EL0 resumes; the demo never calls anything else.
        _ => {
            frame.gprs[0] = u64::MAX;
        }
    }

    frame_sp
}

/// Run the B-HAL.5.0 user-mode demo: drop to EL0 into the inline
/// `aarch64_user_demo_entry` routine; it prints "HELLO from EL0\n" byte-by-byte
/// via SVC #0 and exits via SVC #1. Returns when the exit syscall longjmps
/// back through `el0_return`. Reports the round-trip counts so the boot
/// context can confirm the SVC path serviced every byte.
///
/// EL0 enters via the alias VA (`kernel_va + EL0_ALIAS_OFFSET`): the kernel's
/// L1[1] block is AP=00 (no EL0 access — required because QEMU's cortex-a72
/// would otherwise fault EL1 instruction fetches from the same block); a
/// separate L1[2] alias of the same RAM is AP=01, EL0 enters through there.
pub fn run_el0_demo() {
    use crate::arch::aarch64::mmu::EL0_ALIAS_OFFSET;
    serial::writeln("[el0] entering EL0 via eret...");
    USER_BYTES_WRITTEN.store(0, Ordering::Relaxed);
    USER_EXITED.store(false, Ordering::Relaxed);
    let entry = (&raw const aarch64_user_demo_entry) as u64 + EL0_ALIAS_OFFSET;
    let stack_pa = unsafe { (&raw mut USER_STACK).add(1) } as u64 & !0xF;
    let user_sp = stack_pa + EL0_ALIAS_OFFSET;
    unsafe { enter_el0(entry, user_sp) };
    let bytes = USER_BYTES_WRITTEN.load(Ordering::Relaxed);
    let exited = USER_EXITED.load(Ordering::Relaxed);
    serial::write_str("[el0] back at EL1 — wrote ");
    serial::write_u32_decimal(bytes);
    serial::write_str(" byte(s) via SVC; exit syscall=");
    serial::writeln(if exited { "true" } else { "false" });
    // 23 bytes = "HELLO from EL0\n" (15) + "ticks=" (6) + 1 digit + "\n".
    // The presence of the tick-digit line proves the lower-EL IRQ vector
    // (slot 9) wired correctly: a tick fired *at EL0*, the irq_stub saved+
    // restored the EL0 trap frame, and the user routine kept running.
    if bytes == 23 && exited {
        serial::writeln("[el0] EL0 + SVC roundtrip: ok (IRQs at EL0 too)");
    }
}
