// kernel/src/context.rs
//
// Cooperative context switch (B1 Step 2). Pure native — context switching
// is register/stack manipulation, the territory where a state machine adds
// ceremony without clarity (see architecture.md). The Frame `Scheduler`
// and `Task` systems model policy and lifecycle; this swaps the CPU.
//
// `context_switch(old_rsp, new_rsp)`:
//   1. pushes the six callee-saved GPRs of the *current* thread onto its
//      stack,
//   2. stores the resulting rsp into `*old_rsp` (so we can resume here),
//   3. loads `new_rsp`, pops that thread's six callee-saved GPRs, and
//      `ret`s — into wherever that thread last switched out from, or, for a
//      freshly `init_stack`-ed thread, into its entry function.
//
// Caller-saved registers are preserved by the Rust/SysV calling convention
// at the call site (the compiler spills anything live across the call), so
// we only need the six callee-saved GPRs + rsp. SysV passes arg0 in rdi
// (`old_rsp`) and arg1 in rsi (`new_rsp`).

use core::arch::global_asm;

global_asm!(
    ".global context_switch",
    "context_switch:",
    "  push rbp",
    "  push rbx",
    "  push r12",
    "  push r13",
    "  push r14",
    "  push r15",
    "  mov [rdi], rsp", // *old_rsp = current rsp (after the six pushes)
    "  mov rsp, rsi",   // switch to the new thread's stack
    "  pop r15",
    "  pop r14",
    "  pop r13",
    "  pop r12",
    "  pop rbx",
    "  pop rbp",
    "  ret", // return into the new thread
);

extern "C" {
    /// Save the current thread's context to `*old_rsp` and switch to the
    /// thread whose saved stack pointer is `new_rsp`. Returns (to the
    /// *original* caller) only when some other thread switches back to it.
    ///
    /// # Safety
    /// `old_rsp` must be a valid, writable `*mut u64`. `new_rsp` must be a
    /// stack pointer previously produced by `init_stack` or saved by a
    /// prior `context_switch`. Single-core only at B1 Step 2 (no timer, no
    /// concurrent access to the rsp slots).
    pub fn context_switch(old_rsp: *mut u64, new_rsp: u64);
}

/// Craft a fresh thread's initial stack so the first `context_switch` into
/// it pops six zeroed callee-saved registers and `ret`s to `entry`.
///
/// Layout at the returned rsp (low → high addresses), 16-byte aligned:
/// ```text
///   [r15=0][r14=0][r13=0][r12=0][rbx=0][rbp=0][ret=entry][pad]
/// ```
/// `context_switch` pops the six registers (48 bytes) then `ret` pops the
/// entry address (8 bytes), leaving rsp = base + 56 ≡ 8 (mod 16) on entry —
/// the SysV convention for a function entry point.
///
/// # Safety
/// `stack_top` must point one byte past a writable stack region of at least
/// 64 bytes (in practice a whole per-thread stack). `entry` must never
/// return — if it did, `ret` would pop garbage.
pub unsafe fn init_stack(stack_top: *mut u8, entry: extern "C" fn() -> !) -> u64 {
    let aligned_top = (stack_top as u64) & !0xF;
    let base = aligned_top - 64; // 16-aligned: 7 used 8-byte slots + 1 pad
    let slots = base as *mut u64;
    slots.add(0).write(0); // r15
    slots.add(1).write(0); // r14
    slots.add(2).write(0); // r13
    slots.add(3).write(0); // r12
    slots.add(4).write(0); // rbx
    slots.add(5).write(0); // rbp
    slots.add(6).write(entry as usize as u64); // return address → entry
    base
}
