# MuKernel — Architecture Design

> "아치 리눅스의 자유도 + macOS의 편안함"
> Rust no_std, x86_64, Limine UEFI
>
> **현재 단계: ALPHA 완료 (1~17) → BETA 1~15 완료 → BETA-X(동적 IPC 최적화) 완료
> → ML 1~4 + 장기 수렴 실험 완료**
> 부팅 → 메모리 → 스케줄러 → IPC → ext4 → TCP/IP → Linux syscall →
> ELF 실행 → Shell → GUI까지 end-to-end 동작 확인됨.
>
> **BETA-X 핵심 발견:** 마이크로커널 오버헤드의 진짜 정체는 "메시지 복사
> 비용"이 아니라 "전환(컨텍스트 스위치) 비용"이었다. ALPHA 2(Zero-copy IPC)가
> 이미 복사 문제를 다뤘기 때문에, 그 위에 SharedBuffer로 복사를 더 줄이는
> 시도(BETA-X 2)는 효과가 작거나 역효과(64B 초과 시 0.4~0.6×)였다. 반면
> yield_now()를 생략한 atomic 직접 통신은 32배 빨랐다(A-2 실험).
> 자세한 3차 실험과 결론은 §4 BETA-X 섹션 참고.
>
> **BETA-X-2 완료:** Event Tracer, 비대칭 권한 매핑, 최소 격리 검증, 이상탐지
> 강제회수, Switchless 직접 통신, Safety Bounds (1~6 전체 완료).
> BETA 16(mprotect)·17(Demand Paging) 선행 조건도 완료.
>
> **ML 실험 전체 완료 (실험 4~10, EXPERIMENTS.md 참고):**
>
> | 분류기 | A 지속 | B 스파이크 | C 전환 | D 성장 | 평균 |
> |--------|-------|-----------|--------|--------|------|
> | Baseline | 100% | 30% | 30% | 90% | 62% |
> | ML1 Bayesian (튜닝 전) | 100% | 30% | 30% | 70% | 57% |
> | ML1 Bayesian (튜닝 후, 실험 8) | 100% | 100% | 70% | 70% | **85%** |
> | ML2 GBDT (실험 5) | 100% | 90% | 80% | 70% | **85%** |
> | ML3 RL (10창, 실험 6) | 100% | 90% | 60% | 70% | 80% |
> | ML4 AND Ensemble (실험 9) | 100% | 100% | 80% | 70% | **87% ← 최고** |
>
> ML3 장기 수렴 실험(100창, 실험 10): 61% — RL 학습 실패가 아니라
> Bayesian 상태 누적으로 ML2 피처 공간이 변하는 문제로 판명.
>
> **다음 단계:** §4 "ML 로드맵 — 다음 단계" 참고.

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
| BETA 1 | 키보드 스캔코드 → ASCII 변환 + Shell 에코 | ✅ kbd.rs Set1 변환 테이블 완성. mushell read_line에 문자 에코 + 백스페이스 추가 |
| BETA 2 | GUI 마우스/클릭 이벤트 처리 | ✅ 창 드래그/닫기/태스크바 토글. wm.rs 동적 위치 + on_drag/on_release. mouse.rs rising/falling edge |
| BETA 3 | Linux Compat 확장 | ✅ pipe/select/epoll/socket/signal 등 구현 |
| BETA 4 | Capability Handle Table ↔ 실제 syscall 연결 | ✅ syscall/fd.rs로 open/read/write/close가 HandleTable에 실제로 등록됨 |
| BETA 5 | 멀티코어 (SMP) 지원 | ✅ |
| BETA 6 | 메모리 안전성 정리 | ✅ 빌드 경고 87→0 |
| BETA 7 | 추가 ELF 유틸리티 포팅 | ✅ muls, mupwd 등. mushell에 ls/pwd/mkdir/cat 내장 |
| BETA 8 | 패키지 관리자 고도화 | ✅ deps 필드, pkg_info/install/remove/installed syscall |
| BETA 9 | fork / exec / wait 완성 | ✅ UserProcTable, fork_current, wait4_impl, exec_replace |
| BETA 10 | 시그널 서브시스템 | ✅ signal.rs, rt_sigaction/sigprocmask/sigreturn, SIGSEGV/SIGCHLD |
| BETA 11 | 스레딩 (clone CLONE_THREAD) | ✅ CLONE_VM, futex_wait/wake, TLS(MSR_FS_BASE), MAX_PROCS 16 |
| BETA 12 | 쓰기 가능 tmpfs | ✅ inode 트리, rename/link/unlink/chmod, /proc, /tmp /run /var 마운트 |
| BETA 13 | ext4 copy-up overlay | ✅ copy_up, open_writable/readonly 통합 API |
| BETA 14 | /dev + TTY 서브시스템 | ✅ DevKind(Null/Zero/Tty 등), ioctl 완전 구현 |
| BETA 15 | 파일 백드 mmap + munmap | ✅ mmap_map/mmap_anon/munmap_pages, MAP_FIXED, TLB invlpg |

