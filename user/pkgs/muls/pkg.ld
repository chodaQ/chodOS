OUTPUT_FORMAT("elf64-x86-64")
ENTRY(_start)
PHDRS { load PT_LOAD FLAGS(7); }
SECTIONS {
    . = 0x400000;
    .text   : { KEEP(*(.text.entry)) *(.text .text.*) } :load
    .rodata : ALIGN(16) { *(.rodata .rodata.*) } :load
    .data   : ALIGN(16) { *(.data .data.*) } :load
    .bss    : ALIGN(16) { __bss_start = .; *(.bss .bss.* COMMON) __bss_end = .; } :load
    /DISCARD/ : { *(.eh_frame*) *(.note*) *(.comment) *(.debug*) }
}
