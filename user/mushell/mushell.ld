/* mushell.ld — MuShell 유저 프로세스 링커 스크립트
 *
 * 0x400000에 단일 PT_LOAD 세그먼트 생성.
 * 커널의 ELF 로더(paging::enter_elf)가 이 주소에 바이너리를 매핑.
 */
OUTPUT_FORMAT("elf64-x86-64")
ENTRY(_start)

PHDRS {
    /* R|W|X = 7 : 단일 세그먼트로 모든 섹션 포함 */
    load PT_LOAD FLAGS(7);
}

SECTIONS {
    . = 0x400000;

    /* _start 심볼을 포함하는 .text.entry를 맨 앞에 배치 */
    .text : {
        KEEP(*(.text.entry))
        *(.text .text.*)
    } :load

    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
    } :load

    .data : ALIGN(16) {
        *(.data .data.*)
    } :load

    /* BSS: 파일에 저장되지 않음 — ELF 로더가 페이지 단위로 0 초기화 */
    .bss : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.* COMMON)
        . = ALIGN(8);
        __bss_end = .;
    } :load

    /* 디버그/메타 섹션 제거 → 바이너리 크기 최소화 */
    /DISCARD/ : {
        *(.eh_frame*)
        *(.note*)
        *(.comment)
        *(.debug*)
    }
}
