/*
 * Linker script for the Frame OS aarch64 user-mode demo (B-HAL.5.2).
 *
 * Links at a fixed VA in the kernel's EL0 alias region — L1[2] maps VA
 * [2 GiB, 3 GiB) to PA [1 GiB, 2 GiB) with AP=01 (EL0 R/W/exec), so a VA in
 * that range is reachable from EL0 *and* corresponds to a known RAM PA. We
 * pick 0x8000_0000 + offset where the offset is chosen to stay clear of the
 * kernel image + heap + stacks (which sit at PA 0x40080000 + ~9-10 MiB).
 * 0x80A00000 = PA 0x40A00000 — ~10 MiB past the kernel's BSS, plenty of slack.
 *
 * Two PT_LOAD segments with explicit flags so a future per-page-permission
 * loader can set them per p_flags: text is R+X, data is R+W.
 */

OUTPUT_FORMAT(elf64-littleaarch64)
ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);   /* PF_R | PF_X */
    data PT_LOAD FLAGS(6);   /* PF_R | PF_W */
}

SECTIONS
{
    . = 0x80A00000;

    .text : {
        *(.text.boot)
        *(.text .text.*)
    } :text

    .rodata : {
        *(.rodata .rodata.*)
    } :text

    . = ALIGN(0x1000);

    .data : {
        *(.data .data.*)
    } :data

    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
        *(.comment)
    }
}
