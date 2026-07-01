#!/bin/bash

# 1. 인자값 확인 (커밋 메시지가 있는지 확인)
if [ -z "$1" ]; then
    echo "오류: 커밋 메시지를 입력해주세요."
    echo "사용법: ./git.sh \"커밋 메시지\""
    exit 1
fi

COMMIT_MSG="$1"

echo "--- Git 자동화 프로세스 시작 ---"

# 2. 모든 변경 사항 스테이징
echo "[1/3] 변경 사항 추가 중..."
git add .

# 3. 커밋 생성
echo "[2/3] 커밋 생성 중: '$COMMIT_MSG'"
git commit -m "$COMMIT_MSG"

# 4. 푸시 실행
echo "[3/3] 원격 저장소로 푸시 중..."
git push

# 5. 결과 확인
if [ $? -eq 0 ]; then
    echo "--- 성공적으로 푸시되었습니다! ---"
else
    echo "--- 푸시 실패: 오류를 확인해주세요. ---"
    exit 1
fi
