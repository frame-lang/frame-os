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

/// The AArch64 kernel entry, called by `_start` once SP + BSS are set up.
///
/// # Safety
/// Called once at startup; never re-entered. SP and .bss are established by
/// `_start` immediately before this.
#[no_mangle]
unsafe extern "C" fn kmain() -> ! {
    crate::serial::init_uart(); // enable the PL011 (hal::Console::init)
    crate::serial::writeln("");
    crate::serial::writeln("Frame OS kernel — AArch64 skeleton (B-HAL.3)");
    crate::serial::writeln("[aarch64] PL011 console up via hal::Console; halting.");
    crate::halt_forever();
}
