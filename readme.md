# Minimalist, Efficient, and Self-Optimizing Operating System

> "아치 리눅스의 자유도 + macOS의 편안함"을 목표로 하는 Rust 기반 마이크로커널 OS

---

## 1. 프로젝트 비전

리눅스는 강력하지만 사용자가 직접 설정 파일을 만지고, 패키지를 설치하고, 튜닝해야 한다.
macOS는 편하지만 자유도가 없다.

이 OS는 **그 둘의 장점만 가져온다.**

- 아무것도 안 해도 최적화가 되어 있는 상태
- 원하면 깊이 파고들 수 있는 자유도

---

## 2. 핵심 철학

### Declarative (선언적) 관리
리눅스처럼 "이렇게 하라"고 명령하는 게 아니라, 사용자는 **"목표"만 설정**한다.
OS가 알아서 그 목표에 맞는 최적의 파라미터를 도출한다.

```
Linux  → sysctl, systemd, udev 직접 설정
이 OS  → "배터리 절약 모드" 선언만 하면 끝
```

### Zero-Management
사용자가 관리자가 아닌 **"의도를 설정하는 사람"** 이 된다.

### Event-Driven
백그라운드 데몬 없음. 하드웨어 이벤트가 발생할 때만 CPU가 반응한다.

---

## 3. 아키텍처

```
┌─────────────────────────────┐
│         User Space          │
│                             │
│  Policy Engine (데몬)        │  ← 자율 최적화의 핵심
│  드라이버 / 파일시스템         │  ← 마이크로커널답게 유저 공간에
│  Linux 호환 레이어 (Shim)     │  ← 기존 바이너리 실행
│                             │
├─────────────────────────────┤
│         Kernel Space        │
│                             │
│  Microkernel                │  ← IPC, 메모리, 스케줄러만
│                             │
└─────────────────────────────┘
```

---

## 4. Policy Engine

이 OS의 핵심. 부팅 시 하드웨어를 스캔하고, 실시간으로 자원을 최적화한다.

### 부팅 시 동작 (Hardware Fingerprinting)

```
1. Hardware Discovery
   ACPI 테이블 + PCI 버스 스캔
   → HardwareTopology 구조체 생성
   (P-Core / E-Core 개수, PMIC 주소 등)

2. Profile Matching
   하드웨어 특성에 맞는 Policy Object 로드
   → 울트라북이면 절전 프로파일
   → 데스크탑이면 성능 프로파일

3. Self-Patching
   불필요한 드라이버 로드 안 함
   캐시라인에 맞게 데이터 구조 재정렬
```

### 실시간 최적화 예시

| 상황 | Policy Engine의 판단 |
|---|---|
| 게임 실행됨 | CPU 성능 모드, 백그라운드 스로틀링 |
| 배터리 20% 이하 | 절전 프로파일 자동 전환 |
| 컴파일 중 + 브라우저 사용 | 포그라운드 앱 우선, 컴파일은 E-Core로 |
| 화면 꺼짐 | 극단적 절전 상태 |

---

## 5. Linux vs 이 OS

| 구분 | Linux | 이 OS |
|---|---|---|
| 관리의 주체 | 사용자 (Manual) | OS (Autonomous) |
| 핵심 구조 | 모놀리식 | 마이크로커널 |
| 전력/자원 전략 | 데몬 상주 | 이벤트 기반 |
| 설정 방식 | 명령형 (Imperative) | 선언형 (Declarative) |
| 최적화 주체 | 사용자 튜닝 | OS 자율 최적화 |

---

## 6. 기술 스택

- **언어:** Rust (Nightly)
- **부트로더:** GRUB (Multiboot2)
- **타깃:** x86_64
- **참고 프로젝트:** Redox OS (메모리 관리, IPC 구조)

---

## 7. 커널 개발 순서 (학습 로드맵)

```
현재 단계
├── ✅ GRUB 부팅 + VGA 텍스트 출력
├── 🔄 GDT (Global Descriptor Table)
│       메모리 구역 정의, 보호 모드/롱 모드 진입
├── ⬜ IDT (Interrupt Descriptor Table)
│       키보드, 타이머 인터럽트 처리
│       → Policy Engine 이벤트 드리븐의 기초
├── ⬜ 페이징 (Paging)
│       가상 주소 → 물리 주소 변환
│       프로세스 간 메모리 격리
├── ⬜ 프로세스 & 스케줄러
│       Policy-aware 스케줄러 (CFS/EEVDF 참고)
├── ⬜ IPC (프로세스 간 통신)
│       마이크로커널의 핵심
├── ⬜ 파일시스템 (ext2부터)
└── ⬜ Policy Engine 본격 구현
```

---

## 8. 개발 환경

### 필수 도구
```bash
rustup install nightly
rustup target add x86_64-unknown-none
cargo install bootimage
# QEMU로 에뮬레이션
qemu-system-x86_64 -drive format=raw,file=kernel.img
```

### 빌드 & 실행
```bash
make setup   # 환경 설정
make run     # 빌드 + QEMU 실행
```

---

## 9. 핵심 개념 요약

| 개념 | 설명 |
|---|---|
| `#![no_std]` | OS 없는 환경, 표준 라이브러리 사용 불가 |
| `#[no_mangle]` | 함수 이름 보존, GRUB이 `_start`로 찾아옴 |
| `-> !` | 절대 반환하지 않는 함수 |
| `0xb8000` | VGA 텍스트 버퍼 주소 |
| GDT | CPU에 메모리 구역 등록하는 테이블 |
| IDT | 인터럽트 발생 시 핸들러 주소 등록하는 테이블 |
| 롱 모드 | 64비트 모드, 현대 OS의 기본 동작 모드 |
| 페이징 | 가상 주소 → 물리 주소 변환, 메모리 보호 |

---

*개발자: 중학교 1학년 / 목표: 대학 졸업 전 완성*