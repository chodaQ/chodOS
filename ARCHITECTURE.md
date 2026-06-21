# MuKernel — Architecture Design

> "아치 리눅스의 자유도 + macOS의 편안함"
> Rust no_std, x86_64, Limine UEFI
>
> **현재 단계: ALPHA 완료 (1~17) → BETA 진행 중 (BETA 1~15 ✅)**
> 부팅 → 메모리 → 스케줄러 → IPC → ext4 → TCP/IP → Linux syscall →
> ELF 실행 → Shell → GUI까지 end-to-end 동작 확인됨.
>
> **최종 목표: Arch Linux pacman 실행** (`pacman -S neovim` → 설치 → 실행)
> BETA 9~28 (Phase A~F): 프로세스 모델 → FS → 메모리 → 동적 링커 → 네트워크 → pacman

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
| BETA 1 | 키보드 스캔코드 → ASCII 변환 + Shell 에코 | ✅ kbd.rs Set1 변환 테이블 완성. mushell read_line에 문자 에코 + 백스페이스(\b·space·\b) 추가 |
| BETA 2 | GUI 마우스/클릭 이벤트 처리 | ✅ 창 드래그(타이틀바), 닫기(X 버튼), 태스크바 토글, MuStart 전체 복원. wm.rs AtomicI32 동적 위치 + on_drag/on_release 이벤트. mouse.rs rising/falling edge + 홀드+이동 분기 추가 |
| BETA 3 | Linux Compat 확장 | ✅ pipe/pipe2(4KB 링버퍼), select/pselect6(stdin 블로킹 대기), epoll_create/ctl/wait(16개 fd 감시), socket/connect/bind/listen/accept/send/recv(ECONNREFUSED 스텁), rt_sigaction(핸들러 테이블), rt_sigprocmask(SIG_BLOCK/UNBLOCK/SETMASK), kill(pending 비트), poll 개선(stdin+pipe+sock 분기), Ctrl+C → SIGINT pending |
| BETA 4 | Capability Handle Table ↔ 실제 syscall 연결 | ✅ syscall/fd.rs: FileResource(path+data+AtomicUsize pos)+DirResource를 HandleTable에 등록. sys_open→fd::open_file/open_dir(진짜 fd 발급), sys_read(fd≥3)→fd::read(파일 위치 추적), sys_lseek→fd::seek, pread64→fd::pread(위치 불변), sys_close→fd::close(Arc 해제), getdents64→DirResource.path, fstat→HandleTable조회. 프로세스 시작마다 fd::init()으로 테이블 초기화. |
| BETA 5 | ✅ 멀티코어 (SMP) 지원 | 지금은 싱글코어 전제. 실제 데스크탑 CPU 대응 |
| BETA 6 | 메모리 안전성 정리 | ✅ `addr_of!/addr_of_mut!`로 mutable static reference 제거, 함수 포인터 캐스트(`as *const () as u64`) 수정, dead_code 정리. 빌드 경고 87→0 |
| BETA 7 | ✅ 추가 ELF 유틸리티 포팅 | `muls`(ls via getdents64), `mupwd`(pwd via getcwd) 추가. mushell에 ls/pwd/mkdir/cat 명령 내장. 패키지 syscall 번호 오류(200→400 등) 수정. |
| BETA 8 | ✅ 패키지 관리자 고도화 | `Package`에 `deps` 필드 추가. `AtomicU64` 설치 비트맵. 새 syscall 4개(403~406): pkg_info/install/remove/installed. mushell `mukg info/search/upgrade/installed` 추가. |

---

#### Phase A — 프로세스 모델 완성 (Linux 바이너리가 "살아 있으려면")

