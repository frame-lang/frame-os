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
    if !dtb_ptr.is_null() && unsafe { fdt::valid(dtb_ptr) } {
        serial::write_str("[aarch64] DTB @ 0x");
        serial::write_hex_u64(dtb_ptr as u64);
        serial::write_str(", totalsize 0x");
        serial::write_hex_u64(unsafe { fdt::total_size(dtb_ptr) } as u64);
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
            }
            None => serial::writeln("[aarch64] no /memory node in DTB"),
        }
    } else {
        serial::writeln("[aarch64] no DTB found (x0=0, no FDT magic in RAM scan)");
    }

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
        vectors::install();
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

    serial::writeln("[aarch64] halting.");
    crate::halt_forever();
}
