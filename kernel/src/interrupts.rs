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
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::hal::Cpu;
use crate::serial;

/// Master-PIC vector offset; IRQ0 (PIT timer) lands here.
const TIMER_VECTOR: usize = crate::pic::PIC1_OFFSET as usize;
/// virtio-blk is on IRQ11 (slave PIC) → vector 0x20 + 11 (B4).
const VIRTIO_BLK_VECTOR: usize = crate::pic::PIC1_OFFSET as usize + 11;
/// COM1 serial RX is on IRQ4 (master PIC) → vector 0x20 + 4 (B8).
const SERIAL_VECTOR: usize = crate::pic::PIC1_OFFSET as usize + 4;
/// LAPIC timer vector (B7 Step 4) — the APs' per-core periodic timer. Must match
/// `crate::lapic::TIMER_VECTOR`.
const LAPIC_TIMER_VECTOR: usize = 0x40;
/// LAPIC spurious-interrupt vector — a present no-op gate (must match the
/// spurious vector programmed into the LAPIC SVR).
const SPURIOUS_VECTOR: usize = 0xFF;
/// TLB-shootdown IPI vector (B7 Step 5).
const TLB_SHOOTDOWN_VECTOR: usize = 0x41;

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

    /// Like `set`, but routes the gate to Interrupt Stack Table slot `ist`
    /// (1..=7): the CPU loads RSP from this core's `TSS.ist[ist-1]` on entry,
    /// switching to a known-good stack (R5b — used by the #DF handler).
    fn set_ist(&mut self, handler: u64, selector: u16, ist: u8) {
        self.set(handler, selector);
        self.ist = ist & 0x7;
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
    // Double fault (#DF, vector 8). Routed through IST1 (this core's #DF stack)
    // so it runs on a known-good stack even if the interrupted stack is corrupt.
    // #DF is not recoverable: report which core and halt. The CPU pushed an
    // (always-zero) error code; we never return, so we don't clean it up. `cli`
    // + 16-align before the call (SysV).
    ".global isr_double_fault",
    "isr_double_fault:",
    "  cli",
    "  and rsp, -16",
    "  call double_fault_handler",
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
    // virtio-blk IRQ (vector 43). Save caller-saved GPRs, call the Rust post
    // handler (read the device ISR, record the completion, EOI the PIC), then
    // restore + iretq. The handler does NOT dispatch any Frame system — it
    // only `post`s; the kernel `drain`s from normal context (B4 post/drain).
    ".global isr_virtio_blk",
    "isr_virtio_blk:",
    "  push rax",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  call virtio_blk_irq",
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
    // Serial RX IRQ (B8, IRQ4 → vector 0x24). Same minimal post shape: save
    // caller-saved GPRs, call the Rust half (drain the UART FIFO into the console
    // line buffer + echo, EOI the PIC), restore + iretq. No Frame dispatch — only
    // `post` (the console line buffer); the `read_line` syscall drains it.
    ".global isr_serial",
    "isr_serial:",
    "  push rax",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  call serial_irq",
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
    // virtio-net IRQ (B5). Same shape as virtio-blk: save caller-saved GPRs,
    // call the Rust post handler (read the device ISR, flag a pending event,
    // EOI the PIC), restore + iretq. No Frame dispatch here — only `post`.
    ".global isr_virtio_net",
    "isr_virtio_net:",
    "  push rax",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  call virtio_net_irq",
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
    // LAPIC timer (B7 Step 4 / R1b, vector 0x40) — the application processors'
    // own periodic timer, now a full-frame preemptive switch like `isr_timer`.
    // Save ALL 15 GPRs on top of the CPU's iretq frame, pass rsp to
    // `lapic_schedule`, switch rsp to whatever it returns (the next per-core
    // thread, or the same context when per-core scheduling is inactive), restore
    // the 15 GPRs, iretq. `lapic_schedule` does the per-CPU tick + LAPIC EOI and,
    // when this core's run queue is active (R1b), the round-robin pick.
    ".global isr_lapic_timer",
    "isr_lapic_timer:",
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
    "  call lapic_schedule",
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
    // Spurious interrupt (vector 0xFF) — the LAPIC delivers this if an interrupt
    // is withdrawn before being serviced. By spec it must NOT be EOI'd; just
    // return. A present gate here keeps a spurious IRQ from faulting.
    ".global isr_spurious",
    "isr_spurious:",
    "  iretq",
    // TLB shootdown IPI (B7 Step 5, vector 0x41). Same minimal save shape; the
    // Rust half invalidates the shootdown VA on this core, acks, and EOIs the
    // LAPIC. This is how the initiator flushes other cores' stale translations.
    ".global isr_tlb_shootdown",
    "isr_tlb_shootdown:",
    "  push rax",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  call tlb_shootdown_irq",
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
    // Page fault (#PF, vector 14). Unlike most exceptions, #PF pushes an
    // error code (below the iretq frame), and the faulting address is in
    // CR2. Pass both to the Rust handler; it returns (recovered → retry) or
    // halts (fatal). Before iretq we discard the error code so rsp points at
    // the iretq frame. rbx (callee-saved, preserved across the call) holds
    // rsp across the alignment.
    ".global isr_page_fault",
    "isr_page_fault:",
    "  push rax",
    "  push rcx",
    "  push rdx",
    "  push rsi",
    "  push rdi",
    "  push r8",
    "  push r9",
    "  push r10",
    "  push r11",
    "  push rbx",
    "  mov rdi, cr2",      // arg0 = faulting address
    "  mov rsi, [rsp+80]", // arg1 = error code (10 pushes = 80 bytes above)
    "  mov rdx, [rsp+88]", // arg2 = faulting RIP (iretq frame, just above err code)
    "  mov rbx, rsp",      // save rsp across alignment
    "  and rsp, -16",      // align for the SysV call
    "  call page_fault_handler",
    "  mov rsp, rbx", // restore rsp
    "  pop rbx",
    "  pop r11",
    "  pop r10",
    "  pop r9",
    "  pop r8",
    "  pop rdi",
    "  pop rsi",
    "  pop rdx",
    "  pop rcx",
    "  pop rax",
    "  add rsp, 8", // discard the error code
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
    fn isr_page_fault();
    fn isr_virtio_blk();
    fn isr_virtio_net();
    fn isr_lapic_timer();
    fn isr_spurious();
    fn isr_tlb_shootdown();
    fn isr_double_fault();
    fn isr_serial();
}

