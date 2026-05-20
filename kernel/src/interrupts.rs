// kernel/src/interrupts.rs
//
// IDT + interrupt/exception entry stubs (B1 Step 3).
//
// On stable Rust there is no `extern "x86-interrupt"` ABI (it's a nightly
// feature), so every handler is a naked assembly stub defined in
// `global_asm!` — the same approach as `context.rs`. A stub either:
//   - prints and halts (CPU exceptions — a safety net so a kernel bug
//     surfaces as a serial message instead of a silent triple-fault), or
//   - does its work and `iretq`s back (breakpoint here; the timer ISR at
//     sub-step 3b/3c).
//
// Sub-step 3a (this file's first cut): set up the IDT, install the
// exception safety net on vectors 0..32, install a returning breakpoint
// handler on vector 3, and `lidt`. `kmain` fires `int3` to prove the IDT,
// the gate descriptors, and `iretq` all work before we wire real IRQs.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::serial;

/// Master-PIC vector offset; IRQ0 (PIT timer) lands here.
const TIMER_VECTOR: usize = crate::pic::PIC1_OFFSET as usize;

/// Monotonic timer tick count, incremented by the timer ISR.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Current tick count.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// IDT gate descriptor (x86_64, 16 bytes)
// ---------------------------------------------------------------------------

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        IdtEntry {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            zero: 0,
        }
    }

    fn set(&mut self, handler: u64, selector: u16) {
        self.offset_low = handler as u16;
        self.offset_mid = (handler >> 16) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.selector = selector;
        self.ist = 0;
        // Present, DPL=0, type 0xE = 64-bit interrupt gate.
        self.type_attr = 0x8E;
        self.zero = 0;
    }
}

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: u64,
}

static mut IDT: [IdtEntry; 256] = [IdtEntry::missing(); 256];

// ---------------------------------------------------------------------------
// Naked entry stubs
// ---------------------------------------------------------------------------

global_asm!(
    // Exception safety net: print + halt. Never returns, so no register
    // preservation and no error-code juggling needed (works for both
    // error-code and no-error-code exceptions).
    ".global isr_exception",
    "isr_exception:",
    "  call exception_handler",
    "1:",
    "  hlt",
    "  jmp 1b",
    // Breakpoint (int3): save caller-saved GPRs, print, restore, iretq.
    // 9 caller-saved pushes; the `serial` path uses no SSE so alignment is
    // not load-bearing here.
    ".global isr_breakpoint",
    "isr_breakpoint:",
    "  push rax",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  call breakpoint_handler",
    "  pop r11",
    "  pop r10",
    "  pop r9",
    "  pop r8",
    "  pop rdi",
    "  pop rsi",
    "  pop rdx",
    "  pop rcx",
    "  pop rax",
    "  iretq",
    // Timer IRQ0 (vector 32), full-frame preemptive switch (3c). Save ALL
    // 15 GPRs on top of the CPU's iretq frame, pass rsp to `schedule`,
    // switch rsp to whatever `schedule` returns (the next thread, or the
    // same context when preemption is inactive), restore the 15 GPRs,
    // iretq. `schedule` does the tick count + PIC EOI.
    //
    // The interrupted rsp is arbitrary, so `and rsp, -16` aligns the stack
    // for the SysV `call` (schedule's Rust frame may use SSE). rdi already
    // holds the real rsp; we overwrite rsp with schedule's return anyway.
    ".global isr_timer",
    "isr_timer:",
    "  push rax",
    "  push rbx",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push rbp",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  push r12",
    "  push r13",
    "  push r14",
    "  push r15",
    "  mov rdi, rsp", // arg0 = current rsp (points at saved r15)
    "  and rsp, -16", // align for the call
    "  call schedule",
    "  mov rsp, rax", // switch to the chosen thread's stack
    "  pop r15",
    "  pop r14",
    "  pop r13",
    "  pop r12",
    "  pop r11",
    "  pop r10",
    "  pop r9",
    "  pop r8",
    "  pop rbp",
    "  pop rdi",
    "  pop rsi",
    "  pop rdx",
    "  pop rcx",
    "  pop rbx",
    "  pop rax",
    "  iretq",
);

extern "C" {
    fn isr_exception();
    fn isr_breakpoint();
    fn isr_timer();
}

#[no_mangle]
extern "C" fn exception_handler() {
    serial::writeln("\nKERNEL EXCEPTION — halting");
}

#[no_mangle]
extern "C" fn breakpoint_handler() {
    serial::write_str("[int3 ok]");
}

/// Record a timer tick and acknowledge the PIC. Called by `schedule` (the
/// timer ISR's Rust half) on every IRQ0, whether or not a thread switch
/// happens.
pub fn record_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    crate::pic::eoi_master();
}

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

fn read_cs() -> u16 {
    let cs: u16;
    unsafe {
        asm!("mov {0:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags));
    }
    cs
}

/// Install the IDT and load it. Reuses Limine's GDT by reading the current
/// code segment selector for the interrupt gates.
pub fn init() {
    let cs = read_cs();
    let exc = isr_exception as *const () as usize as u64;
    let bp = isr_breakpoint as *const () as usize as u64;
    let timer = isr_timer as *const () as usize as u64;

    unsafe {
        let idt = &raw mut IDT;
        // CPU exceptions 0..32 → safety-net handler, except 3 (breakpoint).
        for v in 0..32usize {
            (*idt)[v].set(exc, cs);
        }
        (*idt)[3].set(bp, cs);
        // IRQ0 timer.
        (*idt)[TIMER_VECTOR].set(timer, cs);

        let idtr = Idtr {
            limit: (core::mem::size_of::<[IdtEntry; 256]>() - 1) as u16,
            base: idt as u64,
        };
        asm!("lidt [{}]", in(reg) &idtr, options(readonly, nostack, preserves_flags));
    }
}

/// Fire a software breakpoint (`int3`) — used once at boot to validate the
/// IDT path end to end.
pub fn test_breakpoint() {
    unsafe {
        asm!("int3", options(nomem, nostack));
    }
}

/// Enable maskable interrupts (`sti`).
pub fn enable() {
    unsafe {
        asm!("sti", options(nomem, nostack));
    }
}

/// Disable maskable interrupts (`cli`).
pub fn disable() {
    unsafe {
        asm!("cli", options(nomem, nostack));
    }
}

/// Halt until the next interrupt (`hlt`). With interrupts enabled this
/// wakes on the next timer IRQ — used to wait for ticks without busy-spin.
pub fn wait_for_interrupt() {
    unsafe {
        asm!("hlt", options(nomem, nostack));
    }
}
