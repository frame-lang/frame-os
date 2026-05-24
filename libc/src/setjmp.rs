//! setjmp/longjmp (B11-3c) — tcc's error-recovery funnel: a parse/codegen error
//! `longjmp`s back to the per-file `setjmp`. A hand-written asm pair, since it's
//! pure register save/restore + a non-local jump. The 8-slot jmp_buf (see
//! libc/include/setjmp.h) holds the callee-saved registers SysV requires a
//! function to preserve (rbx, rbp, r12-r15) plus the caller's rsp and the
//! return address; restoring them + jumping to the saved rip resumes the
//! setjmp caller as if setjmp had just returned `val`.

use core::arch::global_asm;

global_asm!(
    ".global setjmp",
    ".global _setjmp", // gcc lowers a bare `setjmp` call to `_setjmp`
    "setjmp:",
    "_setjmp:",
    "  mov [rdi + 0], rbx",
    "  mov [rdi + 8], rbp",
    "  mov [rdi + 16], r12",
    "  mov [rdi + 24], r13",
    "  mov [rdi + 32], r14",
    "  mov [rdi + 40], r15",
    "  lea rax, [rsp + 8]", // the caller's rsp (above this call's return addr)
    "  mov [rdi + 48], rax",
    "  mov rax, [rsp]", // the return address = where setjmp's caller resumes
    "  mov [rdi + 56], rax",
    "  xor eax, eax", // direct call returns 0
    "  ret",
    ".global longjmp",
    "longjmp:",
    "  mov rbx, [rdi + 0]",
    "  mov rbp, [rdi + 8]",
    "  mov r12, [rdi + 16]",
    "  mov r13, [rdi + 24]",
    "  mov r14, [rdi + 32]",
    "  mov r15, [rdi + 40]",
    "  mov rsp, [rdi + 48]",
    "  mov eax, esi", // return `val` from the resumed setjmp
    "  test eax, eax",
    "  jnz 1f",
    "  inc eax", // longjmp(env, 0) must make setjmp return 1, not 0
    "1:",
    "  jmp qword ptr [rdi + 56]",
);
