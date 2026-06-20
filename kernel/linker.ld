/*
 * linker.ld — x86_64 커널 링커 스크립트
 *
 * 링커 스크립트는 오브젝트 파일들의 섹션을 어느 메모리 주소에 어떤 순서로
 * 배치할지 링커(lld)에게 지시함.
 *
 * 왜 커스텀 링커 스크립트가 필요한가?
 * - 기본 링커 스크립트는 일반 사용자 프로그램을 가정함 (낮은 주소, OS 지원)
 * - 커널은 "상위 절반(Higher Half)"에 배치되어야 함
 * - limine 프로토콜 요청(requests) 섹션이 ELF에 정확히 들어가야 함
 */

/* ELF 출력 형식: 64비트 x86 리틀엔디안 */
OUTPUT_FORMAT("elf64-x86-64")
OUTPUT_ARCH("i386:x86-64")

/* 커널 진입점: _start 함수 (main.rs에 정의) */
ENTRY(_start)

/*
 * 프로그램 헤더(PHDR) 정의
 * ELF 로더(limine)는 이 헤더를 보고 각 세그먼트를 어떻게 로드할지 결정함.
 *
 * 권한 플래그:
 *   PF_X = 1 << 0 = 1  (실행 가능)
 *   PF_W = 1 << 1 = 2  (쓰기 가능)
 *   PF_R = 1 << 2 = 4  (읽기 가능)
 */
PHDRS {
    /* r-x: 실행 코드 세그먼트 (읽기 + 실행, 쓰기 불가) */
    text    PT_LOAD    FLAGS((1 << 0) | (1 << 2));
    /* r--: 읽기전용 데이터 (상수, 문자열 리터럴 등) */
    rodata  PT_LOAD    FLAGS((1 << 2));
    /* rw-: 읽쓰기 데이터 (전역변수, .bss 등) */
    data    PT_LOAD    FLAGS((1 << 1) | (1 << 2));
    /* PIE를 위한 동적 링킹 정보 */
    dynamic PT_DYNAMIC FLAGS((1 << 1) | (1 << 2));
}

SECTIONS {
    /*
     * 커널 베이스 주소: 0xffffffff80000000
     *
     * x86_64의 가상 주소 공간(48비트 = 256TiB):
     *   0x0000_0000_0000_0000 ~ 0x0000_7fff_ffff_ffff  → 유저스페이스 (하위 절반)
     *   0xffff_8000_0000_0000 ~ 0xffff_ffff_ffff_ffff  → 커널 공간 (상위 절반)
     *
     * 커널을 상위 절반에 배치하는 이유:
     * - 유저 프로세스의 주소와 충돌하지 않음
     * - 페이지 테이블 전환 없이 유저 메모리와 커널 메모리를 구분 가능
     */
    . = 0xffffffff80000000;

    /* ===================== .text: 실행 코드 ===================== */
    .text : {
        *(.text .text.*)    /* 모든 오브젝트 파일의 .text 섹션을 여기에 모음 */
    } :text

    /*
     * 세그먼트 경계를 페이지(4096바이트) 크기로 정렬.
     * CPU는 4KB 단위로 메모리 보호를 설정하므로,
     * 세그먼트가 페이지 경계에 맞지 않으면 권한이 혼재될 수 있음.
     * (예: .text 끝과 .rodata 시작이 같은 페이지에 있으면
     *       코드 페이지가 쓰기 가능해지거나, 데이터 페이지가 실행 가능해짐)
     */
    . = ALIGN(CONSTANT(MAXPAGESIZE));

    /* ===================== .rodata: 읽기전용 데이터 ===================== */
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    /*
     * limine 요청(Request) 마커 섹션
     *
     * limine은 커널 ELF를 메모리에 로드한 뒤,
     * .requests_start_marker 부터 .requests_end_marker 까지 스캔하여
     * 특정 매직 넘버 패턴을 가진 구조체를 찾음.
     * 찾으면 그 구조체의 response 필드에 정보를 채워 넣음.
     *
     * KEEP(): 링커 최적화(-gc-sections)로 "사용 안 됨"으로 판단해
     *         제거되지 않도록 강제 유지.
     */
    .requests_start_marker : {
        KEEP(*(.requests_start_marker))
    } :rodata

    .requests : {
        KEEP(*(.requests))
    } :rodata

    .requests_end_marker : {
        KEEP(*(.requests_end_marker))
    } :rodata

    . = ALIGN(CONSTANT(MAXPAGESIZE));

    /* ===================== .data: 초기화된 전역변수 ===================== */
    .data : {
        *(.data .data.*)
    } :data

    /*
     * .dynamic: PIE(Position Independent Executable)에 필요한 동적 링킹 정보.
     * 런타임 주소 재배치(relocation) 정보가 여기에 있음.
     * limine이 커널을 로드할 때 이 정보를 사용해 절대 주소를 수정함.
     */
    .dynamic : {
        *(.dynamic)
    } :data :dynamic

    /* .got: Global Offset Table
     * PIE에서 전역 심볼의 주소를 런타임에 결정하기 위한 간접 참조 테이블 */
    .got : {
        *(.got .got.*)
    } :data

    /*
     * .bss: 0으로 초기화될 전역변수 공간
     *
     * .data와 달리 ELF 파일에 실제 바이트를 저장하지 않음.
     * 대신 "이 크기만큼 메모리를 확보하고 0으로 채워라"는 메타데이터만 기록.
     * → 실행파일 크기 절약
     *
     * 예: static mut BUFFER: [u8; 4096] = [0; 4096];  → .bss에 배치
     *     static mut COUNT: u32 = 42;                  → .data에 배치 (초기값 있음)
     */
    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /*
     * 불필요한 섹션 제거 (바이너리 크기 감소)
     * .eh_frame: C++ 예외처리 프레임 정보 (커널에서 예외처리 없음)
     * .note: 빌드 시스템 메타데이터
     */
    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.note .note.*)
    }
}