/// Rust half of the serial RX ISR (B8). Drains every byte the UART has buffered
/// into the console line discipline (which echoes + accumulates lines), then EOIs
/// the PIC. Pure native — only `post`s into the console buffer; the `read_line`
/// syscall drains it. (IRQ4 is on the master PIC.)
#[no_mangle]
extern "C" fn serial_irq() {
    while let Some(b) = crate::serial::rx_byte() {
        crate::console::feed(b);
    }
    crate::pic::eoi_master();
}

/// Rust half of the #DF handler (R5b). Runs on this core's IST1 stack. A double
/// fault is unrecoverable; report which core hit it and halt (so it surfaces in
/// the serial log instead of escalating to a triple fault / silent reboot).
#[no_mangle]
extern "C" fn double_fault_handler() -> ! {
    crate::serial::write_str("[df] double fault on core ");
    crate::serial::write_u32_decimal(crate::percpu::this_cpu_index());
    crate::serial::writeln(" — halting");
    loop {
        crate::hal::cpu().halt();
    }
}

/// Rust half of the LAPIC-timer ISR (B7 Step 4 / R1b). Records a per-CPU tick
/// (proof this core was preempted), and — when this core's per-core run queue is
/// active (R1b) — round-robins to the next runnable thread, returning its stack
/// pointer for the asm stub to switch to. EOIs the *LAPIC* (not the PIC) before
/// returning. When per-core scheduling is inactive (B7 Step 4 busy loop, R1a),
/// it returns `current_rsp` unchanged, so the interrupted context just resumes.
/// Pure native: never dispatches a Frame system from interrupt context.
#[no_mangle]
extern "C" fn lapic_schedule(current_rsp: u64) -> u64 {
    crate::percpu::record_tick();
    let cpu = crate::percpu::this_cpu_index() as usize;
    let next_rsp = if crate::pcsched::active(cpu) {
        crate::pcsched::schedule(cpu, current_rsp)
    } else {
        current_rsp
    };
    crate::lapic::eoi();
    next_rsp
}

