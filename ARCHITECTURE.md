# MuKernel — Architecture Design

> "아치 리눅스의 자유도 + macOS의 편안함"
> Rust no_std, x86_64, Limine UEFI
>
> **현재 단계: ALPHA 완료 (1~17) → BETA 진행 중**
> 부팅 → 메모리 → 스케줄러 → IPC → ext4 → TCP/IP → Linux syscall →
> ELF 실행 → Shell → GUI까지 end-to-end 동작 확인됨.

---

## 1. 핵심 철학

### Linux vs MuKernel

| 구분 | Linux | MuKernel |
|------|-------|----------|
| 관리 주체 | 사용자 (Manual) | OS (Autonomous) |
| 설정 방식 | 명령형 (nice, sysctl) | 선언형 (의도만 설정) |
| 최적화 주체 | 사용자 튜닝 | Policy Engine 자율 최적화 |
| 커널 구조 | 모놀리식 | 마이크로커널 |
| 전력 전략 | 데몬 상주 | 이벤트 드리븐 |

### 한마디로
사용자는 **"의도"만 설정하고, OS가 알아서 최적화한다.**

---

## 2. 시스템 구조

```
┌──────────────────────────────────────────────────┐
│                  User Space                       │
│  ┌─────────────┐  ┌──────────────────────────┐   │
│  │  Native App │  │  Linux Binary (ELF)      │   │
│  └──────┬──────┘  └───────────┬──────────────┘   │
│         │                     │                   │
│  ┌──────▼─────────────────────▼──────────────┐   │
│  │         Compatibility Layer (libcompat)    │   │
│  │   Linux syscall ABI → Kernel IPC Message  │   │
│  └──────────────────────┬─────────────────────┘   │
└─────────────────────────┼──────────────────────────┘
                          │ IPC (Message Passing)
┌─────────────────────────▼──────────────────────────┐
│                  Microkernel                        │
│  ┌────────────┐  ┌──────────┐  ┌───────────────┐  │
│  │  Scheduler │  │  Memory  │  │  IPC Manager  │  │
│  │ (event-drv)│  │  Manager │  │               │  │
│  └────────────┘  └──────────┘  └───────────────┘  │
└────────────────────────────────────────────────────┘
                          │
┌─────────────────────────▼──────────────────────────┐
│              Kernel Services (Userspace Drivers)    │
│  ┌──────────┐  ┌─────────┐  ┌──────────────────┐  │
│  │   VFS    │  │ Network │  │  Policy Engine   │  │
│  │ (Server) │  │ (Server)│  │  (Power/Resource)│  │
│  └──────────┘  └─────────┘  └──────────────────┘  │
└────────────────────────────────────────────────────┘
```

---

## 3. 컴포넌트 상세

### 3.1 Microkernel (`kernel/`)

Ring 0에서 실행되는 유일한 컴포넌트. 최소한의 특권 코드만 포함.

**책임:**
- 물리 메모리 관리 (page frame allocator)
- 가상 메모리 / 페이지 테이블 관리
- IPC 메시지 패싱 (capability-based)
- 스케줄러 (선점형, 이벤트 드리븐)
- 인터럽트 핸들링 → 이벤트로 변환

**비책임 (유저스페이스로 위임):**
- 파일시스템, 네트워크, 디바이스 드라이버

**핵심 자료구조:**
```rust
struct PhysFrame(u64);         // 물리 페이지 소유권
struct VirtMapping { ... }     // 가상 주소 매핑 (drop 시 자동 해제)
struct Capability<T> { ... }   // 리소스 접근 권한 (컴파일타임 검증)
struct Message { sender: Pid, payload: [u8; 512] }
```

---

### 3.2 Policy Engine (`services/policy/`)

**이 OS의 핵심.** 하드웨어/프로세스 사용 패턴을 분석해서 자율 최적화.

#### 우선순위 원칙 (수정됨)

> **I/O바운드 → High, CPU바운드 → Low**

이유:
```
I/O바운드 (Firefox, 문서앱)
→ 평소엔 sleep 상태
→ 깨어났을 때 빠르게 처리해줘야 체감 반응성이 좋음
→ High 우선순위

CPU바운드 (빌드, ffmpeg)
→ 어차피 계속 CPU 사용 가능
→ 낮은 우선순위여도 잘 돌아감
→ Low 우선순위
```

Linux CFS의 interactive task detection과 동일한 방향.

#### 스케줄링 방식

**기본: global slice + 선택 빈도**
```
TIME_SLICE는 고정 (3틱)
High   → 더 자주 선택됨
Low    → 덜 선택됨
→ 레이턴시 일정하게 유지
```

