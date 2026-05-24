/* libc/csrc/crti.s (B11-3d) — `.init`/`.fini` section *prologues*.
 *
 * The canonical glibc crti: it opens the `_init` and `_fini` functions (the
 * stack-aligning prologue); crtn.s closes them with the matching epilogue +
 * ret. frame-libc's `__libc_start` does not call _init/_fini, so these are
 * effectively unused on Frame OS — but tcc's standard link always pulls
 * crti.o/crtn.o, so they must exist and be well-formed. */
    .section .init,"ax",@progbits
    .globl _init
    .type _init,@function
_init:
    sub $8, %rsp            /* 16-align the stack inside _init */

    .section .fini,"ax",@progbits
    .globl _fini
    .type _fini,@function
_fini:
    sub $8, %rsp
