// kernel/src/arch/aarch64/boot.rs
//
// AArch64 boot entry (B-HAL.3.1/.2). QEMU's `virt` machine, launched with
// `-kernel <elf>`, loads this ELF at its link address (0x4008_0000, see
// linker-aarch64.ld) and enters at `_start` in EL1 with a pointer to the
// flattened device tree in x0. Unlike x86 (Limine sets up a stack + long mode),
// here `_start` must establish its own stack and zero .bss before any Rust runs.
//
// This is the AArch64 analogue of the x86 Limine handoff → `kmain`. For the
// skeleton, `kmain` brings up the PL011 console (via the shared `serial.rs`
// text layer over `hal::Console`), prints the banner, and parks. Scheduling,
// MMU, GIC, and user mode arrive in B-HAL.3.3+/.4/.5.

use core::arch::global_asm;

// `_start`: set SP to the linker-provided stack top, zero the BSS, branch to the
// Rust entry. Kept in `.text.boot` so the linker places it at the ELF entry.
// (x0 holds the DTB pointer QEMU passed; preserved for B-HAL.3.3 — the skeleton
// does not read it yet.)
global_asm!(
    ".section .text.boot",
    ".global _start",
    "_start:",
    // Enable FP/SIMD access at EL1 (CPACR_EL1.FPEN = 0b11). The aarch64 target
    // emits NEON/SIMD registers (memcpy/memset, byte ops), which trap by default
    // (CPACR_EL1.FPEN = 0) — the ARM analogue of enabling SSE on x86. Without
    // this, the first SIMD instruction in Rust faults (ESR EC=0x7).
    "  mov  x9, #(3 << 20)",
    "  msr  cpacr_el1, x9",
    "  isb",
    // Stack pointer ← __stack_top. adrp/add (PC-relative) avoids a literal pool.
    "  adrp x9, __stack_top",
    "  add  x9, x9, :lo12:__stack_top",
    "  mov  sp, x9",
    // Zero .bss: [__bss_start, __bss_end), 16-byte aligned, 8 bytes/step.
    "  adrp x9, __bss_start",
    "  add  x9, x9, :lo12:__bss_start",
    "  adrp x10, __bss_end",
    "  add  x10, x10, :lo12:__bss_end",
    "1:",
    "  cmp x9, x10",
    "  b.hs 2f",
    "  str xzr, [x9], #8",
    "  b 1b",
    "2:",
    "  bl kmain",
    // kmain is `-> !`; if it ever returned, park rather than run off the end.
    "3:",
    "  wfe",
    "  b 3b",
);

// `__stack_top` is provided by `linker-aarch64.ld` — the address just past the
// kernel image + initial stack. RAM at or above this is free for the frame
// allocator to manage (B-HAL.4.1).
extern "C" {
    static __stack_top: u8;
}