**긴급: 키보드 입력 시 per-process slice 부스트**
```
IRQ1 발생 (키보드 인터럽트)
→ 포그라운드 프로세스 즉시 High + slice 부스트
→ 입력 없는 시간이 지나면 원래대로 복귀
```

#### 동작 원리

**실시간 (매 틱):**
```
컨텍스트 스위치 발생
→ 타이머에 뺏겼는지 (forced_preempt)
  vs 자발적 양보인지 (voluntary_yield) 기록
→ PCB에 누적
```

**주기적 평가:**
```
각 프로세스 분석
→ voluntary_yield 많음 = I/O바운드 → High
→ forced_preempt 많음  = CPU바운드 → Low
→ idle 프로세스        = 계산 제외 (hlt)
```

> ⚠️ 현재 36틱(~2초) 주기는 너무 길다.
> 인터랙티브 작업이 버벅일 수 있어서 나중에 실시간 갱신으로 교체 예정.

#### 우선순위 → TIME_SLICE 매핑

| 우선순위 | 선택 빈도 | 대상 |
|---------|----------|------|
| High | 자주 | I/O바운드, 키보드 입력 포그라운드 |
| Normal | 보통 | 일반 |
| Low | 드물게 | CPU바운드 백그라운드 |
| Idle | hlt | kernel idle (계산 제외) |

#### 실제 시나리오

```
빌드 + 문서 작업 동시에:

평소:
  빌드(CPU바운드)  → Low  → 백그라운드에서 돌아감
  문서앱(I/O바운드) → High → 더 자주 선택됨

타이핑 순간:
  IRQ1 발생 → 문서앱 즉시 slice 부스트
  → 글자 화면에 즉시 나타남 ✅

타이핑 끝나면:
  문서앱 다시 High (선택 빈도 방식으로 복귀)
  → 빌드 Low로 백그라운드 계속 진행
```

사람 타이핑 속도(~10키/초)와 CPU 속도(수십억 사이클/초) 차이 덕분에
**빌드도 빠르고 타이핑도 부드러운 게 동시에 가능.**

#### 알고리즘 진화 방향

> ML은 OS 기본기(VirtIO, 네트워크, Linux Compat, Shell, GUI)가
> 끝난 다음 단계다. 자세한 순서는 §4 개발 현황 참고.

```
Phase 1 (현재, 구현 완료): 규칙 + EMA
  voluntary_yield EMA로 I/O바운드 판단
  Aging으로 starvation 방지

Phase 2 (ML 1~2 단계): 통계 → Bayesian → LightGBM
  대부분의 실사용 시나리오는 LightGBM 단계로 충분할 가능성이 큼

Phase 3 (ML 3, 필요시에만): 강화학습
  상태 공간 정의와 수렴 문제가 어려워 최후순위.
  대부분의 상용 OS도 RL까지는 가지 않음.
```

#### ML 학습 사이클

```
낮      → 사용 패턴 데이터 수집
밤      → 충전 중 온디바이스 학습 (유저 공간 Policy 데몬)
아침    → 업데이트된 모델로 부팅
시간 경과 → 점점 그 사람에게 특화된 OS
```

**온보딩 설정 (최초 1회):**
```
"당신의 사용 스타일은?"
A) 빠른 반응   → 반응속도 가중치 높임
B) 빠른 처리   → 처리량 가중치 높임
C) 배터리 절약 → 전력 가중치 높임
D) 균형        → 균등하게
```

---

### 3.5 Capability Handle Table (ALPHA 9)

Linux fd 호환성을 위한 중간 계층. Compat layer 구현을 단순화하는 핵심 구조.

```rust
// 커널 내부: 실제 리소스 + 권한
struct Capability<T> {
    object: Arc<T>,
    rights: Rights,
}

// 프로세스 내부: 가벼운 핸들 (Linux fd 역할)
struct Handle {
    id: u32,
    rights: Rights,
}

// PCB: 핸들 테이블
BTreeMap<u32, Capability>
```

```
연결 구조:
Linux fd  ↔  Handle  ↔  Capability<T>
```

이 계층이 있으면 나중에 Linux Compat Tier 1의 `read`/`write`/`open`/`close`가
Capability 시스템 위에 자연스럽게 매핑된다.

---

### 3.6 VirtIO 드라이버

초기 드라이버 전략은 **VirtIO 전용**으로 간다. QEMU 환경에서 구현이 가장 단순하고,
실제 하드웨어 드라이버(NVMe, AHCI, USB/XHCI, RTL8139, e1000)는 후순위.