> **BETA 16 이후(mprotect, 동적 링커, glibc, pacman 등)는 "나중에" 칸으로 미룬다.**
> 삭제가 아니라 순서 변경 — §4 맨 아래 "BETA 16~28 (보류)" 참고.
> 그 자리보다 먼저 **BETA-X(동적 IPC 최적화)** 를 진행한다. 이유는 아래 BETA-X 섹션 참고.

### ML — BETA-X-2 완료 후 시작 (실험 4~10 전체 완료)

| Milestone | 내용 | 상태 |
|-----------|------|------|
| ML 1 | Bayesian 분류기 | ✅ 튜닝 완료 (GAIN_CAP=5, COLD_DECAY=3) — 57%→85% |
| ML 2 | GBDT 분류기 | ✅ 5-feature 4-tree. 단독 85% |
| ML 3 | Q-learning RL | ✅ 구현·평가 완료. 10창=80%, 장기 수렴 실험=61% |
| ML 4 | AND Ensemble (ML1∩ML2) | ✅ 87% — 현재 최고 정확도 |

#### 비교 실험 최종 결과 (실험 7·8·9)

| 분류기 | A 지속 | B 스파이크 | C 전환 | D 성장 | 평균 | 비고 |
|--------|-------|-----------|--------|--------|------|------|
| Baseline | 100% | 30% | 30% | 90% | 62% | 단순 누적 카운트 |
| ML1 Bayesian (튜닝 전) | 100% | 30% | 30% | 70% | 57% | β floor 버그 |
| ML1 Bayesian (튜닝 후) | 100% | 100% | 70% | 70% | 85% | GAIN_CAP=5, DECAY=3 |
| ML2 GBDT | 100% | 90% | 80% | 70% | 85% | cold_windows 피처 결정적 |
| ML3 RL (10창) | 100% | 90% | 60% | 70% | 80% | Q-table 미수렴 |
| **ML4 AND Ensemble** | **100%** | **100%** | **80%** | **70%** | **87%** | **현재 최고** |

**ML1 튜닝 (실험 8) — 핵심 파라미터:**
```
BAYES_GAIN_CAP  = 5  : 단일 창 α 최대 증가량 (spike δ=300 → gain=5, not 30)
BAYES_COLD_DECAY = 3 : cold 창당 α 감소량 (기존 1 → 3으로 빠른 망각)
→ B 스파이크: 30% → 100% (+70%p)
→ C 전환:    30% → 70%  (+40%p)
```

**ML4 AND Ensemble (실험 9) — 핵심 발견:**
```
ML1과 ML2의 오판 패턴이 서로 다른 시나리오에서 AND 앙상블이 두 단독 분류기보다 우월:
- B 시나리오: ML2의 FP 1개(spike)를 ML1이 차단 → 100% 달성
- FN 증가 없음 — recall 손실 없이 precision만 향상
```

