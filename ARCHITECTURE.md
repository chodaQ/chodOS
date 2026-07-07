# MuKernel — Architecture Design

> "아치 리눅스의 자유도 + macOS의 편안함"
> Rust no_std, x86_64, Limine UEFI
>
> **현재 단계: ALPHA 완료 (1~17) → BETA 1~17 완료 → BETA-X 완료
> → BETA-X-2 완료 → ML 1~4 완료 → Phase D 완료 → PE-1~5 완료
> → Self-Tuning ST-1 완료 (진동 이슈 발견 → Hysteresis+EMA+표본유보로 안정화 완료)**
>
> **BETA-X 핵심 발견:** 마이크로커널 오버헤드의 진짜 정체는 "메시지 복사
> 비용"이 아니라 "전환(컨텍스트 스위치) 비용"이었다. yield_now()를 생략한
> atomic 직접 통신은 32배 빨랐다(A-2 실험).
>
> **ML 실험 전체 완료 (실험 4~20, EXPERIMENTS.md 참고):**
>
> | 분류기 | A 지속 | B 스파이크 | C 전환 | D 성장 | 평균 |
> |--------|-------|-----------|--------|--------|------|
> | Baseline | 100% | 30% | 30% | 90% | 62% |
> | ML1 Bayesian (튜닝 전) | 100% | 30% | 30% | 70% | 57% |
> | ML1 Bayesian (튜닝 후, GAIN_CAP=5, COLD_DECAY=3) | 100% | 100% | 70% | 70% | 85% |
> | ML2 GBDT (5-feature 4-tree) | 100% | 90% | 80% | 70% | 85% |
> | ML3 RL (10창) | 100% | 90% | 60% | 70% | 80% |
> | **ML4 AND Ensemble (ML1∩ML2)** | **100%** | **100%** | **80%** | **70%** | **87% ← 최고** |
>
> **PE-1~5 완료 및 핵심 발견:**
> PE-4: Policy Engine ON이 OFF 대비 키입력 레이턴시 4.5배 개선.
> 단, CPU바운드 처리량을 9배 희생하는 트레이드오프 확인.
> PE-5: Linux CFS는 처리량 희생 없이 비슷한 반응성 달성 —
> CFS의 vruntime 연속 배분이 MuKernel의 이진 High/Low 분류보다 효율적.
> 원인: 하드코딩된 파라미터(TIME_SLICE 1~4틱, EMA α=0.3, 평가 주기 36틱)가
> 최적이 아니었음. **다음 방향: 파라미터 자체를 런타임에 자동 튜닝.**
>
> **Self-Tuning Policy Engine 진행 중 — ST-1 안정화 완료 (실험 21→22):**
> 평가 주기(report_interval)를 컨텍스트 스위치 빈도 기반으로 8/18/36/72틱
> 4단계 사다리에서 자동 조정한다. 실험 21에서 8→72→36→8틱으로 진동하는
> 문제를 발견했으나(원인: 스위치율을 나누는 창 길이 자체가 지난 조정 결과라
> 댐핑 없는 되먹임 루프가 됨), 실험 22에서 3중 안전장치 —
> **① Hysteresis(창당 ±1단계만 이동) ② EMA 평활화 ③ 표본 부족(창내 스위치
> < 5) 시 판단 유보** — 를 적용해 36→18→8로 순차 수렴 후 안정 유지되는 것을
> 확인했다. **다음 방향: 이 3중 안전장치 패턴을 ST-2(EMA α 동적화)에도 템플릿으로
> 재사용.**

---
## Origin

MuKernel은 거창한 계획에서 시작된 프로젝트가 아닙니다.

어느 날 **'Desktop Without You'**라는 하츠네 미쿠 보컬로이드 곡을 듣고 있었습니다. 오랜만에 들은 노래라 "아, 이 노래였지." 하고 듣다가 일러스트를 봤고, 갑자기 **"나도 나만의 데스크톱 운영체제를 만들어 보고 싶다."**는 생각이 들었습니다.

그렇게 별생각 없이 시작한 프로젝트가 어느새 마이크로커널, Linux syscall 호환, ELF 로더, GUI, Policy Engine, 동적 링커까지 구현하는 프로젝트가 되었습니다.

지금 생각해도 시작 계기는 꽤 황당하지만, 덕분에 MuKernel이 탄생했습니다.


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

#### 현재 관찰하는 신호 (모두 완료)