```
VirtIO Block → 디스크 I/O 경로
VirtIO Net   → 네트워크 카드
```

> 마이크로커널의 진짜 약점은 IPC 오버헤드가 아니라 **드라이버 부족**이다.

---

### 3.7 네트워크 스택

```
ARP → IPv4 → UDP → TCP → DHCP → DNS
```

ext4보다 더 큰 산이 될 가능성이 있는 구간. 단계적으로 접근.

---

Linux ABI를 마이크로커널 IPC로 변환하는 번역 레이어.

**현실적인 목표:**
```
Tier 1 (MVP)     : fork, exec, read, write, open, close, exit, mmap, wait
+ epoll + socket : 여기까지만 돼도 이미 엄청난 성과
Tier 2 (이후)    : signal, ioctl, pipe
Tier 3 (장기)    : io_uring, BPF, namespaces, cgroups, seccomp
                   → Linux 커널 일부를 다시 만드는 수준, 호환 계층이 커널보다 커질 수 있음
```

---

### 3.4 VFS Server (`services/vfs/`)

유저스페이스에서 동작하는 파일시스템 서버.

- 마운트 테이블 관리
- 파일시스템 드라이버 플러그인 (ext4, fat32, tmpfs)
- `/proc`, `/sys`, `/dev` 가상 FS 제공

---

## 4. 개발 현황

> **설계 원칙: ML은 마지막이다.**
> 많은 취미 OS가 ML/RL부터 떠들다가 VFS, IPC, userspace 같은
> 기본기를 완성하지 못하는 경우가 많다. MuKernel은 기본기 완성 →
> 실사용 가능 → 그 다음 ML 순서로 간다.

### 완료된 Milestone

| Milestone | 내용 | 상태 |
|-----------|------|------|
| 1 | 부팅 + 시리얼 출력 | ✅ |
| 2 | 물리 메모리 + 커널 힙 | ✅ |
| 3 | 프로세스 + 협력형 스케줄러 + IPC | ✅ |
| 3.5 | GDT + IDT + PIC + 인터럽트 | ✅ |
| 3.6 | 4단계 페이징 + Ring3 유저스페이스 | ✅ |
| 3.7 | VFS + tmpfs | ✅ |
| ALPHA 1 | 선점형 스케줄러 | ✅ |
| ALPHA 2 | Zero-copy Capability IPC | ✅ |
| ALPHA 3 | Policy Engine 기초 (관찰 + TIME_SLICE 조정) | ✅ |
| ALPHA 4 | PCB에 voluntary_yields / forced_preempts 추가 | ✅ |
| ALPHA 5 | 우선순위 시스템 (I/O바운드→High, CPU바운드→Low) | ✅ |
| ALPHA 6 | 키보드 입력 부스트 + 선택 빈도 방식 | ✅ |
| ALPHA 7 | EMA 실시간 분류 + Aging(starvation 방지) | ✅ |
| ALPHA 8 | 물리 저장소 (ext4 디스크 이미지) | ✅ |
| ALPHA 9 | Capability Handle Table | ✅ |
| ALPHA 10 | VirtIO Block 드라이버 | ✅ |
| ALPHA 11 | VirtIO Net 드라이버 | ✅ |
| ALPHA 12 | TCP/IP 스택 (ARP/IPv4/ICMP/UDP) | ✅ |
| ALPHA 13 | Linux Compat Tier 1 (write/getpid/mmap/exit) | ✅ |
| ALPHA 14 | ELF Loader | ✅ |
| ALPHA 15/16 | Shell (mushell) + 패키지 관리자 (mukg) | ✅ |
| ALPHA 17 | GUI / Window System (framebuffer + 8x8 font) | ✅ |

### ALPHA 9~17 상세 (모두 완료)

| Milestone | 내용 | 비고 |
|-----------|------|------|
| ALPHA 9 | Capability Handle Table | PCB에 `BTreeMap<u32, Capability>` — Linux fd ↔ Handle ↔ Capability 연결 구조 |
| ALPHA 10 | VirtIO Block 드라이버 | QEMU 환경에서 구현이 단순한 디스크 드라이버 |
| ALPHA 11 | VirtIO Net 드라이버 | 네트워크 카드 드라이버 |
| ALPHA 12 | TCP/IP 스택 | ARP → IPv4 → ICMP(ping 동작 확인) → UDP |
| ALPHA 13 | Linux Compat Tier 1 | write/getpid/mmap/exit syscall 동작 확인 |
| ALPHA 14 | ELF Loader | 실제 ELF 바이너리를 ring3에서 실행 |
| ALPHA 15 | Shell (`mushell`) | 프롬프트, ASCII 로고, 명령 입력 동작 |
| ALPHA 16 | 패키지 관리자 (`mukg`) | list/install/run 기본 동작 |
| ALPHA 17 | GUI | framebuffer 1280x800, 3 windows + taskbar 렌더링 |
| ML 1 | 통계 기반 (EMA → Bayesian) | 여기서부터 ML 단계 시작 |
| ML 2 | LightGBM 기반 분류 | 대부분의 경우 이 단계로 충분할 가능성이 큼 |
| ML 3 (필요시) | 강화학습(RL) | 상태공간 정의·수렴 문제가 어려워 최후순위 |