| Milestone | 내용 | 비고 |
|-----------|------|------|
| BETA 9  | ✅ **fork / exec / wait 완성** | `UserProcTable` (최대 8 프로세스, 64KB per-process 커널 스택). `fork_current`: 유저 주소 공간 deep-copy + isr128 복귀 프레임 구성. `wait4_impl`: 부모 block → child IRETQ. `try_wake_parent`: 자식 exit 시 부모 frame rax 기록 → IRETQ 복귀. `exec_replace`: forked child exec → 주소 공간 교체. `up_iretq_from_frame` asm 심볼. `clone_user_space` / `load_elf_into_space` 분리. TSS.RSP0 per-process 갱신. |
| BETA 10 | ✅ **시그널 서브시스템** | `signal.rs` 신규. `MuSigFrame`(120B): 트램폴린(mov rax,15;int 0x80) + 저장 컨텍스트. `deliver_pending_signals` → isr128 프레임 RIP/RSP/rdi 수정. `sys_rt_sigaction` / `sys_rt_sigprocmask` / `sys_rt_sigreturn` 실구현. `SIGSEGV`/`SIGBUS`: #PF vec14 유저모드 폴트 → 즉시 종료. `SIGCHLD`: try_wake_parent에서 부모 Running 시 pending 비트 세팅. SA_NODEFER / SA_RESETHAND 지원. |
| BETA 11 | ✅ **스레딩 (clone CLONE\_THREAD)** | `clone_thread` (SYS_CLONE=56): CLONE_VM 공유 CR3, 새 kstack, child frame 복사(rax=0/RSP=child_stack). CLONE_SETTLS → `fs_base` 저장. CLONE_CHILD_SETTID/CLEARTID → tid 기록 및 exit 시 클리어. `iretq_to_frame` → WRMSR MSR_FS_BASE(0xC0000100) per-thread TLS 복원. `futex_wait_impl`: *uaddr==val → Blocked(uaddr) + pick_next_runnable (IRETQ). `futex_wake_impl`: Blocked→Ready. `sched_yield_impl`: Ready + IRETQ to next (cooperative). thread exit: tid_ptr 클리어 → futex_wake → 자동 reap → pick_next. `sys_arch_prctl(ARCH_SET_FS)` → MSR + UserProc.fs_base 동시 저장. `current_tgid()` / `current_pid()` 분리(getpid=tgid, gettid=own). MAX_PROCS 16으로 확장. |

---

#### Phase B — 파일시스템 스택 (pacman이 `/var`, `/tmp`, `/etc`에 써야 함)

| Milestone | 내용 | 비고 |
|-----------|------|------|
| BETA 12 | ✅ **쓰기 가능 tmpfs** | 인메모리 파일시스템 (inode 트리 + 블록 벡터), `rename`/`link`/`unlink`/`chmod`/`chown`/`truncate` 완전 구현. `/proc` 가상 FS. `/tmp`, `/run`, `/var` 마운트. `*at` 변형 syscall 전체. |
| BETA 13 | ✅ **ext4 copy-up overlay** | `copy_up(path)`: ext4 읽기 전용 파일을 tmpfs로 복사 후 쓰기 가능하게. `open_writable` / `open_readonly` / `is_dir` / `exists_any` / `stat_any` 통합 API. `sys_open` + `sys_openat` 단일 경로로 통합. `pwrite64` fd≥3 지원. 디스크 write 없이 세션 내 ext4 파일 수정 가능. |
| BETA 14 | ✅ **/dev + TTY 서브시스템** | `syscall/dev.rs` 신규. DevKind: Null/Zero/Full/Random/Tty/Pts/Stdin/Stdout/Stderr. `DevResource` → HandleTable 등록. `ioctl` 완전 구현: TCGETS(termios 더미)/TIOCGWINSZ(80×24)/TIOCGPGRP/FIONREAD + fd 0/1/2도 tty로 취급 (`isatty()` 성공). `/dev/urandom` → xorshift64 PRNG. `stat_any`/`exists_any`/`is_dir`/`getdents64`에 `/dev` 통합. |
| BETA 15 | ✅ **파일 백드 mmap + munmap** | `paging::mmap_map/mmap_anon/munmap_pages/virt_to_phys` 신규. `UserProc.mmap_next` (0x40000000 bump allocator). `MMAP_TABLE: BTreeMap<vaddr, pages>` 추적. `MAP_ANONYMOUS` + `MAP_FIXED` 지원 (기존 영역 내 재매핑). 파일 백드: `fd::pread`로 offset 읽기 → `mmap_map`. `munmap`: TLB `invlpg` + `free_frame` 실제 해제. 동적 링커(ld-musl)의 두 단계 mmap 패턴 지원. |

---

#### Phase C — 메모리 서브시스템 (동적 링커의 기반)

| Milestone | 내용 | 비고 |
|-----------|------|------|
| BETA 16 | **mprotect 완전 구현** | PROT\_READ/WRITE/EXEC 페이지 단위 권한 변경, NX 비트 (Execute Disable) 적용. 동적 링커 PLT write-then-protect 패턴 필수. |
| BETA 17 | **Demand paging & 페이지 폴트** | `#PF` 핸들러에서 lazy alloc, CoW PTE 복사, stack 자동 확장(guard page 감지). |

