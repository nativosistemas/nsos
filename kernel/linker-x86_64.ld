/* Script de enlace del kernel para x86_64 con el protocolo Limine.
 * El kernel vive en los 2 GiB superiores (modelo de código "kernel"). */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(kmain)

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
        /* KPTI: el código de entrada y salida va en sus propias páginas,
         * las únicas del texto del kernel que ven las tablas de usuario. */
        . = ALIGN(4096);
        __kpti_text_start = .;
        KEEP(*(.kpti_text))
        . = ALIGN(4096);
        __kpti_text_end = .;
        *(.text .text.*)
    } :text
    __text_end = .;

    . = ALIGN(CONSTANT(MAXPAGESIZE));

    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata
    __rodata_end = .;

    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __data_start = .;

    .data : {
        /* KPTI: GDT, TSS, IDT, pilas de entrada y las variables que lee el
         * código de entrada antes de cambiar de tablas. */
        . = ALIGN(4096);
        __kpti_data_start = .;
        KEEP(*(.kpti_data))
        . = ALIGN(4096);
        __kpti_data_end = .;
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