### BETA — 실사용 다듬기 (ALPHA 1~17 완료 후)

> ALPHA가 "동작하는 골격"이었다면, BETA는 "실제로 쓸 수 있게 다듬기"가 목표.
> ML은 여기서도 마지막 — BETA 전체가 끝난 뒤 시작한다.

| Milestone | 내용 | 비고 |
|-----------|------|------|
| BETA 1 | 키보드 스캔코드 → ASCII 변환 | 지금은 `[kbd] make=0x01 (?)`만 찍힘. 실제 타이핑 가능하게 |
| BETA 2 | GUI 마우스/클릭 이벤트 처리 | 지금은 정적 렌더링(3 windows + taskbar)만 됨. 입력 반응 추가 |
| BETA 3 | Linux Compat 확장 | epoll, socket, pipe, signal — Tier 1을 넘어서는 확장 |
| BETA 4 | Capability Handle Table ↔ 실제 syscall 연결 | ALPHA 9에서 만든 구조를 open/read/write/close 경로에 실제로 연결 |
| BETA 5 | 멀티코어 (SMP) 지원 | 지금은 싱글코어 전제. 실제 데스크탑 CPU 대응 |
| BETA 6 | 메모리 안전성 정리 | `mutable static reference` 등 컴파일 경고 제거 (UB 가능성 있는 부분) |
| BETA 7 | 추가 ELF 유틸리티 포팅 | ls, cat, echo 등 실사용 가능한 기본 프로그램들 |
| BETA 8 | 패키지 관리자 고도화 | ALPHA 16 `mukg`를 실제 설치/의존성 관리로 확장 |
| BETA 2-1 | "Linux syscall table 보고 epoll, socket, pipe, signal외의 이것저것 모든 syscall의 번호와 인자 시그니처를 그대로 따르되, 내부 구현은 MuKernel의 capability/IPC 시스템으로 만들어줘. Linux 커널 소스코드는 가져오지 말고 ABI 명세만 참고해" |

### ML — BETA 완료 후 시작

| Milestone | 내용 | 비고 |
|-----------|------|------|
| ML 1 | 통계 기반 (EMA → Bayesian) | 여기서부터 ML 단계 시작 |
| ML 2 | LightGBM 기반 분류 | 대부분의 경우 이 단계로 충분할 가능성이 큼 |
| ML 3 (필요시) | 강화학습(RL) | 상태공간 정의·수렴 문제가 어려워 최후순위 |

### 드라이버 우선순위 노트

```
초기 목표: VirtIO 전용으로 간다 (QEMU 친화적, 구현 단순)
  VirtIO Block, VirtIO Net 우선

후순위: 실제 하드웨어 드라이버
  NVMe, AHCI, USB/XHCI, RTL8139, e1000
```

마이크로커널에서 가장 큰 난적은 IPC 오버헤드가 아니라 **드라이버 부족**이다.

---

## 5. 기술 스택

| 항목 | 선택 | 이유 |
|------|------|------|
| 언어 | Rust (nightly) | 메모리 안전성, zero-cost abstraction |
| 부트로더 | limine | Rust 친화적, UEFI 지원 |
| 에뮬레이터 | QEMU x86_64 | KVM 가속, 디버깅 편의 |
| 타겟 | x86_64 (1차), aarch64 (2차) | 개발 환경 접근성 |
| IPC 모델 | Capability-based Message Passing | seL4 참조 |

---

## 6. 참고 자료

- [seL4 Microkernel](https://sel4.systems/) — capability 시스템 설계
- [Redox OS](https://www.redox-os.org/) — Rust 마이크로커널 실사례
- [OSDev Wiki](https://wiki.osdev.org/) — 부트/메모리/인터럽트 레퍼런스
- [Writing an OS in Rust](https://os.phil-opp.com/) — 튜토리얼 시리즈
- [Linux syscall table](https://syscalls.mebeim.net/?table=x86/64/x86_64/latest) — ABI 구현 참조