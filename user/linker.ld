/*
 * Linker script for the Frame OS freestanding user program (B3 Step 4).
 *
 * Links at a fixed low-half virtual base (0x1000_0000) — a USER-accessible
 * canonical address proven free in the kernel's address space (the B3 Step 1
 * demo mapped its hand-asm blob there). The kernel's ElfLoader maps each
 * PT_LOAD segment at its p_vaddr, so this base is where the program lands.
 *
 * Two PT_LOAD segments with explicit flags so ElfLoader can set page
 * permissions from p_flags: text is R+X (no WRITABLE), data is R+W.
 */

OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);   /* PF_R | PF_X */
    data PT_LOAD FLAGS(6);   /* PF_R | PF_W */
}

SECTIONS
{
    . = 0x10000000;

    .text : {
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
