#!/bin/bash
# MuKernel 코드 라인 수 세기 스크립트
# 사용법: ./count_loc.sh [프로젝트_경로]
# 인자 없으면 현재 디렉토리 기준으로 실행

TARGET_DIR="${1:-.}"

echo "========================================"
echo "  MuKernel 코드 라인 수 집계"
echo "  대상: $TARGET_DIR"
echo "========================================"
echo ""

# Rust 소스 파일만 대상 (target/ 빌드 산출물 제외)
RUST_FILES=$(find "$TARGET_DIR" -name "*.rs" -not -path "*/target/*" -not -path "*/.git/*" 2>/dev/null)

if [ -z "$RUST_FILES" ]; then
    echo "⚠️  .rs 파일을 찾지 못했습니다. 경로를 확인하세요."
    exit 1
fi

FILE_COUNT=$(echo "$RUST_FILES" | wc -l | tr -d ' ')
TOTAL_LINES=0
TOTAL_CODE=0
TOTAL_COMMENT=0
TOTAL_BLANK=0

echo "── 파일별 상세 ──────────────────────────"
printf "%-50s %8s %8s %8s\n" "파일" "전체" "코드" "주석/빈줄"

for f in $RUST_FILES; do
    lines=$(wc -l < "$f" | tr -d ' ')
    blank=$(grep -c '^[[:space:]]*$' "$f")
    comment=$(grep -cE '^[[:space:]]*(//|/\*|\*)' "$f")
    code=$((lines - blank - comment))

    TOTAL_LINES=$((TOTAL_LINES + lines))
    TOTAL_BLANK=$((TOTAL_BLANK + blank))
    TOTAL_COMMENT=$((TOTAL_COMMENT + comment))
    TOTAL_CODE=$((TOTAL_CODE + code))

    # 경로가 너무 길면 뒤에서부터 50자만 표시
    short_path=$(echo "$f" | sed 's|.*\(.\{47\}\)$|...\1|')
    printf "%-50s %8d %8d %8d\n" "$short_path" "$lines" "$code" "$((blank+comment))"
done

echo ""
echo "── 전체 합계 ────────────────────────────"
echo "  .rs 파일 개수      : $FILE_COUNT"
echo "  전체 라인 수       : $TOTAL_LINES"
echo "  순수 코드 라인     : $TOTAL_CODE"
echo "  주석 라인          : $TOTAL_COMMENT"
echo "  빈 줄              : $TOTAL_BLANK"
echo ""

# 디렉토리(모듈)별 집계도 같이
echo "── 모듈(디렉토리)별 라인 수 ─────────────"
find "$TARGET_DIR" -name "*.rs" -not -path "*/target/*" -not -path "*/.git/*" 2>/dev/null \
  | xargs -I{} dirname {} \
  | sort -u \
  | while read -r dir; do
      sub_lines=$(find "$dir" -maxdepth 1 -name "*.rs" -exec cat {} \; 2>/dev/null | wc -l | tr -d ' ')
      printf "  %-40s %8d 줄\n" "$dir" "$sub_lines"
    done

echo ""
echo "========================================"
echo "  완료"
echo "========================================"