/* Script de enlace del kernel para ARM64 con el protocolo Limine.
 * El kernel vive en los 2 GiB superiores (como en x86_64). */
OUTPUT_FORMAT(elf64-littleaarch64)
ENTRY(nsos_entry)

PHDRS
{
    text   PT_LOAD;
    rodata PT_LOAD;
    data   PT_LOAD;
}

SECTIONS
{
    . = 0xffffffff80000000;
    __kernel_start = .;

    __text_start = .;
    .text : {
        *(.text .text.*)
    } :text
    __text_end = .;

    . = ALIGN(4K);

    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata
    __rodata_end = .;

    . = ALIGN(4K);
    __data_start = .;

    .data : {
        *(.data .data.*)

        /* Las requests de Limine deben quedar juntas y entre marcadores. */
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
    } :data

    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
    }
}
