/* libc/csrc/crtn.s (B11-3d) — `.init`/`.fini` section *epilogues*.
 *
 * Closes the `_init`/`_fini` functions crti.s opened: undo the stack alignment
 * and return. See crti.s for why these exist on Frame OS (tcc's standard link
 * pulls them; frame-libc never calls them). */
    .section .init,"ax",@progbits
    add $8, %rsp
    ret

    .section .fini,"ax",@progbits
    add $8, %rsp
    ret