// B7 Step 5: TLB shootdown state. The initiator stores the VA to flush, resets
// the ack count, and IPIs the other cores; each core's shootdown ISR invalidates
// the VA in its own TLB and acks. The initiator waits for all acks (the barrier)
// before reusing the page.
static SHOOTDOWN_VA: AtomicU64 = AtomicU64::new(0);
static SHOOTDOWN_ACKS: AtomicUsize = AtomicUsize::new(0);

/// Initiator side (B7 Step 5): flush `va` from every *other* core's TLB. Sets the
/// VA, resets the ack count, and sends the shootdown IPI to all-but-self. The
/// caller then polls `shootdown_acks()` until it equals the number of other
/// cores (the barrier) before reusing the page. The initiator must flush its own
/// TLB separately (e.g. via `Mmu::unmap`, which `invlpg`s).
pub fn shootdown(va: u64) {
    SHOOTDOWN_VA.store(va, Ordering::SeqCst);
    SHOOTDOWN_ACKS.store(0, Ordering::SeqCst);
    crate::lapic::send_ipi_all_but_self(TLB_SHOOTDOWN_VECTOR as u32);
}
/// How many cores have acked the current shootdown.
pub fn shootdown_acks() -> usize {
    SHOOTDOWN_ACKS.load(Ordering::SeqCst)
}

/// Rust half of the TLB-shootdown ISR (B7 Step 5). Invalidates the shootdown VA
/// on this core, acks, and EOIs the LAPIC.
#[no_mangle]
extern "C" fn tlb_shootdown_irq() {
    let va = SHOOTDOWN_VA.load(Ordering::SeqCst);
    unsafe { asm!("invlpg [{}]", in(reg) va, options(nostack, preserves_flags)) };
    SHOOTDOWN_ACKS.fetch_add(1, Ordering::SeqCst);
    crate::lapic::eoi();
}

/// Rust half of the virtio-blk IRQ stub (B4). Posts the completion (native,
/// interrupt-safe) and EOIs the slave PIC. Never touches a Frame system.
#[no_mangle]
extern "C" fn virtio_blk_irq() {
    crate::virtio_blk::on_irq();
    crate::pic::eoi_slave();
}

/// Rust half of the virtio-net IRQ stub (B5). Posts a pending network event
/// (native, interrupt-safe — no Frame dispatch) and EOIs the PIC on whichever
/// line the device landed (read from PCI config; master or slave).
#[no_mangle]
extern "C" fn virtio_net_irq() {
    crate::virtio_net::on_irq();
    crate::pic::eoi_for(crate::virtio_net::irq_line());
}

