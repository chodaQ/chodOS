#!/usr/bin/env bash
# BETA 21: musl-libc 런타임(.so)을 Alpine Linux apk에서 직접 추출.
#
# Docker 없이 curl + tar만으로 musl .so 파일을 얻는다.
# Alpine apk는 gzip+tar 형식이므로 tar xzf로 추출 가능.
#
# 결과물:
#   build/lib/ld-musl-x86_64.so.1   ← musl 동적 링커 겸 libc
#   build/dyn_hello.elf              ← musl-cross로 빌드한 동적 테스트 바이너리

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJ_DIR="$(dirname "$SCRIPT_DIR")"
BUILD_DIR="$PROJ_DIR/build"
LIB_OUT="$BUILD_DIR/lib"
TMP_DIR="$BUILD_DIR/_musl_tmp"

mkdir -p "$LIB_OUT" "$TMP_DIR"

# ── Alpine musl apk 다운로드 + 추출 ──────────────────────────────────────────
#
# 주의: apk 파일명의 리비전(-rN)은 Alpine이 수시로 올린다. 예전에는
# musl-1.2.5-r0.apk를 하드코딩했는데, Alpine이 r3으로 올리면서 404가 나
# 저장소를 새로 clone한 환경에서 빌드가 통째로 실패했다(CI 도입하며 발견).
# 그래서 파일명을 고정하지 않고 디렉토리 목록에서 현재 존재하는 것을 찾는다.

ALPINE_VERSIONS="3.20 3.21 3.22"
APK_FILE="$TMP_DIR/musl.apk"

# 사용 가능한 musl apk URL을 찾는다. 실패하면 빈 문자열.
find_musl_apk_url() {
    for ver in $ALPINE_VERSIONS; do
        base="https://dl-cdn.alpinelinux.org/alpine/v${ver}/main/x86_64"
        # 디렉토리 목록에서 musl-<버전>-r<N>.apk 를 추출 (musl-dev 등은 제외)
        name=$(curl -fsSL "$base/" 2>/dev/null \
               | grep -oE 'musl-[0-9][0-9.]*-r[0-9]+\.apk' \
               | sort -u | tail -1)
        if [ -n "$name" ]; then
            echo "$base/$name"
            return 0
        fi
    done
    return 1
}

if [ ! -f "$LIB_OUT/ld-musl-x86_64.so.1" ]; then
    echo "[musl] Alpine 저장소에서 musl 패키지 탐색 중..."
    APK_URL="$(find_musl_apk_url || true)"

    if [ -z "$APK_URL" ]; then
        echo "[musl] 오류: Alpine 저장소에서 musl apk를 찾지 못했습니다."
        echo "       네트워크 연결을 확인하거나, musl 런타임을 직접 받아"
        echo "       build/lib/ld-musl-x86_64.so.1 에 배치한 뒤 다시 실행하세요."
        exit 1
    fi

    echo "[musl] Alpine apk 다운로드: $APK_URL"
    curl -fsSL -o "$APK_FILE" "$APK_URL"

    echo "[musl] apk 압축 해제..."
    # Alpine apk = gzip+tar (앞 512바이트 서명 헤더 스킵 필요한 경우가 있으나 보통 그냥 tar로 가능)
    cd "$TMP_DIR"
    tar xzf "$APK_FILE" 2>/dev/null || tar xf "$APK_FILE" 2>/dev/null || true

    # 추출된 경로 탐색
    SO_FOUND=$(find "$TMP_DIR" -name "ld-musl-x86_64.so.1" 2>/dev/null | head -1)
    if [ -z "$SO_FOUND" ]; then
        echo "[musl] 직접 추출 실패. 내부 .tar.gz 시도..."
        # apk 내부에 data.tar.gz가 있을 수 있음
        DATA_TGZ=$(find "$TMP_DIR" -name "*.tar.gz" 2>/dev/null | head -1)
        if [ -n "$DATA_TGZ" ]; then
            tar xzf "$DATA_TGZ" -C "$TMP_DIR" 2>/dev/null || true
            SO_FOUND=$(find "$TMP_DIR" -name "ld-musl-x86_64.so.1" 2>/dev/null | head -1)
        fi
    fi

    if [ -n "$SO_FOUND" ]; then
        cp "$SO_FOUND" "$LIB_OUT/ld-musl-x86_64.so.1"
        echo "[musl] ld-musl-x86_64.so.1 추출 완료 ($(wc -c < "$LIB_OUT/ld-musl-x86_64.so.1") bytes)"
    else
        echo "[musl] 경고: apk 추출 실패. 수동으로 build/lib/ld-musl-x86_64.so.1 배치 필요."
        exit 1
    fi
    cd "$PROJ_DIR"
else
    echo "[musl] ld-musl-x86_64.so.1 이미 존재 — 스킵"
fi

# ── musl-cross 툴체인으로 동적 바이너리 빌드 ────────────────────────────────

DYN_BIN="$BUILD_DIR/dyn_hello.elf"
DYN_SRC="$PROJ_DIR/user/musl-test/hello_dyn_start.c"

if [ ! -f "$DYN_BIN" ]; then
    # x86_64-linux-musl-gcc 탐색
    MUSL_GCC=""
    for candidate in \
        "$(command -v x86_64-linux-musl-gcc 2>/dev/null)" \
        "/usr/local/bin/x86_64-linux-musl-gcc" \
        "/opt/homebrew/bin/x86_64-linux-musl-gcc"; do
        if [ -x "$candidate" ]; then
            MUSL_GCC="$candidate"
            break
        fi
    done

    if [ -z "$MUSL_GCC" ]; then
        echo "[musl] musl-cross 없음. 다음 명령으로 설치:"
        echo "       brew install filosottile/musl-cross/musl-cross"
        echo ""
        echo "[musl] 또는 Docker Desktop 실행 후 scripts/fetch_musl.sh 재실행."
        echo ""
        echo "[musl] 대안: 정적 stub 바이너리 사용 (완전한 동적 링킹 테스트 불가)."
        # 정적 바이너리를 복사해서 임시 대체
        cp "$BUILD_DIR/musl_hello.elf" "$DYN_BIN" 2>/dev/null || true
        echo "[musl] build/musl_hello.elf를 dyn_hello.elf로 복사 (임시 대체)."
    else
        echo "[musl] $MUSL_GCC 발견. 동적 바이너리 빌드..."
        "$MUSL_GCC" \
            -nostartfiles \
            -Wl,--dynamic-linker,/lib/ld-musl-x86_64.so.1 \
            -o "$DYN_BIN" \
            "$DYN_SRC" \
            -lc
        echo "[musl] dyn_hello.elf 빌드 완료 ($(wc -c < "$DYN_BIN") bytes)"
    fi
else
    echo "[musl] dyn_hello.elf 이미 존재 — 스킵"
fi

ls -lh "$LIB_OUT/"
echo "[musl] fetch_musl.sh 완료."
