/*
 * BETA 21: musl-linked 동적 바이너리 테스트 — _start 직접 구현
 *
 * -nostartfiles로 빌드해서 musl의 __libc_start_main 초기화를 우회.
 * 대신 -lc로 libc.so에 링크하므로 DT_NEEDED와 재배치 테이블이 생성됨.
 * → DynLinker의 심볼 해석 + 재배치 적용을 테스트하기 위한 최소 동적 바이너리.
 *
 * 완전한 musl main() 지원은 musl __libc_start_main 초기화가 완성된 이후 가능.
 */

/* Linux x86_64 syscall 번호 */
#define SYS_write  1
#define SYS_exit  60

static long syscall1(long nr, long a1) {
    long ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(nr), "D"(a1)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static long syscall3(long nr, long a1, long a2, long a3) {
    long ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(nr), "D"(a1), "S"(a2), "d"(a3)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static void write_str(const char *s) {
    int n = 0;
    while (s[n]) n++;
    syscall3(SYS_write, 1, (long)s, n);
}

/*
 * _start: musl-linked 동적 바이너리 진입점.
 * ld-musl이 재배치를 완료하고 DT_INIT_ARRAY를 실행한 뒤 여기로 점프.
 * 커널 내장 DynLinker(RTLD_NOW) 경로에서는 재배치 직후 이 주소로 IRETQ.
 */
void _start(void) {
    write_str("Hello from musl-dyn!\n");
    write_str("BETA 21: ELF .so 동적 링킹 성공!\n");
    syscall1(SYS_exit, 0);
    __builtin_unreachable();
}