**ML3 장기 수렴 실험 (실험 10) — 예상과 반대:**
```
가설: 100창 학습 → RL 수렴 → 10창 80%보다 높은 정확도
결과: 100창 평균 61% — 10창 결과(80%)보다 낮음

원인: Q-table 학습 실패가 아니라 Bayesian 상태(α, β, rate_ema) 누적
      패턴 반복 시 α가 계속 쌓여 ML2 피처 공간이 이동함
      특히 D 시나리오: "느린 성장" 패턴이 사이클 반복 후엔
      rate_ema가 이미 높아 ML2 입장에서 "이미 핫"으로 보임 → 34%

교훈: RL 수렴 실험과 장기 상태 실험은 구분해야 함.
      진정한 RL 수렴 = 매 사이클 Bayesian 상태 리셋 + Q-table만 누적.
```

> **ML 실험 최종 결론:**
> ML4 AND Ensemble (87%)이 현재 최고. 실제 Policy Engine에는
> `is_hot = ml1_hot && ml2_hot` 조합 적용이 권장됨.
> ML3 Q-learning은 "학습 부족"이 아니라 "상태 누적 취약성"이 본질적 한계.
> Shadow Mode(ML2 active + ML3 shadow → 자동 전환)는 설계만 됐고 미구현.

#### ML 1 → ML 2 → ML 3 정적 전환의 문제, 그리고 대안 (Shadow Mode)

설계만 됐고 아직 구현하지 않음. 필요해지면 재검토:

```
Shadow Mode → Gradual Rollout 패턴:

[IPC 패턴 감지]
       ↓
  Active 분류기 (ML4 Ensemble, 즉시 결정에 사용)
       ↓
  동일 입력을 Shadow 분류기(ML3)에도 계산 (결정에는 미반영, 로그만)
       ↓
  사후 검증 프록시: "HOT 판단 후 즉시 decay됐나?" → 오판으로 간주
       ↓
  최근 M창 오판률 비교 + cooldown 충족 시에만 Active 분류기 전환
```

#### ML 로드맵 — 다음 단계

ML 실험이 10개로 마무리된 현재 시점에서 세 가지 방향이 있다:

**방향 A: ML4 Ensemble 커널 반영 ✅ 완료 (실험 11)**
```
변경: is_hot = ml1_hot && ml2_hot  (AND Ensemble)
      hot_ipc_pairs()도 동일 적용
효과: WM↔GFX에서 채널 생성 시점이 123000 IPC 더 늦어짐 (보수적)
      ENCOURAGE로 ML2 thr=220 낮아져도 Bayesian<650이면 차단
레이블: [HOT/Ens] — 시리얼 로그에서 ensemble 결정 즉시 식별 가능
```

**방향 B: ML3 공정 재평가 ✅ 완료 (실험 12)**
```
결과: 78% (ML2 85%보다 낮음) — 가설 기각
핵심 발견: A 시나리오 99% 수렴 → Q-table 학습 자체는 정상 동작
           B 스파이크 74%로 악화 → 보상 함수 결함이 원인
           d≥20이면 무조건 ENCOURAGE 보상(+12) → spike에서 ENCOURAGE 학습
           → DISCOURAGE로 spike 억제가 불가능한 구조
결론: ML3는 보상 함수 재설계 없이 ML2를 대체할 수 없음
개선 방향(미구현): reward에 rate_ema 대비 spike 판별 추가
```

**방향 C: Phase D — ELF .so / 동적 링커 재개**
```
현재: "리눅스쪽은 잊고 ML에서 일하자"로 보류
ML 실험이 사실상 마무리됐으므로 Phase D 재개 검토 가능
내용: .so 파서, PLT/GOT 설정, 동적 심볼 해석, ld-mukernel.so
선행 조건: BETA 16(mprotect) ✅, BETA 17(Demand Paging) ✅
```

