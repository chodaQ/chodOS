# chodOS

Rust로 밑바닥부터 만드는 x86_64 마이크로커널. QEMU에서 부팅부터 셸까지 동작합니다.

[![CI](https://github.com/chodaQ/chodOS/actions/workflows/ci.yml/badge.svg)](https://github.com/chodaQ/chodOS/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

성능에 관한 주장은 전부 측정해서 [EXPERIMENTS.md](EXPERIMENTS.md)에 기록합니다.


## 빠르게 실행하기

```bash
brew install qemu xorriso e2fsprogs      # macOS
rustup toolchain install nightly

make run          # 시리얼 콘솔 / Quick 모드
make run-gui      # GUI 창 포함
make run-full	  # Full 모드
```

* QEMU는 **11.0.3 이상** 권장 — 11.0.0에서 `iretq` `#GP` fault 재현 사례 ([실험 40~42](EXPERIMENTS.md))
* 빌드 없이 받아보기: [Actions 아티팩트](https://github.com/chodaQ/chodOS/actions) (커널 ELF + 부팅 ISO)
* `make run`은 데모 워크로드를 줄인 quick 모드 — 셸까지 약 5분
* `make run-full`은 전체 벤치마크 워크로드 — 약 41분. `EXPERIMENTS.md`에 기록된
  수치를 재현할 때 사용 (quick 모드 출력값과 직접 비교 불가)


## 무엇이 동작하나

```
부팅       Limine UEFI, 시리얼 출력
메모리     프레임 할당자, 페이징, 힙 32MB, Demand Paging, mprotect
프로세스   Ring3 유저스페이스, 선점형 스케줄러, fork/exec/시그널/스레딩
IPC        Zero-copy capability 기반 + 동적 fast path
파일시스템 ext4(읽기), tmpfs, VFS
드라이버   VirtIO Block/Net, PS/2 키보드·마우스
네트워크   IPv4 스택 — ARP / ICMP / UDP (TCP 미구현)
호환성     Linux syscall Tier 1, ELF 로더, musl 동적 링킹
응용       mushell, 패키지 관리자(mukg), framebuffer GUI
```

**아직 안 되는 것:** 실제 하드웨어 부팅(VirtIO 의존), 유저스페이스 소켓
(syscall 스텁 상태), 하드웨어 프로파일 자동 감지, 게임/절전 모드 전환.

## 설계

```
User Space    Policy Engine (행동 패턴 관찰 → 자원 배분)
              드라이버 / 파일시스템 / Linux 호환 Shim
──────────────────────────────────────────────────────
Kernel Space  Microkernel — IPC, 메모리, 스케줄러
```

Policy Engine은 프로세스의 `voluntary_yields`/`forced_preempts`를 EMA로 누적해
I/O 바운드와 CPU 바운드를 분류하고 우선순위·타임슬라이스·IPC 채널 생성을
조정합니다. 분류기는 Bayesian / GBDT / RL / 앙상블을 비교해 앙상블이 87%로
가장 정확했습니다.


## 문서

| | |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | 설계 원칙, 마일스톤, 로드맵 |
| [EXPERIMENTS.md](EXPERIMENTS.md)   | 실험 원본 기록  |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 개발 환경, 기여 절차, 측정 기록 규칙 |
| [SBOM.md](SBOM.md) | 사용·배포하는 제3자 소프트웨어와 라이선스 |


## 라이선스

[MIT](LICENSE)