/// The AArch64 kernel entry, called by `_start` once SP + BSS are set up. `dtb`
/// is the flattened-device-tree pointer QEMU passed in x0 (preserved through
/// `_start`, which clobbers only x9/x10).
///
/// # Safety
/// Called once at startup; never re-entered. SP and .bss are established by
/// `_start` immediately before this; `dtb` is whatever the firmware passed.
#[no_mangle]
unsafe extern "C" fn kmain(dtb: usize) -> ! {
    use crate::arch::aarch64::fdt;
    use crate::serial;

    serial::init_uart(); // enable the PL011 (hal::Console::init)
    serial::writeln("");
    serial::writeln("Frame OS kernel — AArch64 skeleton (B-HAL.3)");
    serial::writeln("[aarch64] PL011 console up via hal::Console");

    serial::write_str("[aarch64] x0/dtb = 0x");
    serial::write_hex_u64(dtb as u64);
    serial::writeln("");

    // B-HAL.3.3: locate the device tree. QEMU `virt` passes its address in x0
    // only for the Linux Image boot protocol; a bare `-kernel <ELF>` entered at
    // its entry gets neither x0 nor a DTB auto-loaded into RAM. So: use x0 if the
    // firmware set it (real hardware / a future Image boot), else scan the RAM
    // window for the FDT magic — the test harness places the DTB at a fixed
    // address via QEMU `-device loader`. Never dereference a null/garbage pointer.
    const RAM_BASE: usize = 0x4000_0000;
    const SCAN_LEN: usize = 128 * 1024 * 1024; // default `virt` RAM size
    let dtb_ptr: *const u8 = if dtb != 0 && unsafe { fdt::valid(dtb as *const u8) } {
        dtb as *const u8
    } else {
        match unsafe { fdt::find(RAM_BASE, SCAN_LEN) } {
            Some(p) => {
                serial::write_str("[aarch64] DTB located by RAM scan @ 0x");
                serial::write_hex_u64(p as u64);
                serial::writeln("");
                p
            }
            None => core::ptr::null(),
        }
    };
    let mut mem_region: Option<(u64, u64)> = None;
    let mut dtb_size: u64 = 0;
    if !dtb_ptr.is_null() && unsafe { fdt::valid(dtb_ptr) } {
        dtb_size = unsafe { fdt::total_size(dtb_ptr) } as u64;
        serial::write_str("[aarch64] DTB @ 0x");
        serial::write_hex_u64(dtb_ptr as u64);
        serial::write_str(", totalsize 0x");
        serial::write_hex_u64(dtb_size);
        serial::writeln("");
        match unsafe { fdt::memory_region(dtb_ptr) } {
            Some((base, size)) => {
                serial::write_str("[aarch64] RAM base 0x");
                serial::write_hex_u64(base);
                serial::write_str(" size 0x");
                serial::write_hex_u64(size);
                serial::write_str(" (");
                serial::write_u32_decimal((size / (1024 * 1024)) as u32);
                serial::writeln(" MiB)");
                mem_region = Some((base, size));
            }
            None => serial::writeln("[aarch64] no /memory node in DTB"),
        }
    } else {
        serial::writeln("[aarch64] no DTB found (x0=0, no FDT magic in RAM scan)");
    }

    // Install the EL1 exception vectors *before* MMU enable so any fault on
    // the MMU-enable instruction (or after) lands at a real handler instead of
    // PC=0 (which is in the device-memory region post-MMU and would generate
    // an instruction-abort loop). The vectors themselves live in the kernel
    // image and have valid PAs both before and after translation (identity
    // map), so `msr vbar_el1, vectors` set here stays correct across enable.
    unsafe { crate::arch::aarch64::vectors::install() };

    // B-HAL.3.4: enable the MMU (identity map). That this line — and the halt
    // banner below — still reach the PL011 proves translation is live and the
    // device/normal mappings are correct (the console runs translated now).
    unsafe { crate::arch::aarch64::mmu::enable() };
    serial::writeln("[aarch64] MMU enabled (identity map via TTBR0)");

    // B-HAL.3.5: install EL1 vectors, bring up the GIC + the ARM generic timer,
    // unmask DAIF.I, and spin in `wfi` until the IRQ handler has counted a few
    // ticks. Taking a real interrupt on a second ISA proves the Irq/Timer trait
    // shapes against actual hardware (the contracts deferred from B-HAL.2).
    use crate::arch::aarch64::{gic, timer, vectors};
    use core::arch::asm;
    use core::sync::atomic::Ordering;
    const TARGET_TICKS: u32 = 3;
    unsafe {
        // Vectors are already installed (above, pre-MMU); GIC + timer next.
        gic::init();
        timer::init(10); // 10 Hz tick
        gic::unmask(timer::TIMER_IRQ);
        asm!("msr daifclr, #2", options(nomem, nostack)); // unmask IRQs
    }
    serial::writeln("[aarch64] GIC + generic timer up; awaiting ticks");
    while vectors::TICK_COUNT.load(Ordering::Relaxed) < TARGET_TICKS {
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
    unsafe { asm!("msr daifset, #2", options(nomem, nostack)) }; // mask IRQs
    serial::write_str("[aarch64] generic-timer fired ");
    serial::write_u32_decimal(vectors::TICK_COUNT.load(Ordering::Relaxed));
    serial::writeln(" ticks");

    // B-HAL.4.0: per-CPU base register (TPIDR_EL1) — the ARM analogue of x86's
    // GS base. Initialize this core's PerCpu block, then read its index back
    // through the base register. Same arch-agnostic `percpu` data layer the
    // x86 boot path uses; the seam underneath is the only ISA-specific bit.
    crate::percpu::init_this_cpu(0, 0);
    serial::write_str("[aarch64] this_cpu_index = ");
    serial::write_u32_decimal(crate::percpu::this_cpu_index());
    serial::writeln("");

    // B-HAL.4.1: physical frame allocator. The x86 boot path takes its USABLE
    // regions from Limine; here we carve one from the FDT `/memory` node, minus
    // the kernel image (everything below `__stack_top`) and the DTB. The
    // allocator's bitmap + alloc/free is the same code on both ISAs — only the
    // memory-map source differs. HHDM offset = 0: aarch64 boots through an
    // identity map (B-HAL.3.4), so phys == virt in the kernel.
    if let Some((ram_base, ram_size)) = mem_region {
        let kernel_end = page_align_up((&__stack_top as *const u8) as u64);
        let ram_end = ram_base + ram_size;
        let dtb_start = if dtb_size > 0 {
            page_align_down(dtb_ptr as u64)
        } else {
            ram_end
        };
        let dtb_end = if dtb_size > 0 {
            page_align_up(dtb_ptr as u64 + dtb_size)
        } else {
            ram_end
        };
        // Two usable runs around the DTB (whichever is non-empty). Defensive
        // ordering: only include a run if its end actually exceeds its start.
        let mut regions: [(u64, u64); 2] = [(0, 0); 2];
        let mut n = 0usize;
        if dtb_start > kernel_end {
            regions[n] = (kernel_end, dtb_start - kernel_end);
            n += 1;
        }
        if ram_end > dtb_end {
            regions[n] = (dtb_end, ram_end - dtb_end);
            n += 1;
        }
        crate::frames::init_from_regions(&regions[..n], 0);

        serial::write_str("[aarch64] frames usable: ");
        serial::write_u32_decimal(crate::frames::free_count() as u32);
        serial::writeln("");
        let before = crate::frames::free_count();
        let f1 = crate::frames::alloc_frame().expect("frame alloc");
        let f2 = crate::frames::alloc_frame().expect("frame alloc");
        if f1 != f2 && f1 % 4096 == 0 && f2 % 4096 == 0 && crate::frames::free_count() == before - 2
        {
            serial::writeln("[frames] alloc two distinct frames: ok");
        }
        crate::frames::free_frame(f1);
        crate::frames::free_frame(f2);
        if crate::frames::free_count() == before {
            serial::writeln("[frames] free restores count: ok");
        }
    } else {
        serial::writeln("[aarch64] frame allocator skipped (no /memory in DTB)");
    }

    // B-HAL.4.2: global allocator (heap). Same arch-agnostic
    // `allocator.rs` the x86 boot uses — a static 8 MiB BSS buffer behind
    // `linked_list_allocator`, wrapped in a counting `GlobalAlloc`. Calling
    // `init()` lets `Box`/`Vec`/`Rc` work — the substrate the Frame-generated
    // systems' event + compartment plumbing needs (B-HAL.4.3+ on aarch64).
    crate::allocator::init();
    {
        use alloc::boxed::Box;
        use alloc::vec::Vec;
        let allocs_before = crate::allocator::alloc_count();
        let b = Box::new(0xCAFE_F00D_u32);
        let mut v: Vec<u32> = Vec::with_capacity(8);
        for i in 0..8u32 {
            v.push(i * i);
        }
        let sum: u32 = v.iter().sum();
        let allocs_delta = crate::allocator::alloc_count() - allocs_before;
        if *b == 0xCAFE_F00D && sum == 140 && allocs_delta >= 2 {
            serial::write_str("[heap] Box+Vec round-trip: ok (allocs delta=");
            serial::write_u32_decimal(allocs_delta as u32);
            serial::writeln(")");
        }
        // Drops `b` + `v` here; counter only tracks allocs, so no read-back.
    }

    // B-HAL.4.3: cooperative context switch via `hal::context()`. Two kernel
    // "threads" ping-pong on independent stacks for 5 rounds (mirror of the x86
    // sched_demo, same trait, different ISA underneath). Reaching
    // "[switch] back in main" proves: aarch64 init_stack laid out the 12
    // callee-saved slots + LR=entry correctly, the naked-asm `aarch64_context_switch`
    // saves+restores x19–x30 around the SP swap, and the freshly-init'd thread's
    // first switch lands at `entry` via `ret` consuming x30.
    aarch64_ctx_pingpong();

    // B-HAL.4.4: drive a Frame `Scheduler` on aarch64. *Same generated code*
    // the x86 BSP runs in `sched.rs` (`scheduler.frs` → `scheduler.rs`); the
    // headline claim of the milestone — write the FSM once, run on both ISAs.
    // Trajectory:
    //   __create()     → $Idle    (runnable=0)
    //   task_ready ×3  → $Active  (runnable=3 — peak)
    //   task_unready×3 → $Idle    (runnable=0 — drained)
    // The cooperative ping-pong above proves the *switch primitive* works;
    // this proves the *FSM logic* itself ports — the heap-typed event /
    // compartment / Rc plumbing framec generates compiles and dispatches.
    {
        use crate::frame_systems::Scheduler;
        let mut sched = Scheduler::__create();
        let initial_idle = sched.is_idle();
        sched.task_ready();
        sched.task_ready();
        sched.task_ready();
        let peak = sched.runnable_count();
        let active_after_ready = !sched.is_idle();
        sched.task_unready();
        sched.task_unready();
        sched.task_unready();
        let final_idle = sched.is_idle();
        let final_count = sched.runnable_count();
        serial::write_str("[sched] init idle=");
        serial::writeln(if initial_idle { "true" } else { "false" });
        serial::write_str("[sched] peak runnable=");
        serial::write_u32_decimal(peak);
        serial::write_str(", active=");
        serial::writeln(if active_after_ready { "true" } else { "false" });
        serial::write_str("[sched] drained runnable=");
        serial::write_u32_decimal(final_count);
        serial::write_str(", idle=");
        serial::writeln(if final_idle { "true" } else { "false" });
        if initial_idle && peak == 3 && active_after_ready && final_count == 0 && final_idle {
            serial::writeln("[sched] Frame Scheduler trajectory: ok ($Idle→$Active→$Idle)");
        }
    }

    // B-HAL.5.0: drop to EL0 and run a tiny user routine that prints
    // "HELLO from EL0" byte-by-byte via `svc #0` then exits via `svc #1`.
    // First proof of the user/kernel boundary on aarch64: the EL0→EL1 sync
    // vector + ESR_EL1 decode + per-syscall dispatch + `eret`-back round-trip
    // every call. Runs *before* the preemptive demo so the SVC path is
    // exercised against the IRQs-still-masked boot context (the preemption
    // demo unmasks IRQs and never returns to idle masked).
    crate::arch::aarch64::usermode::run_el0_demo();

    // B-HAL.4.5: timer-driven preemptive scheduling on aarch64. The same
    // pattern x86 uses (`sched.rs` `run()`): spawn two non-yielding workers
    // that print '1'/'2' in busy loops; the generic-timer IRQ preempts mid-
    // spin and `sched_preempt::schedule` rotates between them; each worker
    // exits via `task_unready` after a few rounds; the boot context idles
    // until the Frame Scheduler reports `$Idle`, then returns. Interleaved
    // 1/2/1/2/… output is the proof of preemption.
    crate::arch::aarch64::sched_preempt::run();

    serial::writeln("[aarch64] halting.");
    crate::halt_forever();
}

// ---------------------------------------------------------------------------
// B-HAL.4.3 cooperative ping-pong demo (mirror of x86 sched_demo, on aarch64).
//
// Two threads (A and B) hand control back and forth via `hal::context().switch`
// until ROUNDS reaches MAX_ROUNDS; B's last switch returns to `main` (kmain),
// which prints the closing banner. All three SP slots (MAIN/A/B) are plain
// statics (single-core, no preemption — kmain is alone here).
// ---------------------------------------------------------------------------

const PP_STACK_SIZE: usize = 16 * 1024;
const PP_MAX_ROUNDS: u32 = 5;

static mut PP_STACK_A: [u8; PP_STACK_SIZE] = [0; PP_STACK_SIZE];
static mut PP_STACK_B: [u8; PP_STACK_SIZE] = [0; PP_STACK_SIZE];
static mut PP_MAIN_SP: u64 = 0;
static mut PP_A_SP: u64 = 0;
static mut PP_B_SP: u64 = 0;
static PP_ROUNDS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

extern "C" fn pp_thread_a() -> ! {
    use crate::hal::{self, Context as _};
    loop {
        crate::serial::write_str("A");
        unsafe {
            let b = (&raw const PP_B_SP).read();
            hal::context().switch(&raw mut PP_A_SP, b);
        }
    }
}

extern "C" fn pp_thread_b() -> ! {
    use crate::hal::{self, Context as _};
    use core::sync::atomic::Ordering;
    loop {
        crate::serial::write_str("B");
        let done = PP_ROUNDS.fetch_add(1, Ordering::SeqCst) + 1 >= PP_MAX_ROUNDS;
        unsafe {
            if done {
                let m = (&raw const PP_MAIN_SP).read();
                hal::context().switch(&raw mut PP_B_SP, m);
            } else {
                let a = (&raw const PP_A_SP).read();
                hal::context().switch(&raw mut PP_B_SP, a);
            }
        }
    }
}

fn aarch64_ctx_pingpong() {
    use crate::hal::{self, Context as _};
    crate::serial::writeln("[switch] starting A/B ping-pong");
    unsafe {
        let a_top = (&raw mut PP_STACK_A).add(1) as *mut u8;
        let b_top = (&raw mut PP_STACK_B).add(1) as *mut u8;
        (&raw mut PP_A_SP).write(hal::context().init_stack(a_top, pp_thread_a));
        (&raw mut PP_B_SP).write(hal::context().init_stack(b_top, pp_thread_b));
        let a_start = (&raw const PP_A_SP).read();
        hal::context().switch(&raw mut PP_MAIN_SP, a_start);
    }
    crate::serial::writeln("\n[switch] back in main, demo done");
}

#[inline]
fn page_align_up(addr: u64) -> u64 {
    (addr + 0xFFF) & !0xFFF
}

#[inline]
fn page_align_down(addr: u64) -> u64 {
    addr & !0xFFF
}