**방향 D: Phase E — Policy Engine 실전 검증**
```
현재: 합성 IPC 트레이스로만 검증 (bench_ml*)
필요: 실제 Shell + ext4 + GUI 워크로드에서 Policy Engine 동작 관찰
목표: ML4 Ensemble의 fast channel 생성 판단이 실제로 효과 있는지
```

---



### BETA-X — 동적 IPC 최적화 (BETA 16 이전에, 우선 진행)

> **이 트랙이 BETA 16(mprotect)~28(pacman)보다 먼저 진행된다.**
> BETA 16~28은 보류된 것이고 삭제된 게 아니다 — 순서만 바뀜.
>
> 계기: ARCHITECTURE.md가 점점 "Policy Engine 달린 Linux 클론"으로 흘러가고 있다는
> 위기 인식에서 시작. BETA 9~28 전체 중 Policy Engine 관련 마일스톤이 0개였음.
> Linux 호환은 원래 README에서 "Shim(도구)"으로 정의됐지, 본체가 아니었다.

#### 배경 — 왜 이 방향인가

```
질문: "Policy Engine, 그냥 유저 공간 앱으로 만들면 안 되나?"
답:   컨텍스트 스위치 시점/페이지 폴트 시점처럼
      "커널만 볼 수 있는 시점"에 개입해야 의미가 있다.
      지금 Policy Engine(EMA+aging+키보드 부스트)은 이미 그 기준을 통과함.

질문: "마이크로커널 성능 저하의 최대 원인은?"
답:   흔히 'IPC 자체 비용(메시지 복사)'이라 생각하지만, Liedtke의 분석에
      따르면 진짜 원인은 캐시 미스(capacity cache-miss) — 즉 "전환
      (컨텍스트 스위치) 횟수"다. 프로세스 전환마다 캐시가 비워지는데,
      마이크로커널은 같은 작업에도 전환 횟수가 모놀리식보다 훨씬 많다
      (예: 파일 읽기 1회에 모놀리식 전환 2회 vs 마이크로커널 전환 4회).
      → "전환 횟수를 줄이는 것"과 "캐시 미스를 줄이는 것"은 거의 같은 문제.

      참고: 메시지 복사 비용 자체는 ALPHA 2(Zero-copy Capability IPC)에서
      이미 어느 정도 다뤘다. BETA-X가 진짜로 풀어야 할 새로운 부분은
      "복사"가 아니라 "전환"이라는 게, 아래 측정 결과에서 뒤늦게 드러났다.

질문: "자주 통신하는 프로세스 쌍한테 전용 채널 만들어주는 사례, 이미 있나?"
답:   있다 — 단, 전부 '정적'이다.
      - seL4 fastpath: 미리 정해진 Call/ReplyWait 패턴만 최적화
      - dIPC: 미리 지정된 프로세스들을 공유 주소공간에 매핑 (L4보다 8.87배 빠름)
      - SkyBridge: 미리 지정된 쌍에게 공유 버퍼 제공
      - QNX: 메시지 "크기" 기준으로 레지스터/공유메모리 분기 (빈도 기준 아님)
      → "런타임에 통신 빈도를 관찰해서 자동으로 fast path를 만드는" 사례는
        검색 범위 내에서 발견되지 않음. 여기가 비어있는 자리.
```

#### 핵심 아이디어

> **Policy Engine이 프로세스의 CPU 행동 패턴을 관찰하던 것과 같은 방식으로,
> 프로세스 쌍의 IPC 통신 빈도 패턴도 관찰한다. 자주 통신하는 쌍을 감지하면
> 전용 공유 메모리 채널(fast path)을 런타임에 자동 생성한다.**

```
지금 (모든 IPC가 동일 경로):
Process A → 일반 메시지 큐 → Microkernel → 일반 메시지 큐 → Process B

목표 (감지된 고빈도 쌍만 전용 경로):
Process A ←→ [전용 SharedBuffer, Ring0 안 거침] ←→ Process B
나머지 쌍은 그대로 일반 IPC 큐 사용
```