```
✅ CPU 행동: voluntary_yield / forced_preempt → EMA → 우선순위
✅ IPC 빈도: 프로세스 쌍별 통신 카운터 → 동적 fast path 생성
✅ 키보드 인터럽트: IRQ1 → 포그라운드 즉시 High + slice 부스트
✅ 메모리 압력: 페이지 폴트 빈도 → 우선순위 보정 (PE-1)
✅ 전력 상태: 유휴율 기반 C-state 권고 (PE-2)
✅ 코어 특성: CPUID P-core/E-core 구분 → affinity 권고 (PE-3)
```

#### PE-4/PE-5 발견 — 하드코딩의 한계

```
PE-4 결과:
  키입력 레이턴시: PE=ON 103M cycles vs PE=OFF 470M cycles (4.5배 개선)
  CPU바운드 완료: PE=ON 37tick vs PE=OFF 4tick (9배 희생)
  → Policy Engine은 설계대로 동작하지만 처리량 희생이 너무 큼

PE-5 결과 (Linux CFS 비교):
  CFS: 처리량 희생 없이 비슷한 반응성 달성
  원인: CFS의 vruntime 연속 배분 vs MuKernel 이진 High/Low 분류
  → MuKernel의 하드코딩된 파라미터가 최적이 아니었음

하드코딩된 문제 파라미터:
  TIME_SLICE: 1~4틱 (High/Low 차이 너무 극단적)
  EMA α:     0.3 (고정, 워크로드 변화에 둔감)
  평가 주기: 36틱 (고정, 너무 느림)
  GAIN_CAP:  5 (고정)
  ...전부 "실험해보니 이 정도면 되더라"로 정한 값
```

#### 알고리즘 진화 (완료)

```
Phase 1: 규칙 + EMA (완료)
Phase 2: Bayesian(ML1) + GBDT(ML2) (완료)
Phase 3: RL(ML3) + AND Ensemble(ML4) (완료, ML4가 최고 87%)
```

---

### 3.8 Linux 호환 레이어

```
Tier 1 ✅: fork, exec, read, write, open, close, exit, mmap, wait
Tier 2 ✅: signal, pipe, epoll, socket, clone, futex
Tier 3 ⬜: io_uring, BPF, namespaces, cgroups, seccomp
동적 링킹 ✅: ELF .so 파서, 동적 링커, musl-libc (Phase D 완료)
```

---

## 4. 개발 현황

### 완료된 Milestone

| Milestone | 내용 | 상태 |
|-----------|------|------|
| 1~3.7 | 부팅·메모리·협력형 스케줄러·GDT/IDT/PIC·페이징·VFS | ✅ |
| ALPHA 1~7 | 선점형 스케줄러·Zero-copy IPC·Policy Engine·EMA+Aging | ✅ |
| ALPHA 8~17 | ext4·VirtIO·TCP/IP·Linux Compat·ELF·Shell·패키지·GUI | ✅ |
| BETA 1~17 | Linux Compat 확장·SMP·프로세스 모델·파일시스템·mprotect·Demand Paging | ✅ |
| BETA-X 1~7 | 동적 IPC fast path (관찰→생성→회수) | ✅ |
| BETA-X-2 1~6 | Event Tracer·보안 안전장치·Switchless | ✅ |
| ML 1~4 | Bayesian·GBDT·RL·AND Ensemble (87%) | ✅ |
| Phase D (BETA 19~21) | ELF .so·동적 링커·musl-libc | ✅ |
| PE-1~5 | 메모리·전력·코어·A/B 비교·Linux CFS 비교 | ✅ |

---

### PE-1~5 — Policy Engine 확장 (완료)

| Milestone | 내용 | 결과 |
|-----------|------|------|
| PE-1 | 메모리 압력 감지 | ✅ #PF 카운터 → 우선순위 보정 |
| PE-2 | 전력 상태 추정 | ✅ 유휴율 기반 C-state 권고 (C0/C2) |
| PE-3 (실험 18) | 코어 특성 감지 | ✅ CPUID 0x1A P/E-core 구분. QEMU 미지원이라 구조만 검증 |
| PE-4 (실험 19) | A/B 비교 (PE on vs off) | ✅ 레이턴시 4.5배 개선, 처리량 9배 희생 트레이드오프 확인 |
| PE-5 (실험 20) | Linux CFS 비교 | ✅ CFS가 처리량 희생 없이 비슷한 반응성 달성. 하드코딩 한계 발견 |

