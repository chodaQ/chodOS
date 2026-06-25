# MuKernel — Minimalist, Efficient, and Self-Optimizing Operating System

> "아치 리눅스의 자유도 + macOS의 편안함"을 목표로 하는 Rust 기반 마이크로커널 OS

**현재 단계: ALPHA 1~17 완료 → BETA 1~15 완료 → BETA-X(동적 IPC 최적화) 진행 중**
부팅 → 메모리 → 스케줄러 → IPC → ext4 → TCP/IP → Linux syscall → ELF 실행 →
Shell → GUI → fork/exec/시그널/스레딩까지 end-to-end 동작 확인됨.
자세한 마일스톤은 `ARCHITECTURE.md` 참고.

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

> ⚠️ **자기 점검 노트:** 개발 중 한 번, Linux 호환 작업(fork/exec/시그널/동적
> 링커/pacman 등)에 깊이 들어가다가 "Policy Engine 달린 Linux 클론"이 되어가는
> 방향 이탈을 겪었다. Linux 호환은 아래 §3에서도 명시하듯 **Shim(도구)** 이지
> 본체가 아니다. 이 문서의 §4(Policy Engine)가 항상 우선이라는 걸 잊지 말 것.

---

## 3. 아키텍처

```
┌─────────────────────────────┐
│         User Space          │
│                             │
│  Policy Engine (데몬)        │  ← 자율 최적화의 핵심
│  드라이버 / 파일시스템         │  ← 마이크로커널답게 유저 공간에
│  Linux 호환 레이어 (Shim)     │  ← 기존 바이너리 실행 (도구일 뿐, 목적 아님)
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

이 OS의 핵심. 하드웨어/프로세스 사용 패턴을 분석해서 자율 최적화한다.

### 지금까지 구현된 것 (검증됨)

```
voluntary_yields / forced_preempts → PCB에 누적
EMA(지수이동평균)로 I/O바운드 vs CPU바운드 분류
  → I/O바운드 High, CPU바운드 Low (Linux CFS의 interactive task detection과 동일 방향)
Aging → starvation 방지
키보드 입력 부스트 → 포그라운드 프로세스 즉시 High + slice 부스트
```

### 부팅 시 동작 (Hardware Fingerprinting) — 아직 미구현, 목표로 유지

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
| 빌드 + 문서 작업 동시에 | 빌드는 Low, 타이핑은 즉시 부스트 → 둘 다 체감상 빠름 (구현됨) |
| 게임 실행됨 | CPU 성능 모드, 백그라운드 스로틀링 (목표) |
| 배터리 20% 이하 | 절전 프로파일 자동 전환 (목표) |
| 화면 꺼짐 | 극단적 절전 상태 (목표) |

### 다음 확장 방향 — "OS 레벨 아니면 안 되는 것"으로 좁히기

Policy Engine을 "그냥 유저 공간 데몬으로 만들면 되지 않나"라는 질문에 답하려면,
**커널만 볼 수 있는 시점에 개입하는가**가 기준이 되어야 한다.

```
✅ 컨텍스트 스위치 시점 개입 (지금 구현된 부분)
✅ 인터럽트 핸들러 레이턴시 활용 (키보드 부스트)
⬜ 페이지 폴트 시점 개입 (메모리 정책)
⬜ IPC 통신 빈도 관찰 → 동적 fast path 생성 (ARCHITECTURE.md BETA-X 참고)
```

마지막 항목(동적 IPC 최적화)은 학계 선례 조사 결과 "정적으로 미리 정한 fast
path"는 있어도(seL4, dIPC, SkyBridge) "런타임에 통신 빈도를 관찰해서 자동으로
fast path를 만드는" 사례는 발견되지 않은, 비어있는 자리다. 자세한 내용은
`ARCHITECTURE.md`의 BETA-X 섹션 참고.

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
- **부트로더:** limine (UEFI) — 원래는 GRUB을 검토했으나 Rust 생태계 친화성과
  UEFI 지원 때문에 전환
- **타깃:** x86_64
- **에뮬레이터:** QEMU x86_64 (KVM 가속)
- **참고 프로젝트:** Redox OS(메모리/IPC 구조), seL4(capability 시스템)

---

## 7. 커널 개발 순서 (현재 상태)

```
✅ 부팅 (limine UEFI + 시리얼 출력)
✅ GDT / IDT / PIC (인터럽트 처리)
✅ 페이징 + Ring3 유저스페이스
✅ 프로세스 & 스케줄러 (선점형, Policy Engine 연동)
✅ IPC (Zero-copy Capability 기반)
✅ 파일시스템 (ext4 디스크 이미지, tmpfs)
✅ VirtIO Block/Net 드라이버 + TCP/IP 스택
✅ Linux syscall 호환 (Tier 1 + fork/exec/signal/threading)
✅ ELF Loader, Shell(mushell), 패키지 관리자(mukg)
✅ GUI (framebuffer, 마우스/키보드 입력)
🔄 동적 IPC 최적화 (BETA-X) — 현재 진행 중, Policy Engine 핵심 확장
⬜ Policy Engine 본격 확장 (메모리/전력까지)
⬜ Linux/Arch 완전 호환 (BETA 16~28, 보류 중 — pacman 실행이 최종 목표지만
   BETA-X보다 우선순위 낮춤)
⬜ ML (BETA-X 이후, 규칙 기반 → EMA는 이미 완료, Bayesian → LightGBM 순서)
```

상세 마일스톤, 각 단계의 구현 디테일, BETA-X(동적 IPC 최적화)의 기술적 배경은
`ARCHITECTURE.md` 참고.

---

## 8. 개발 환경

### 필수 도구
```bash
rustup install nightly
rustup target add x86_64-unknown-none
cargo install bootimage
# QEMU로 에뮬레이션 (Apple Silicon에서도 크로스컴파일 가능)
qemu-system-x86_64 -drive format=raw,file=kernel.img
```

### 빌드 & 실행
```bash
make setup    # 환경 설정
make run      # 빌드 + QEMU 실행 (시리얼 콘솔만)
make run-gui  # 빌드 + QEMU 실행 (GUI + VirtIO Block/Net 포함)
```

---

## 9. 핵심 개념 요약

| 개념 | 설명 |
|---|---|
| `#![no_std]` | OS 없는 환경, 표준 라이브러리 사용 불가 |
| `#[no_mangle]` | 함수 이름 보존, 부트로더가 `_start`로 찾아옴 |
| `-> !` | 절대 반환하지 않는 함수 |
| GDT | CPU에 메모리 구역 등록하는 테이블 |
| IDT | 인터럽트 발생 시 핸들러 주소 등록하는 테이블 |
| 롱 모드 | 64비트 모드, 현대 OS의 기본 동작 모드 |
| 페이징 | 가상 주소 → 물리 주소 변환, 메모리 보호 |
| Capability | 컴파일타임에 검증되는 리소스 접근 권한 (seL4 참조) |
| EMA | 지수이동평균. Policy Engine이 행동 패턴(I/O바운드 vs CPU바운드)을 분류하는 핵심 지표 |
| Aging | 오래 기다린 프로세스의 우선순위를 강제로 올려 starvation 방지 |

---

*개발자: 중학교 1학년 / 목표: 대학 졸업 전 완성*