/// Wire the virtio-net ISR at runtime, once its IRQ line is known (read from
/// PCI config at net init). The IDT is live (already `lidt`'d), so updating a
/// gate takes effect immediately. virtio-net's line isn't fixed like the
/// timer's or virtio-blk's, so it can't be set in `init()`.
pub fn wire_virtio_net(irq: u8) {
    let cs = read_cs();
    unsafe {
        let idt = &raw mut IDT;
        (*idt)[crate::pic::PIC1_OFFSET as usize + irq as usize]
            .set(isr_virtio_net as *const () as usize as u64, cs);
    }
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
    let pf = isr_page_fault as *const () as usize as u64;

    unsafe {
        let idt = &raw mut IDT;
        // CPU exceptions 0..32 → safety-net handler, except 3 (breakpoint).
        for v in 0..32usize {
            (*idt)[v].set(exc, cs);
        }
        (*idt)[3].set(bp, cs);
        // Double fault (#DF, vector 8) → per-core IST1 stack (R5b), so it runs on
        // a known-good stack instead of triple-faulting.
        (*idt)[8].set_ist(isr_double_fault as *const () as usize as u64, cs, 1);
        // Page fault (#PF) → demand-paging / fatal classifier.
        (*idt)[14].set(pf, cs);
        // IRQ0 timer.
        (*idt)[TIMER_VECTOR].set(timer, cs);
        // virtio-blk IRQ (B4).
        (*idt)[VIRTIO_BLK_VECTOR].set(isr_virtio_blk as *const () as usize as u64, cs);
        // COM1 serial RX (B8) — the interactive console's input IRQ.
        (*idt)[SERIAL_VECTOR].set(isr_serial as *const () as usize as u64, cs);
        // LAPIC timer (B7 Step 4) — the APs' per-core timer; the spurious vector
        // gets a present (no-op) gate so a withdrawn IRQ can't fault.
        (*idt)[LAPIC_TIMER_VECTOR].set(isr_lapic_timer as *const () as usize as u64, cs);
        (*idt)[SPURIOUS_VECTOR].set(isr_spurious as *const () as usize as u64, cs);
        // TLB-shootdown IPI (B7 Step 5).
        (*idt)[TLB_SHOOTDOWN_VECTOR].set(isr_tlb_shootdown as *const () as usize as u64, cs);

        let idtr = Idtr {
            limit: (core::mem::size_of::<[IdtEntry; 256]>() - 1) as u16,
            base: idt as u64,
        };
        asm!("lidt [{}]", in(reg) &idtr, options(readonly, nostack, preserves_flags));
    }
}

/// Load the (already-built) IDT on an application processor. The BSP's `init()`
/// built the table; an AP just points its IDTR at it with `lidt` before it
/// enables interrupts. (B7 Step 4.)
pub fn load_idt_on_ap() {
    unsafe {
        let idt = &raw const IDT;
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

// The interrupt-control primitives below are the arch-agnostic named wrappers
// over the HAL CPU seam (`hal::Cpu`); the actual sti/cli/hlt mechanism lives in
// `arch/<isa>/cpu.rs`. They keep their long-standing names so their many
// callers are unchanged (B-HAL.1 Cpu seam).

/// Enable maskable interrupts.
pub fn enable() {
    crate::hal::cpu().enable_irqs();
}

/// Disable maskable interrupts.
pub fn disable() {
    crate::hal::cpu().disable_irqs();
}

/// Halt until the next interrupt. With interrupts enabled this wakes on the
/// next timer IRQ — used to wait for ticks without busy-spin.
pub fn wait_for_interrupt() {
    crate::hal::cpu().halt();
}

/// Enable interrupts and halt as one step. Used by a blocking task (B3 Step 5d
/// `wait`) to yield to the scheduler from an interrupts-off (syscall) context:
/// the enable-then-halt pair leaves no wake-losing window.
pub fn wait_for_interrupt_enabled() {
    crate::hal::cpu().enable_irqs_and_halt();
}

/// Run `f` with interrupts disabled, restoring the previous interrupt-enable
/// state afterward. Single-core mutual exclusion: a Frame system is
/// non-reentrant, so when one is shared across preemptible threads (e.g. the
/// `Scheduler`), every dispatch must run in such a critical section or a
/// timer preemption mid-dispatch would corrupt it.
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let was_enabled = crate::hal::cpu().irqs_enabled();
    crate::hal::cpu().disable_irqs();
    let r = f();
    // Only re-enable if interrupts were enabled on entry.
    if was_enabled {
        crate::hal::cpu().enable_irqs();
    }
    r
}