---

#### Phase D — 동적 링커 (`.so` 없이는 어떤 앱도 안 돌아감)

| Milestone | 내용 | 비고 |
|-----------|------|------|
| BETA 19 | **ELF .so 파서 & 재배치** | `PT_LOAD` 세그먼트 mmap, `SHT_RELA`/`R_X86_64_JUMP_SLOT`/`R_X86_64_GLOB_DAT` 재배치, `.plt`/`.got.plt` 패치, `DT_NEEDED` 의존성 체인 파싱 |
| BETA 20 | **동적 링커 (`ld-musl` 호환)** | `PT_INTERP=/lib/ld-musl-x86_64.so.1` 인터프리터 실행, 심볼 해석 (`dlopen`/`dlsym`/`dlclose`), `RTLD_LAZY`/`RTLD_NOW`, 초기화 순서 (`DT_INIT_ARRAY`) |
| BETA 21 | **musl-libc 내장** | musl 1.2.x를 커널 initrd에 포함 (`/lib/ld-musl-x86_64.so.1`, `/lib/libc.so`). musl-linked 동적 바이너리 실행 성공이 이 단계의 완료 기준 |

---

#### Phase E — 네트워크 스택 (pacman이 미러에서 패키지를 받아야 함)

| Milestone | 내용 | 비고 |
|-----------|------|------|
| BETA 22 | **TCP/IP 실제 구현** | virtio-net 드라이버 완성 (현재 스텁), `smoltcp` 크레이트 통합, IP/TCP/UDP 소켓이 실제로 인터넷에 연결. `socket`/`connect`/`send`/`recv` 스텁 → 실동작 |
| BETA 23 | **DNS 리졸버** | `/etc/resolv.conf` 파싱, UDP DNS 쿼리 (포트 53), `getaddrinfo`/`getnameinfo` 구현. pacman mirrorlist의 도메인 이름 해석 필수 |
| BETA 24 | **TLS / HTTPS** | `rustls` 통합, X.509 인증서 검증 (`/etc/ssl/certs`), HTTPS 커넥션. Arch Linux 미러는 전부 HTTPS — 이 없이는 다운로드 불가 |

---

#### Phase F — 패키지 매니저 (목표 지점)

| Milestone | 내용 | 비고 |
|-----------|------|------|
| BETA 25 | **아카이브 & 압축** | `zstd` 디컴프레서 (`zstd` 크레이트), `.tar` 스트림 파서, `.pkg.tar.zst` 추출. `bzip2`/`gzip`/`xz` 폴백. pacman이 패키지 설치 시 이 형식 사용 |
| BETA 26 | **pacman-static 실행** | musl-static 빌드 pacman 커널에 내장 또는 ext4 이미지에 포함. `/etc/pacman.conf`, `/etc/pacman.d/mirrorlist`, `/var/lib/pacman/` DB 초기화. `pacman -Sy` 성공이 완료 기준 |
| BETA 27 | **glibc 호환성 레이어** | glibc symbol versioning (`GLIBC_2.17` 등), `IFUNC` 리졸버, `/lib/x86_64-linux-gnu/libc.so.6`. glibc-linked 동적 바이너리 실행. `ldd`, `ldconfig` 대응 |
| BETA 28 | **완전한 Arch Linux 환경** | glibc 동적 링크 pacman 실행, `pacman -S <pkg>` 로 임의 Arch 패키지 설치·제거·업그레이드. bash, coreutils, python 등 실제 앱 구동 확인 |

---

> **각 Phase의 완료 기준**
> - Phase A 완료 → musl-static 단순 바이너리 (hello, coreutils-static) 정상 실행
> - Phase B 완료 → `/tmp`, `/var`, `/proc` 읽기/쓰기, TTY 제어 정상
> - Phase C 완료 → 대형 ELF (수 MB) mmap 로딩, CoW fork 메모리 절약
> - Phase D 완료 → musl-linked 동적 바이너리 (`ls`, `bash` musl 빌드) 실행
> - Phase E 완료 → `curl https://archlinux.org` 성공
> - Phase F 완료 → `pacman -S neovim` → neovim 설치 후 실행 ✓

---

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