가장 직접적인 적용 대상은 **유저 공간 서버끼리의 통신**이다(Ring 0 내부 함수 호출은
이미 충분히 빠르므로 대상이 아님):

```
WM ↔ 그래픽 드라이버    → 초당 60회(프레임마다) 통신. 전통적으로
                          마이크로커널이 GUI에서 약했던 지점이 바로 여기.
마우스 드라이버 ↔ WM    → 클릭/이동마다 통신
Policy Engine ↔ VFS/Net/드라이버 → 행동 신호 수집 경로
```

#### 단계 (모두 완료 ✅)

| Milestone | 내용 | 결과 |
|-----------|------|------|
| BETA-X 1 | IPC 통신 빈도 계측 | ✅ PCB `ipc_peer_pids[8]`/`ipc_peer_counts[8]` → `observe_ipc()` → Policy Engine `ipc_table`. 로그: `[policy-X] IPC 핫 쌍 top2: pid3→pid4: 120회 [HOT]` |
| BETA-X 2 | 임계값(count≥100) 기반 자동 채널 생성 | ✅ `ensure_channel()` idempotent 설계. `send()`/`recv()`가 기존 코드 수정 없이 투명하게 fast path 전환 (`fast_cap` sentinel) |
| BETA-X 3 | WM ↔ 그래픽 드라이버 적용 | ✅ 120회 일반 IPC → 36틱 후 자동으로 fast channel 전환. 실제 `fb::fill_rect` 렌더링까지 확인. 로그: `[gfx] ★ fast channel 첫 수신!` |
| BETA-X 4 | 입력 경로 직통화 | ✅ IRQ-safe 링버퍼(`AtomicU64[64]`, Mutex 없음) → `push_key()`를 IRQ1 핸들러에서 직접 호출 → count=101에서 자동 fast 전환 |
| BETA-X 5 | Core affinity | ✅ 구조 완성 (`pin_pair`, `set_preferred_cpu`, 스케줄러 tie-breaking). QEMU 1코어라 효과는 미관측 — 실 하드웨어 멀티코어 필요 |
| BETA-X 6 | 채널 회수(decay) | ✅ cold_windows 2회(≈4초) 무통신 시 자동 회수 확인. 채널+affinity+PCB 모두 정리됨 |
| BETA-X 7 | A/B 벤치마크 (rdtsc) | ✅ 측정 인프라 완성. **결과: avg 0.9x (오히려 700 cycles 느림)** — 아래 "측정 결과" 참고 |

#### 측정 결과 — 3차례 실험과 최종 결론

**1차 (BETA-X 7, 64B 고정, 1코어, yield 포함)**
```
[bench] Phase A (일반 IPC):     avg 12400 cycles
[bench] Phase B (fast channel): avg 13100 cycles  →  0.9× (fast가 더 느림)
```

**2차 (A-1, 페이로드 크기별, yield 포함) — crossover 지점 발견**

| 크기 | 일반 IPC | fast channel | 비율 |
|------|---------|---------------|------|
| 64B | 2428 cy | 1464 cy | **1.6× (fast가 빠름)** |
| 256B | 1535 cy | 2250 cy | 0.6× |
| 1024B | 1785 cy | 2678 cy | 0.6× |
| 4096B | 1500 cy | 3357 cy | 0.4× |

원인: 일반 IPC는 항상 64B만 복사(나머지는 버림, 데이터 손실). fast channel은
`overwrite_shared()`가 전체 페이로드를 spinlock 보유 중에 매번 통째로
복사함 — 4KB면 4KB를 그대로 복사. **처음 가정("큰 데이터일수록 유리")이
정반대로 뒤집힘.** fast channel의 실제 가치는 "큰 데이터 전달"이 아니라
"64B 이하에서 메시지 큐의 힙 재할당을 피하는 것" 하나였음.

