/* libc/csrc/crt1.s (B11-3d) — the C startup object for tcc-linked programs.
 *
 * tcc links an executable the standard way: `crt1.o crti.o <user.o> crtn.o -lc`.
 * crt1.o owns the program entry `_start`; everything else (argc/argv parsing,
 * std-stream init, calling main, exit) is frame-libc's `__libc_start`, pulled
 * from libc.a. This is byte-for-byte the same `_start` frame-libc's own crt0
 * uses (libc/src/lib.rs) — but that copy is compiled out of the sysroot libc.a
 * (its `crt0` feature is off) so it doesn't collide with this one.
 *
 * At process entry the kernel leaves rsp pointing at the System V initial stack
 * (argc, argv[], NULL, envp[], NULL, auxv). Hand that pointer to __libc_start in
 * rdi and 16-align rsp for the call. __libc_start never returns (it exits). */
    /* Mark `main` hidden so tcc resolves __libc_start's call to it as a direct
       PC32 (no PLT). tcc 0.9.27's PLT for a fully-static x86-64 exe is broken
       (the stub's GOT displacement points back into the PLT), so any call left
       routed through a PLT jumps to garbage. The libc archive's symbols are
       hidden via `ld --exclude-libs=ALL`, but the user program's `main` is a
       default-visibility global; this `.hidden main` constrains the merged
       symbol's visibility to hidden, taking the same no-PLT path. */
    .hidden main
    .text
    .globl _start
_start:
    mov %rsp, %rdi          /* arg0 = &argc (the initial stack) */
    and $-16, %rsp          /* ABI: 16-align before the call */
    call __libc_start
    ud2                     /* __libc_start never returns */
