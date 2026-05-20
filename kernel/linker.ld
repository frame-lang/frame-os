/*
 * Frame OS kernel linker script (x86_64, Limine boot protocol v3).
 *
 * Standard higher-half layout: the kernel lives at -2GB in virtual address
 * space (0xffffffff80000000). Limine relocates the kernel ELF to this
 * address when it does the handoff.
 *
 * The .requests* sections are critical: Limine scans for protocol-info
 * markers there. Without the KEEP() the linker would garbage-collect them
 * because nothing in the rest of the kernel directly references them.
 */

OUTPUT_FORMAT(elf64-x86-64)
ENTRY(kmain)

PHDRS
{
    text    PT_LOAD;
    rodata  PT_LOAD;
    data    PT_LOAD;
}

SECTIONS
{
    /* Higher-half kernel base address (canonical -2GB on x86_64). */
    . = 0xffffffff80000000;

    .text : {
        *(.text .text.*)
    } :text

    . = ALIGN(CONSTANT(MAXPAGESIZE));

    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    /*
     * Limine boot-protocol markers. Order matters:
     *   - requests_start_marker first
     *   - all .requests next (the BASE_REVISION, etc.)
     *   - requests_end_marker last
     */
    .requests_start : {
        KEEP(*(.requests_start_marker))
    } :rodata
    .requests : {
        KEEP(*(.requests))
    } :rodata
    .requests_end : {
        KEEP(*(.requests_end_marker))
    } :rodata

    . = ALIGN(CONSTANT(MAXPAGESIZE));

    .data : {
        *(.data .data.*)
    } :data

    .bss : {
        *(COMMON)
        *(.bss .bss.*)
    } :data

    /*
     * Discard sections that confuse Limine or aren't needed in a kernel:
     *   - .eh_frame: unwinding tables (we use panic=abort)
     *   - .note.*: ELF notes (gnu-build-id, etc.)
     */
    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
    }
}