**3차 (A-2, `-smp 4`, yield 자체를 제거하고 atomic 직접 ping-pong)**
```
[a2] Phase A (cross-core atomic, 스케줄러 미경유):  avg 35 cycles
[a2] Phase B (same-core yield,   스케줄러 경유):    avg 1125 cycles
→ 32× 차이
```

**최종 결론 — 진짜 원인을 찾음**

```
1차·2차에서 줄이려 한 것: "메시지 복사 비용"
  → 근데 ALPHA 2(Zero-copy Capability IPC)가 이미 이 문제를 해결한 상태였음
  → 그 위에 BETA-X로 복사를 한 번 더 줄이려 했으니 효과가 작거나 역효과난 게 당연했음

3차에서 드러난 것: "전환(컨텍스트 스위치/스케줄러 경유) 자체"
  → 이게 §1에서 인용한 Liedtke의 캐시 미스 분석과 정확히 일치하는 지점
  → "전환 횟수를 줄이는 것"이 진짜 핵심이었고, 그 효과는 32배로 명확히 측정됨
```

> **마이크로커널 오버헤드의 진짜 정체는 "복사 비용"이 아니라
> "전환 비용(스케줄러 경유, 캐시 미스)"이었다.**
> BETA-X 2(SharedBuffer fast channel)는 여전히 메시지 큐 + `yield_now()`를
> 거치는 구조라 이 핵심을 건드리지 못했다. A-2가 같은 아이디어(직통 통로)를
> "전환 생략" 방식으로 구현하자 32배 차이로 즉시 드러났다.

다음에 더 검증하려면: ① fast channel을 "yield_now() 없이 atomic 직접
polling"으로 다시 설계 — 이게 진짜 BETA-X의 다음 형태가 되어야 함,
② 그 위에서 다시 크기별 crossover를 재측정.

#### 알려진 한계 (정직하게 기록)

```
- 캐시 미스는 "줄어들" 수는 있어도 "없어지지"는 않는다 (하드웨어 구조적 한계)
- QEMU 에뮬레이션에서는 실제 하드웨어 캐시 동작이 정확히 안 보일 수 있음
  → 측정 신뢰도에 한계가 있을 수 있다는 점을 인지할 것
- 동적으로 채널이 생성/소멸되면 디버깅이 어려워짐 (한 리뷰에서 지적된
  "Observability" 문제와 직결) → 채널 생성/소멸 이벤트를 로깅하는
  경량 트레이싱이 함께 가야 함
- 학계에 선례가 없다는 것은 "비어있는 자리"이자 동시에 "아무도 안 한 이유가
  있을 수 있다"는 뜻이기도 함 — 보안(공유 메모리 접근 범위)과 메모리 관리
  복잡도가 실제로 만만치 않을 가능성을 열어둘 것
```

---

### BETA-X 완료 후 — 다음 방향: Switchless Direct Path (BETA-X-2)

BETA-X가 "동적 패턴 감지 → 자동 최적화" 기계 자체는 증명했고, 실험 끝에
진짜 원인(전환 비용, 복사 비용 아님)까지 찾아냈다(A-2: yield 없는 atomic
직접 통신이 32배 빠름). 그런데 외부 리뷰에서 정확히 이 지점이 동시에
**가장 큰 보안/관측 위험**이라는 지적을 받았다:

```
"커널의 중재 없이 공유 메모리로 통신하면 Capability 검증을 우회하는
 사각지대가 생긴다. BETA-X가 완성되면 '성능은 좋지만 왜 빠른지 아무도
 모르는 마법의 상자'가 될 가능성이 크다."
```

**그래서 32배 효과를 그대로 구현(전환 생략)하는 대신, 속도와 안전장치를
함께 설계한다.** 순서가 중요하다 — Tracer가 먼저 있어야 이상 탐지(3번)가
"평소 패턴이 뭔지" 기준을 가질 수 있다.

