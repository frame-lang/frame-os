/*
 * Frame OS kernel linker script (AArch64, QEMU `virt` direct `-kernel` boot).
 *
 * Unlike the x86 build (Limine, higher-half), the AArch64 skeleton is loaded
 * directly by QEMU at the `virt` machine's RAM base region and entered at its
 * ELF entry point (`_start`) in EL1, with a pointer to the flattened device
 * tree in x0. We link at 0x4008_0000 (RAM base 0x4000_0000 + 512 KiB slack),
 * the conventional AArch64 kernel load address for `virt`.
 *
 * `_start` (in .text.boot, kept first) sets up the stack, clears .bss, and
 * branches to the Rust entry. __bss_start/__bss_end bound the zero-fill;
 * __stack_top is the initial SP (the stack grows down from just past the image,
 * in RAM clear of both the image and QEMU's DTB placement).
 */

OUTPUT_FORMAT(elf64-littleaarch64)
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5);   /* R-X */
    rodata PT_LOAD FLAGS(4);   /* R-- */
    data   PT_LOAD FLAGS(6);   /* RW- */
}

SECTIONS
{
    . = 0x40080000;

    .text : {
        KEEP(*(.text.boot))   /* _start must be the entry — keep it first */
        *(.text .text.*)
    } :text

    . = ALIGN(4096);
    .rodata : { *(.rodata .rodata.*) } :rodata

    . = ALIGN(4096);
    .data : { *(.data .data.*) } :data

    .bss : {
        . = ALIGN(16);
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } :data

    /* Initial stack: 64 KiB just past the image, in RAM. SP starts at the top
     * and grows down. (No section content — these are address symbols only.) */
    . = ALIGN(16);
    . += 0x10000;
    __stack_top = .;

    /DISCARD/ : {
        *(.comment)
        *(.note .note.*)
        *(.eh_frame)
        *(.eh_frame_hdr)
    }
}