**PE-5 핵심 발견 — 예상이 뒤집힘 (이 프로젝트의 반복 패턴):**
```
기대: MuKernel Policy Engine > Linux CFS
실제: CFS가 더 적은 처리량 희생으로 비슷한 반응성 달성

원인:
  CFS: vruntime 연속 비례 배분 → 잠든 스레드 깨어날 때 자동 우선권
  MuKernel: 이진(High/Low) + 하드코딩 TIME_SLICE → 너무 거칠게 동작

한계 명시:
  비교 환경이 공정하지 않음 (MuKernel=QEMU TCG x86_64 vs Linux=Docker aarch64)
  → 같은 아키텍처 bare metal에서 재측정 시 더 엄밀한 결과 가능

결론:
  "MuKernel이 이겼다"가 아니라
  "왜 CFS가 더 효율적인지 발견하고 다음 개선 방향을 도출했다"
  → 이게 이 프로젝트의 연구 방법론
```

---

### Self-Tuning Policy Engine — 다음 트랙

> **핵심 아이디어:**
> 기존 OS들은 파라미터를 고정하고 사용자가 sysctl로 수동 튜닝한다.
> MuKernel은 파라미터 자체를 런타임에 관찰 기반으로 자동 조정한다.
> **"다른 OS는 파라미터를 튜닝해서 최적화, MuKernel은 파라미터 튜닝 자체를 자동화"**
>
> 이것이 README 원래 비전("사용자는 의도만 설정")의 가장 깊은 구현이다.

#### 왜 가능하냐면

```
안전장치가 이미 다 있어요:
1. Safety Bounds (BETA-X-2 6) → 파라미터가 극단으로 가면 차단
2. Event Tracer (BETA-X-2 1) → 파라미터 변경 이력 추적 가능
3. ML4 Ensemble → 분류 자체는 이미 87%로 안정적

디버깅 문제 해결:
→ 모든 파라미터 변경을 Event Tracer에 기록
→ "왜 갑자기 이렇게 동작하지?" 추적 가능
```

#### 구현 계획

| Milestone | 내용 | 상태 |
|-----------|------|------|
| ST-1 | **평가 주기 동적화** — 컨텍스트 스위치 빈도 기반으로 평가 주기 자동 조정. 부하 높을 때 8틱, 낮을 때 72틱. 고정 36틱보다 반응성·오버헤드 균형 개선 | ✅ 구현·부팅 검증 완료 (실험 21). ⚠→✅ 8→72→36→8틱 진동 발견 후 Hysteresis+EMA+표본유보로 안정화 완료 (실험 22) |
| ST-2 | **EMA α 동적화** — 프로세스 행동이 안정적이면 α 낮춤(0.1), 갑자기 바뀌면 α 높임(0.5). "빌드하다 타이핑으로 전환" 같은 패턴에 빠르게 적응 | ⬜ |
| ST-3 | **TIME_SLICE 범위 동적화** — 현재 레이턴시/처리량 트레이드오프를 관찰해서 범위 자동 조정. 게임 모드(반응성 우선)→1~4틱, 빌드 모드(처리량 우선)→2~3틱 | ⬜ |
| ST-4 | **WorkloadProfile 자동 감지** — 사용자가 명시적으로 선언 안 해도 현재 워크로드 패턴(게임/빌드/균형)을 자동 감지해서 ST-3 범위 선택. README 원래 비전의 "온보딩 설정" 자동화 | ⬜ |
| ST-5 | **A/B 비교 재측정** — Self-Tuning 적용 후 PE-4와 동일 워크로드로 재측정. 처리량 희생이 줄어들면서 반응성은 유지되는지 확인. 특히 Linux CFS와의 트레이드오프 간격이 좁혀지는지 검증 | ⬜ |

#### ST-1 진동(oscillation) 이슈 — 예상 밖 발견 및 안정화 (실험 21→22)