#### BETA-X-2 단계

| Milestone | 내용 | 상태 |
|-----------|------|------|
| BETA-X-2 1 | **Event Tracer (블랙박스)** — 채널 생성/소멸/부스트 등 모든 자율 결정을 고정 크기 ring buffer에 기록. 리뷰에서 "지금 당장 구현해야 할 가장 큰 기능"이라 지적된 부분 | ✅ ring=256, 90개 이벤트 기록 확인, 오버플로 없음. 7종 이벤트(PolicyTick/PriorityBoost/HotPairDetected/ChannelCreated/PinPair/DecayWarning/ChannelDropped) 전부 동작 |
| BETA-X-2 2 | **비대칭 권한 매핑** — 채널 생성 시 A는 `WRITE_ONLY`, B는 `READ_ONLY`로 페이지 매핑 | ✅ |
| BETA-X-2 3 | **최소 격리 매핑 검증** — 공유 페이지가 정확히 그 두 프로세스의 PML4에만 매핑되는지 확인 | ✅ |
| BETA-X-2 4 | **이상 탐지 기반 강제 회수** — BETA-X 6 decay 로직 확장 | ✅ |
| BETA-X-2 5 | **Switchless 직접 통신 구현** — A-2의 atomic 직접 polling 방식을 실제 fast channel에 반영 | ✅ |
| BETA-X-2 6 | **Safety Bounds** — Policy Engine의 자율 결정에 하드코딩된 상한/하한 (오판 thrashing 차단) | ✅ Priority cooldown, TIME_SLICE 동적 조정, 창당 채널 생성 한도 구현 |

> **BETA-X-2 전체 완료.** 다음 단계는 §4 "BETA-X-2 완료 후" 섹션 참고.

#### 선행 작업 — BETA 16/17을 보류 트랙에서 끌어옴

BETA-X-2 2(비대칭 권한 매핑)를 구현하려면 페이지 단위로 `WRITE_ONLY`/
`READ_ONLY`를 거는 기능이 먼저 있어야 한다. 이건 원래 "BETA 16~28 보류"
트랙(Linux/Arch 호환용으로 미뤄둔 부분)에 있던 항목인데, BETA-X-2의
전제 조건과 정확히 겹쳐서 두 트랙 모두에 의미 있게 먼저 완료했다.

| Milestone | 내용 | 상태 |
|-----------|------|------|
| BETA 16 | mprotect 완전 구현 | ✅ `PTE_WRITABLE` 비트 제어로 페이지 단위 읽기/쓰기 권한 변경 |
| BETA 17 | Demand Paging | ✅ `MAP_ANONYMOUS` lazy 할당 + `#PF`(not-present) 핸들러 |

> 이 둘은 "BETA 16~28 보류" 트랙에서 미리 가져온 것이고, 나머지(Phase D~F,
> 동적 링커~pacman)는 여전히 보류 상태다. §4 맨 아래 "BETA 16~28" 표 갱신.

#### 설계 원칙 — "생성은 엄격하게, 사용은 가볍게"

```
채널 생성 시점 (드물게, 36틱마다 한 번)
  → Capability 검증 + 비대칭 권한 매핑 + 격리 확인 (전부 엄격)

채널 사용 시점 (자주, 매 프레임/매 클릭)
  → atomic 직접 읽기/쓰기, 매번 검증 없음 (이게 32배 빨라지는 핵심)

채널 감시 (지속적, Policy Engine 백그라운드)
  → 이상 패턴 감지 시 즉시 강제 회수 (사용 시점에 생긴 검증 공백을 사후에 메움)
```

공항 검색대(생성, 깐깐함) → 탑승 후 좌석 이동(사용, 검사 없음) → 이상 행동
감지 시 즉시 제지(감시) 비유와 같다. 완벽한 보안과 완벽한 속도를 동시에
가질 수는 없다는 걸 인정하고, 그 사이 절충점을 명시적으로 설계한 것.

#### 외부 리뷰 대응 매핑

| 리뷰가 지적한 위험 | 위험도 | 대응 마일스톤 |
|---|---|---|
| 보안 경계 우회 (Capability 미검증 직통 채널) | 상 | BETA-X-2 2, 3 |
| 디버깅 난이도 (Observability) | 상 | BETA-X-2 1 |
| 드라이버 부족 | 최상 | 별도 트랙 (§4 "드라이버 우선순위 노트" 참고, BETA-X-2 범위 밖) |
| 정책 오판/Thrashing | 중 | BETA-X-2 6 |

드라이버 부족 문제는 리뷰에서 "위험도 최상"으로 지적됐지만 BETA-X-2(IPC
안전장치) 범위와는 결이 달라서 별도로 다룬다 — 지금은 VirtIO 전용 전략을
유지하고, 실하드웨어 드라이버 포팅은 BETA 16~28(보류 트랙)과 비슷하게
나중으로 미룬다.

---

### BETA 16~28 (대부분 보류 — Linux/Arch 완전 호환, 나중에)

> BETA 16/17은 BETA-X-2 2의 선행 조건이라 먼저 완료. 나머지는 BETA-X-2
> 완료 후 재개. 진행 시 Phase A~F 순서를 따른다.

| Phase | 내용 | 상태 |
|-------|------|------|
| Phase C (BETA 16~17) | mprotect 완전 구현, Demand paging & 페이지 폴트 | ✅ 완료 (BETA-X-2 2 선행 조건으로 먼저 끌어옴) |
| Phase D (BETA 19~21) | ELF .so 파서 & 재배치, 동적 링커(ld-musl 호환), musl-libc 내장 | ⬜ 보류 |
| Phase E (BETA 22~24) | TCP/IP 실제 구현(smoltcp), DNS 리졸버, TLS/HTTPS | ⬜ 보류 |
| Phase F (BETA 25~28) | 아카이브/압축(zstd), pacman-static 실행, glibc 호환성, 완전한 Arch 환경 | ⬜ 보류 |

> **완료 기준:** Phase D → musl-linked 동적 바이너리 실행 / Phase E → `curl https://archlinux.org` 성공
> / Phase F → `pacman -S neovim` 설치 후 실행 ✓

---

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

### 외부 의존성

```
ext4 파일시스템 구현: rust-fs-ext4 (외부 crate)
  → ALPHA 8(물리 저장소)에 사용. 저널링, htree 디렉토리, 체크섬,
    xattr까지 포함된 완성도 높은 외부 라이브러리를 가져다 씀.
  → 직접 작성한 게 아니라 통합(integration)한 것 — 정직하게 구분할 것.
```

### 코드 규모 (직접 작성분만, 2026-06-29 기준)

```
kernel/src/  : 16,704줄
user/        :  1,101줄
─────────────────────
합계         : 17,805줄  (rust-fs-ext4 외부 crate ~43,599줄 제외)
```

모듈별 분포: syscall 3,087줄(18%, Linux ABI 호환 디테일이 가장 큼) >
process 2,524줄(14%) > policy 1,215줄(7%, Policy Engine 자체는 토론
시간 대비 코드량은 작음 — 설계 판단이 코드량보다 무거운 작업이었다는 방증).
`count_loc.sh`로 재현 가능 (저장소 루트에서 실행).

---

## 6. 참고 자료

- [seL4 Microkernel](https://sel4.systems/) — capability 시스템 설계
- [Redox OS](https://www.redox-os.org/) — Rust 마이크로커널 실사례
- [OSDev Wiki](https://wiki.osdev.org/) — 부트/메모리/인터럽트 레퍼런스
- [Writing an OS in Rust](https://os.phil-opp.com/) — 튜토리얼 시리즈
- [Linux syscall table](https://syscalls.mebeim.net/?table=x86/64/x86_64/latest) — ABI 구현 참조