```
증상 (실험 21): report_interval이 36→8→72→36→8틱으로 연속 두 창도 같은
                값을 유지 못함

원인: switch_rate_x10 = (창 내 스위치 수 × 10) / window
      → window(창 길이) 자체가 "지난 번 조정 결과"라서 되먹임 루프가
        자기참조적 구조가 됨. 짧은 창(8틱) 뒤에는 표본이 적어 노이즈가
        커지고, 그 노이즈가 다시 극단적 조정을 유발.

교훈: PE-5에서 배운 "하드코딩보다 완만한 조정이 낫다"는 원칙이
      Self-Tuning 로직 자체에도 적용됨 — 파라미터를 자동 튜닝하는
      메커니즘도 급격한 이진 전환이 아니라 댐핑이 필요하다.

해결 (실험 22, 3중 안전장치 — kernel/src/policy/mod.rs 반영 완료):
  1. Hysteresis — report_interval을 연속값이 아닌 8/18/36/72틱 4단계
     사다리로 만들고, 창당 ±1단계만 이동 (8↔72 직행 금지)
  2. EMA 평활화 — switch_rate_x10을 raw로 쓰지 않고 다른 신호(vol_ema
     등)와 동일한 α=0.3 EMA로 평활
  3. 표본 부족 시 유보 — 창내 스위치 수 < 5 (SWITCH_SAMPLE_MIN)이면
     EMA 갱신도, 레벨 이동도 하지 않고 이전 값 유지

검증 결과: 동일 40초 관찰 구간에서 리포트 창 57개 중 파라미터 변경은
단 2회(36→18→8)뿐이었고, 이후 나머지 55개 창 동안 안정 유지 —
극단적으로 높은 스위치율(raw 최대 7143/틱)에서도 한 번에 1단계만
이동함을 확인. 표본 부족 유보 경로는 이번 워크로드에서는 트리거되지
않아(항상 충분히 활동적) 유휴 구간 검증은 후속 과제로 남음.
```

#### 설계 원칙

```
"관찰 → 판단 → 조정 → 검증" 사이클
  관찰: 레이턴시, 컨텍스트 스위치 수, 처리량
  판단: 지금 파라미터가 최적인가?
  조정: Safety Bounds 안에서 파라미터 변경
  검증: 다음 평가 주기에 결과 확인

모든 조정은 Event Tracer에 기록:
  "TIME_SLICE 상한을 4→3틱으로 조정 (이유: 처리량 손실 과다)"
  → 디버깅 가능, 논문 재현 가능
```

---

### BETA 22~28 보류 트랙 (언제든 재개 가능)

| Phase | 내용 | 상태 |
|-------|------|------|
| Phase E (BETA 22~24) | smoltcp, DNS, TLS/HTTPS | ⬜ 보류 |
| Phase F (BETA 25~28) | zstd, pacman-static, glibc, 완전한 Arch | ⬜ 보류 |

> "언제든지 할 수 있는 것"이므로 Self-Tuning Policy Engine보다 후순위.

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
rust-fs-ext4 (외부 crate): ext4 파일시스템 구현 전체
→ ALPHA 8에서 통합. 직접 작성이 아닌 통합(integration).
→ 정직하게 구분할 것.
```

### 코드 규모 (직접 작성분만)

```
kernel/src/  : 16,704줄+ (Phase D, PE 이후 증가)
user/        :  1,101줄+
개발 기간    : 약 3주 (2026-06-29 기준)
```

`count_loc.sh`로 재현 가능 (저장소 루트에서 실행).

---

## 6. 이 프로젝트에서 반복된 패턴

```
BETA-X 7: "SharedBuffer가 빠를 것" → 0.9× (느림)
ML1:      "Bayesian이 Baseline보다 나을 것" → 57% < 62%
ML3 장기: "더 학습할수록 좋을 것" → 61% (나빠짐)
PE-5:     "Policy Engine이 CFS보다 나을 것" → CFS가 더 효율적
→ 예상이 뒤집히는 결과를 정직하게 기록하고 원인을 추적하는 것이
  이 프로젝트의 핵심 연구 방법론이다.
→ 논문 관점에서 "우리가 이겼다"보다
  "왜 이렇게 됐는지 발견하고 다음 방향을 도출했다"가 더 가치있다.
```

---

## 7. 참고 자료

- [seL4 Microkernel](https://sel4.systems/) — capability 시스템 설계
- [Redox OS](https://www.redox-os.org/) — Rust 마이크로커널 실사례
- [OSDev Wiki](https://wiki.osdev.org/) — 부트/메모리/인터럽트 레퍼런스
- [Writing an OS in Rust](https://os.phil-opp.com/) — 튜토리얼 시리즈
- [Linux syscall table](https://syscalls.mebeim.net/?table=x86/64/x86_64/latest) — ABI 구현 참조
- [ELF Specification](https://refspecs.linuxfoundation.org/elf/elf.pdf) — Phase D 참조
- [musl-libc](https://musl.libc.org/) — Phase D 동적 링커 호환 대상
- [Intel SDM Vol.2 — CPUID](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html) — PE-3 코어 특성 감지 참조
- [Linux CFS Scheduler](https://www.kernel.org/doc/html/latest/scheduler/sched-design-CFS.html) — PE-5 비교 기준1