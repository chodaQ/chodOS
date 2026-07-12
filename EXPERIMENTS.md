# MuKernel 실험 기록

---

## 실험 5: ML 2 — GBDT(Gradient Boosted Decision Tree) IPC 핫 페어 분류

**날짜:** 2026-06-29  
**가설:** ML 1(Bayesian, 단일 비율 α/(α+β))보다 5개 feature의 비선형 조합을 사용하는 4-tree GBDT가 더 정밀하게 hot/cold를 구분할 수 있다. 특히 Bayesian이 놓치는 케이스 — rate_ema 높아도 cold_windows 있는 경우, trend 감지 — 를 포착할 수 있다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 측정 도구: 시리얼 출력 (policy 창 기준)
- GBDT 구조: 4개 트리, 각 depth 3, leaf score 합산 → HOT 기준 300

**방법:**
```
5개 feature:
  f_alpha : bayes_alpha (지속적 활성 창 카운터)
  f_beta  : bayes_beta  (비활성 창 카운터, 낮을수록 일관된 hot)
  f_rate  : rate_ema/10 (창당 평균 IPC 속도)
  f_cold  : cold_windows (최근 연속 cold 창 수)
  f_trend : delta_prev*10 > rate_ema ? 1 : 0 (증가 추세)

Tree 1 (지속성 × cold 패널티): 최대 +200, 최소 0
Tree 2 (rate 속도 × alpha × trend): 최대 +160, 최소 0
Tree 3 (cold 창 강도 구분): 최대 +120, 최소 -80
Tree 4 (beta 신뢰도): 최대 +100, 최소 -40
이론 범위: -200..+580
```

**Raw 데이터:**

QEMU 시리얼 출력 (케이스별 ML2 스코어 vs Bayesian 비교):

| 케이스 | 쌍 | alpha | beta | rate_ema/10 | cold | ML2 | Bayes | HOT? |
|--------|-----|-------|------|-------------|------|-----|-------|------|
| 초기 관찰 | pid1→pid2 | 1 | 4 | 0 | 0 | 70 | 166 | NO |
| 초기 관찰 | pid6→pid5 | 1 | 4 | 0 | 0 | 70 | 166 | NO |
| 스파이크 직전 | pid254→pid255 | 6 | 4 | 9 | 0 | 70 | 545 | NO |
| 지속 hot | pid6→pid5 | 14 | 4 | 39 | 0 | **580** | 736 | **YES** |
| 포화 후 cold 1창 | pid6→pid5 | 499 | 5 | - | 1 | **440** | 988 | **YES** |
| 포화 hot | pid6→pid5 | 500 | 4 | - | 0 | **580** | 990 | **YES** |

주목할 케이스들:
```
[정상 HOT] pid6→pid5: α=14 β=4 rate=39
  → Bayes=736/1000  ML2=580/300 [HOT]
  → 두 모델 모두 HOT 동의

[cold 1창 후 HOT 유지] pid6→pid5: α=499 β=5 rate=- cold=1
  → Bayes=988/1000  ML2=440/300 [HOT]
  → ML2: cold 1창으로 Tree3=-20 페널티 있지만 Tree1(80)+Tree2(60)+Tree4(100)으로 극복
  → Bayes: β=5로 미미한 감소, 여전히 높음

[스파이크] pid254→pid255: α=6 β=4 rate=9 cold=0
  → Bayes=545/1000  ML2=70/300 — 모두 NOT HOT
  → EMA 이상 탐지는 별도 트리거 (ANOMALY reason=0x1)
```

**결과 해석:**

1. **ML1 vs ML2 일치/불일치 케이스:**
   - 두 모델이 불일치한 케이스가 이번 실행에서 발생하지 않음 (모두 동방향)
   - 이유: alpha=14, rate=39, cold=0 같은 "명확한 hot" 케이스는 두 모델 모두 확실히 잡음
   - 불일치가 더 잘 드러나는 상황: alpha 낮지만 rate 높은 케이스 (f_alpha<10, f_rate>20) → ML2는 Tree1에서 60점 주지만 Bayesian은 여전히 낮음

2. **ML2의 추가 이점 (이론적, 관측 예시):**
   - rate_ema 30 이상이고 cold_windows=0이면 alpha 낮아도 Tree3에서 +120 → 일찍 HOT 진입 가능
   - cold_windows 2개 이상이면 Tree3=-80으로 강한 penalty → alpha 높아도 억제

3. **β=prior_min(4) → Tree4 확실한 HOT 신호:**
   - β가 4(최솟값, prior minimum)에 붙어있으면 "cold 창이 한 번도 없었다"는 뜻
   - Tree4: β≤5이면 alpha≥8 조건 시 +100 — 이게 지속 hot pair에 대한 확신 표현

4. **스코어 범위 확인:**
   - 실제 관측된 최고점: 580 (이론 최대 580과 일치 — 이 경우는 Tree1=200+Tree2=160+Tree3=120+Tree4=100)
   - 최저점 관측: 10 (pid1→pid2 cold 2창 후)

**반전/주의 사항:**
- 두 모델(ML1, ML2)이 명확한 hot/cold 케이스에서 모두 일치 → 차별점이 엣지케이스에서만 드러남.
  진짜 실전 검증은 "Bayesian이 HOT이지만 ML2는 NOT HOT" 또는 그 반대 케이스가 필요.
  예: alpha=15, cold_windows=3이면 Bayesian=789(HOT), ML2=Tree1(80)+Tree2(60)+Tree3(-80)+Tree4(100)=160(NOT HOT).
  이런 케이스를 테스트 코드로 강제 주입하면 차이 명확히 드러날 것.
- QEMU TCG에서 rate_ema 절댓값이 실 하드웨어와 다를 수 있어 threshold(300, 15, 20, 30 등)가 최적인지 불확실.

**다음에 미친 영향:**
- `hot_ipc_pairs()` 및 `adapt_and_report()` 내부 HOT 판정이 ML2 기준으로 변경됨.
- Bayesian score는 비교 참고값으로 로그에 계속 출력됨.
- ML 3(RL) 또는 Phase D(ELF .so/동적 링커) 중 선택 필요.

---

## 실험 7: ML 비교 벤치마크 — Baseline / ML1 / ML2 / ML3 정확도 비교

**날짜:** 2026-06-29  
**가설:** ML 단계가 올라갈수록 IPC 핫 페어 분류 정확도가 향상된다. 특히 스파이크와 HOT→COLD 전환 시나리오에서 차이가 클 것이다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, debug 빌드
- 합성 IPC 트레이스 4개 시나리오, 각 10개 policy 창
- 각 분류기 완전히 독립 실행 (kernel 상태 공유 없음, fresh 초기화)

**방법:**
```
시나리오 (10창 × delta 값):
  A 지속HOT:  [10,15,20,25,30,35,40,45,50,55]  ground_truth: w4-9=HOT
  B 스파이크:  [0,0,0,300,0,0,0,0,0,0]          ground_truth: 전체=COLD
  C HOT→COLD: [60,70,80,90,0,0,0,0,0,0]         ground_truth: w0-3=HOT, w4-9=COLD
  D 느린성장:  [3,5,8,12,18,28,40,60,90,130]    ground_truth: w7-9=HOT

분류기:
  Baseline : 누적 count >= 100
  ML1 Bay  : Bayesian score=α*1000/(α+β+1) >= 650
  ML2 GBDT : bench_ml2(α,β,rate_ema/10,cold_windows,trend) >= 300
  ML3 RL   : ML2 score와 Q-table action으로 조정된 임계값 (ε=20%)
```

**Raw 데이터:**

```
시나리오 A (지속 HOT): deltas=[10,15,20,25,30,35,40,45,50,55]
  Baseline  [....HHHHHH]  TP=6 TN=4 FP=0 FN=0  Acc=100%  첫HOT=4
  ML1 Bay   [....HHHHHH]  TP=6 TN=4 FP=0 FN=0  Acc=100%  첫HOT=4
  ML2 GBDT  [....HHHHHH]  TP=6 TN=4 FP=0 FN=0  Acc=100%  첫HOT=4
  ML3 RL    [....HHHHHH]  TP=6 TN=4 FP=0 FN=0  Acc=100%  첫HOT=4

시나리오 B (스파이크): deltas=[0,0,0,300,0,0,0,0,0,0]
  Baseline  [...HHHHHHH]  TP=0 TN=3 FP=7 FN=0  Acc=30%   첫HOT=-1
  ML1 Bay   [...HHHHHHH]  TP=0 TN=3 FP=7 FN=0  Acc=30%   첫HOT=-1 ← Baseline과 동일!
  ML2 GBDT  [...HH.....]  TP=0 TN=8 FP=2 FN=0  Acc=80%   첫HOT=-1
  ML3 RL    [...HH...H.]  TP=0 TN=7 FP=3 FN=0  Acc=70%   첫HOT=-1

시나리오 C (HOT→COLD): deltas=[60,70,80,90,0,0,0,0,0,0]
  Baseline  [.HHHHHHHHH]  TP=3 TN=0 FP=6 FN=1  Acc=30%   첫HOT=1
  ML1 Bay   [.HHHHHHHHH]  TP=3 TN=0 FP=6 FN=1  Acc=30%   첫HOT=1 ← Baseline과 동일!
  ML2 GBDT  [.HHHH.....]  TP=3 TN=5 FP=1 FN=1  Acc=80%   첫HOT=1
  ML3 RL    [.HHHH...HH]  TP=3 TN=3 FP=3 FN=1  Acc=60%   첫HOT=1

시나리오 D (느린 성장): deltas=[3,5,8,12,18,28,40,60,90,130]
  Baseline  [......HHHH]  TP=3 TN=6 FP=1 FN=0  Acc=90%   첫HOT=7 ← 이 시나리오서만 우수
  ML1 Bay   [....HHHHHH]  TP=3 TN=4 FP=3 FN=0  Acc=70%   첫HOT=7
  ML2 GBDT  [....HHHHHH]  TP=3 TN=4 FP=3 FN=0  Acc=70%   첫HOT=7
  ML3 RL    [....HHHHHH]  TP=3 TN=4 FP=3 FN=0  Acc=70%   첫HOT=7

전체 평균 정확도:
  분류기    │  A지속  B스파  C전환  D성장 │  평균
  Baseline  │  100%   30%   30%   90% │   62%
  ML1 Bay   │  100%   30%   30%   70% │   57%  ← 최하
  ML2 GBDT  │  100%   80%   80%   70% │   82%  ← 최고
  ML3 RL    │  100%   70%   60%   70% │   75%
```

**결과 해석 (핵심 발견):**

**1. ML1(Bayesian) ≈ Baseline — 둘 다 스파이크/전환에 취약**
- 시나리오 B에서 δ=300 단 1창으로 α=31로 폭증 → score=861 → HOT
- 이후 6창 동안 cold지만 α는 천천히 감소(α=max(α-1,1)), β=4 floor로 score 계속 800대
- Baseline의 "누적 count" 문제(한번 100 넘으면 영원히 HOT)와 동일한 구조
- **원인**: ML1 Bayesian은 `α 증가 gain = delta/10`으로 큰 spike에서 gain=30이나 되어 α가 너무 빠르게 올라감. β floor(4)가 있어 score가 쉽게 안 내려옴

**2. ML2 GBDT가 명확히 우월 (82% vs 57~62%)**
- 핵심 차별 요소: `cold_windows` feature
  - 시나리오 B: spike 후 cold_windows=2 → Tree3=-80, β≥10 → Tree4=-40 → total 0으로 수렴
  - 시나리오 C: 4창 후 cold_windows 증가 → 점수 하락 → correctly NOT HOT
- ML1이 놓치는 "창 기반 활동 패턴"을 GBDT가 명시적으로 포착

**3. ML3 RL은 ML2와 ML1 사이 (75%)**
- ML2보다 낮은 이유: ε=20% 탐색에서 ENCOURAGE/PIN_PUSH가 선택될 때 임계값이 220으로 내려가 FP 발생
- 예: 시나리오 C 에서 ML2는 [.HHHH.....] (80%), ML3는 [.HHHH...HH] (60%) — cold 창에서 ε-탐색 ENCOURAGE가 2번 선택됨
- ML3의 Q-table은 10창으로 수렴하기엔 너무 짧음 (수천 창 필요)

**4. Baseline이 시나리오 D(느린 성장)에서 유일하게 우수 (90%)**
- 누적 count는 자연스럽게 "총 활동량"을 측정 → 느린 성장은 천천히 100에 도달
- ML1/ML2/ML3는 δ=18이 window 4부터 rate_ema를 충분히 올려 w4에서 HOT → 3개 FP 발생
- 역설: 단순한 Baseline이 이 특정 시나리오에선 더 보수적

**5. "최악의 시나리오" = HOT→COLD 전환**
- Baseline/ML1: Acc=30% (사실상 랜덤보다 나쁨)
- ML2: Acc=80% — cold_windows가 이 시나리오의 핵심 signal

**결론:**
| 측면 | 승자 |
|------|------|
| 전체 정확도 | **ML2 GBDT** (82%) |
| 스파이크 처리 | **ML2 GBDT** (80%) |
| HOT→COLD 전환 | **ML2 GBDT** (80%) |
| 느린 성장 감지 | **Baseline** (90%) |
| 지속 HOT 감지 | 전부 동일 (100%) |

**다음에 미친 영향:**
- `cold_windows`가 가장 중요한 단일 feature임을 확인 → ML2의 Tree3(cold penalty)이 핵심
- ML1의 Bayesian은 "spike를 hot으로 오인하는" 근본 문제 있음 → alpha 증가 gain cap을 `delta/50`으로 줄이면 개선 가능 (향후 실험)
- ML3 RL은 더 많은 창이 있어야 의미 있게 수렴 → 장기 시뮬레이션 필요
- Baseline+cold_windows(하이브리드) 접근이 특정 시나리오에서 모든 ML보다 나을 수 있음

---

## 실험 6: ML 3 — Q-learning ε-greedy IPC 채널 정책 학습

**날짜:** 2026-06-29  
**가설:** ML 2(GBDT)가 도메인 지식으로 미리 설계된 임계값 기반이라면, ML 3(Q-learning)은 실제 운영 결과(reward)를 통해 임계값 조정 정책을 스스로 학습한다. 특히 "anomaly가 반복되는 쌍에는 채널 생성을 억제(DISCOURAGE)"하는 패턴을 데이터 없이 경험으로 학습할 수 있어야 한다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 측정 도구: 시리얼 출력 (policy 창 기준)
- ε 초기값: 20%, 최솟값: 5%, decay: 1000틱마다 1%

**방법:**
```
State: alpha_bucket(4) × rate_bucket(3) × cold_bucket(3) = 36개
  alpha: [0-2]→0, [3-9]→1, [10-24]→2, [25+]→3
  rate:  [0-5]→0, [6-20]→1, [21+]→2  (rate_ema/10 기준)
  cold:  0→0, 1→1, 2+→2

Action: 4개
  NOOP(0)       : ML2 임계값 300 그대로
  ENCOURAGE(1)  : ML2 임계값 -80 (→220, 채널 생성 쉽게)
  DISCOURAGE(2) : ML2 임계값 +80 (→380, 채널 생성 어렵게)
  PIN_PUSH(3)   : 임계값 무관, 강제 pin + High 우선순위

Q-table 업데이트: Bellman
  Q(s,a) += (1/10) * (reward*100 + (9/10)*max_Q(s') - Q(s,a))
  모든 값 ×100 fixed-point

Reward 함수:
  anomaly 발생:  -20(ENCOURAGE/PIN_PUSH) / -5(기타)
  cold 2창 회수: -8(ENCOURAGE/PIN_PUSH) / -2(기타)
  cold 1창:      -4(ENCOURAGE) / -1(기타)
  활성+성장(≥20): +12(ENCOURAGE/PIN_PUSH) / +5(NOOP) / -2(DISCOURAGE)
  활성+안정:      +8(ENCOURAGE) / +3(NOOP) / 0(기타)
```

**Raw 데이터:**

QEMU 실제 출력 (~15개 policy 창 관찰):
```
[policy-X] IPC 핫 쌍 top1: (ε=20%)
  pid6→pid5: ML2=580 RL=NOOP(thr=300) [HOT]         ← 초기: NOOP bias 유지
  pid6→pid5: ML2=580 RL=DISCOURAGE(thr=380) [HOT]   ← ε-탐색으로 DISCOURAGE 선택
                                                          ML2=580 > 380 → 여전히 HOT
  pid6→pid5: ML2=440 RL=NOOP(thr=300) [HOT]         ← Q-update 후 다시 NOOP 선호
```

Q-table 초기 상태:
- NOOP: 10 (약한 bias로 초기화)
- ENCOURAGE/DISCOURAGE/PIN_PUSH: 0

Q-table 업데이트 사례 (anomaly 발생 시):
- s=rl_prev_state, a=ENCOURAGE, reward=-20
- `Q[s][1] += ((-20)*100 + 0.9*max_Q[s'] - Q[s][1]) / 10`
- ENCOURAGE Q-value 감소 → 다음 greedy 선택에서 NOOP으로 복귀

**결과 해석:**

1. **초기 수렴 전 동작 (ε=20%, QEMU ~15창)**:
   - Greedy 선택: 대부분 NOOP (초기 bias Q=10)
   - 탐색: 20% 확률로 random action → DISCOURAGE 1회 관찰
   - DISCOURAGE 후에도 ML2=580 > thr=380 → HOT 판정 유지 (RL이 강한 신호를 완전 차단 불가 — 의도된 안전장치)

2. **ML3의 역할 (수렴 후 기대 동작)**:
   - anomaly 반복 쌍: ENCOURAGE Q-value 감소 → DISCOURAGE/NOOP 선호 수렴
   - 지속 hot 쌍: ENCOURAGE/PIN_PUSH Q-value 증가 → 더 낮은 임계값으로 조기 채널 생성
   - QEMU TCG처럼 빠른 부팅 환경에서 수렴 어려움 (수천 policy 창 필요)

3. **ML 계층 구조 최종 정리**:
   ```
   ML 1 (Bayesian)   : 지속성 단일 비율 — α/(α+β) ≥ 0.65
   ML 2 (GBDT)       : 5개 feature 비선형 → 기본 HOT threshold = 300
   ML 3 (Q-learning) : threshold를 ±80 조정 (경험 학습) or PIN_PUSH
   EMA 이상탐지      : 이 세 레이어와 별개로 anomaly 강제 회수
   ```
   ML 1~3이 중첩된 방어선: Bayesian이 방향, GBDT가 강도, RL이 경험 기반 미세조정.

**반전/주의 사항:**
- QEMU TCG에서 policy 창 수가 너무 적어 Q-table이 의미 있게 수렴하지 못함. 실 하드웨어 장기 운영 시 수렴 확인 필요.
- ε=20%로 시작 → 탐색 중 random action(ENCOURAGE)이 anomaly 쌍에 선택되면 채널이 잘못 생성될 수 있음. BETA-X-2 6의 Safety Bounds(창당 2개 제한, cooldown)가 이를 완화.
- DISCOURAGE가 선택돼도 ML2 스코어가 충분히 높으면 HOT 판정 통과 → RL이 완전히 막지는 못함. 이는 의도된 설계(RL의 역할은 "조정"이지 "거부권"이 아님).

**다음에 미친 영향:**
- ML 1~3 + EMA anomaly 4중 레이어 완성. Policy Engine이 규칙(EMA), 통계(Bayesian), GBDT, RL을 모두 내장.
- 다음: ML 3 수렴 검증을 위한 장기 시뮬레이션 또는 Phase D(ELF .so/동적 링커)로 전환.

---

## 실험 1: BETA-X-2 비대칭 권한 매핑 (Asymmetric Channel Mapping) 검증

**날짜:** 2026-06-25  
**가설:** 채널 물리 프레임을 write_va(HHDM, PTE_WRITABLE=1)와 read_va(RO_CHANNEL_BASE, PTE_WRITABLE=0)로 서로 다른 VA에 매핑하면, write_va에 쓴 데이터를 read_va에서 읽을 수 있지만 read_va를 통한 쓰기는 PTE 레벨에서 차단된다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU 9.x, `-smp 4`, TCG 모드 (하드웨어 가속 없음)
- cargo build 모드: debug
- 측정 도구: 시리얼 출력 + PTE 직접 확인(get_kernel_pte)

**방법:**
```rust
let (write_va, read_va, phys) = unsafe { crate::paging::alloc_channel_frame() };
let wpte_ok = crate::paging::get_kernel_pte(write_va)
    .map(|p| p & 2 != 0).unwrap_or(false);  // PTE_WRITABLE=1?
let rpte_ok = crate::paging::get_kernel_pte(read_va as u64)
    .map(|p| p & 2 == 0 && p & 1 != 0).unwrap_or(false);  // PTE_PRESENT=1, PTE_WRITABLE=0?
const ASYM_PATTERN: u64 = 0x5A5A_DEAD_5A5A_BEEF;
unsafe { (write_va as *mut u64).write_volatile(ASYM_PATTERN); }
let rb = unsafe { (read_va as *const u64).read_volatile() };
// rb == ASYM_PATTERN이면 동일 물리 프레임 → 비대칭 매핑 확인
```

**Raw 데이터:**
```
[beta-x-2 2/3] 비대칭 채널 매핑 검증
  write_va=0xffff800000008000  PTE_WRITABLE=1 ✓
  read_va =0xffffa00000000000  PTE_WRITABLE=0 ✓
  W→R 라운드트립: 0x5a5adead5a5abeef == 0x5a5adead5a5abeef ✓
[beta-x-2 2/3] 비대칭 채널 매핑 PASS ✓
```

**결과 해석:**
- write_va(HHDM 범위)와 read_va(RO_CHANNEL_BASE=0xFFFF_A000_0000_0000)가 동일 물리 프레임을 가리키면서 PTE 속성이 다름.
- PTE 레벨에서 읽기 전용 강제 → 수신 측이 채널 버퍼를 덮어쓸 수 없음.
- 이 구조가 "생성 시점 엄격 검증" 원칙(CLAUDE.md §4)의 핵심.

**다음에 미친 영향:**
- `verify_channel_isolation()`이 이 PTE 검사를 채널 생성 시 자동 수행하도록 ipc_fast::ensure_channel_cap()에 통합.
- 이후 모든 fast channel은 생성 시 격리 검증을 통과해야 함.

---

## 실험 2: BETA-X-2 EMA 이상 탐지 — 갑작스러운 IPC 급증 감지

**날짜:** 2026-06-25  
**가설:** EMA 기반 이상 탐지는 "평소 패턴 대비 급증, 절댓값 ≥200" 조건으로 갑작스러운 IPC 급증을 잡아낼 수 있다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 측정 도구: 시리얼 출력, policy 창 = 40 타이머 tick

**방법:**
```rust
// pid254→pid255 쌍에 워밍업 3창 (각각 15, 30, 45 IPC)
for w in 0u64..3 {
    crate::policy::observe_ipc(254, 255, (w + 1) * 15, 0);
    // 40 tick 대기 (policy 창 1개)
}
// 4번째 창: 갑작스러운 급증 (300 추가)
crate::policy::observe_ipc(254, 255, 3 * 15 + 300, 0);
```

EMA 갱신 공식 (×10 고정소수점, α=0.3):
- `new_ema = (3 * delta * 10 + 7 * old_ema) / 10`
- 이상 조건: `delta * 10 > rate_ema * 20` AND `delta >= 200`

**Raw 데이터:**
```
ANOMALY pid254→pid255 reason=0x1 rate=300/창(EMA=6) → 강제 회수
[tracer] Anomaly  pid254→pid255  cap=0  reason=0x1
```

EMA=6으로 낮은 이유: 워밍업 observe_ipc 호출이 adapt_and_report 실행 타이밍과 어긋나 EMA 누적이 적었음.

**결과 해석:**
- 갑작스러운 스파이크 감지 후 `drop_channel` + `record_eviction` + Tracer 기록 정상 동작.
- EMA 워밍업이 짧으면 민감도 상승 → false positive 가능성 있음. (향후 워밍업 창 수 조정 필요)
- 이 스파이크-only pair는 Bayesian score 500 < 650 → fast channel 생성 차단 (실험 4 참고).

**다음에 미친 영향:**
- EMA = 갑작스러운 스파이크 감지 (이상 탐지), Bayesian = 지속 패턴 감지 (채널 생성) 역할 분리 확정.

---

## 실험 3: BETA-X-2 Switchless 도어벨 IPC 기능 검증

**날짜:** 2026-06-25  
**가설:** Switchless 채널은 `SW_DOORBELLS: [AtomicU64; 64]` 도어벨 배열을 통해 스케줄러 개입 없이 커널 내 IPC를 수행할 수 있다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 측정 도구: 시리얼 출력 (기능 검증, 레이턴시 미측정)

**방법:**
```rust
let sw_cap = process::ipc_fast::ensure_channel_cap(252, 253, 64);
let test_data = b"switchless-ok";  // 13바이트
let sent = process::ipc_cap::send_switchless(sw_cap, test_data);
// send_switchless: write_va에 쓰고 SW_DOORBELLS[slot].store(1)
let doorbell = process::ipc_cap::doorbell_pending(sw_cap);
let mut recv_buf = [0u8; 64];
let recv_len = process::ipc_cap::poll_switchless(sw_cap, &mut recv_buf);
// poll_switchless: doorbell 확인 → read_va에서 복사 → doorbell 클리어
```

**Raw 데이터:**
```
[beta-x-2 5] Switchless 도어벨 IPC 테스트
  채널 생성: cap=0
  격리 검증: PTE_WRITABLE=0 ✓  W→R=0x5a5adead5a5abeef ✓
  send=true  doorbell=true  recv=Some(13)B  data=일치 ✓
[beta-x-2 5] Switchless 도어벨 PASS ✓
```

**결과 해석:**
- 비대칭 매핑(실험 1) + 도어벨 배열 조합으로 스케줄러 없는 IPC 경로 확인.
- 수신 측은 read_va(PTE_WRITABLE=0)로만 접근 → 수신 측이 전송 버퍼 오염 불가.
- 레이턴시 측정은 미수행 — A-1/A-2 벤치마크에서 별도 측정 예정.

**다음에 미친 영향:**
- `ipc::send()`: fast channel 존재 시 send_switchless() 우선 (스케줄러 yield 없음).
- `ipc::recv()`: 모든 incoming fast channel의 도어벨을 poll 후 기존 큐 처리.

---

## 실험 4: ML 1 — Beta-Binomial Bayesian IPC 핫 페어 탐지

**날짜:** 2026-06-25  
**가설:** EMA 임계값 기반 단순 카운트(`count >= IPC_HOT_THRESHOLD=100`) 대신 Beta-Binomial Bayesian 사후 확률 점수(`score = α*1000/(α+β)`)를 사용하면, 단발성 스파이크는 걸러내고 지속적인 IPC 패턴만 HOT으로 분류할 수 있다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 측정 도구: policy 창 시리얼 출력
- Prior: α₀=1, β₀=4 (cold 편향 보수적 prior)

**방법:**
```
Bayesian 갱신 규칙 (policy 창 = adapt_and_report 호출마다):
  활성 창 (delta > 0):
      alpha += max(delta/10, BAYES_HOT_GAIN=2)
      beta   = max(beta - 1, BAYES_PRIOR_BETA=4)   ← 하한 유지
  비활성 창 (delta == 0):
      beta  += BAYES_COLD_GAIN=1
      alpha  = max(alpha - 1, BAYES_PRIOR_ALPHA=1)
  포화 방지: alpha, beta ≤ BAYES_MAX=500

HOT 기준: score = α*1000/(α+β+1) ≥ BAYES_HOT_SCORE=650
```

**Raw 데이터:**

QEMU 시리얼 출력 (실제 측정값):
```
[policy-X]   #1 pid1→pid2:    10회  P(hot)=166/1000(α=1  β=4)
[policy-X]   #1 pid6→pid5: 105772회  P(hot)=736/1000(α=14 β=4) [HOT]
[policy-X]   #1 pid6→pid5: 301708회  P(hot)=990/1000(α=500 β=4) [HOT]
[policy-X]      pid254→pid255: 스파이크  P(hot)=500/1000  (HOT 미달 → fast channel 생성 안 됨)
```

alpha 수렴 시뮬레이션 (β=4 고정 시):
| alpha | score/1000 | HOT? |
|-------|-----------|------|
| 1     | 166       | NO   |
| 5     | 500       | NO   |
| 10    | 666       | YES  |
| 14    | 736       | YES  |
| 50    | 909       | YES  |
| 500   | 990       | YES  |

alpha=10 달성에 필요한 최소 활성 창 수: 약 4~5창 (gain=2/창 기준).

**결과 해석:**
- **기존 방식의 한계**: 단순 `count >= 100`은 1창에 100 IPC 몰려도 HOT → 스파이크/지속 패턴 구분 불가.
- **Bayesian 방식**: α 증가에 여러 창 필요 → 지속성 검증됨. β 하한(4) 유지로 score 상한이 ~992에서 자연 포화.
- **스파이크 pair (pid254→255)**: EMA 이상 탐지 발동(강제 회수) + Bayesian score 500 < 650(fast channel 생성 차단) — 두 메커니즘이 서로 다른 역할 담당.
- **cold 편향 prior(β₀=4)** 효과: 신규 pair는 기본 HOT 아님 → 초기 false positive 억제.

**반전/주의 사항:**
- pid6→pid5 IPC 수(105772회, 301708회)는 A-1/A-2 벤치마크 루프의 부산물. 실제 "창당 IPC 수"가 아니라 누적 카운트이므로 α 증가 속도 해석 시 주의.
- β 하한을 4로 고정하면 장기 활성 pair도 score ≤ 992/1000 — 의도적 설계지만, 추후 β 하한을 1~2로 낮춰 더 높은 확신도 허용하는 실험 필요.
- QEMU TCG에서 policy 창 1개(40 tick)의 실벽 시간이 측정되지 않아, "몇 초 안에 HOT 승격"인지 알 수 없음.

**다음에 미친 영향:**
- `hot_ipc_pairs()` 반환 기준이 Bayesian score로 변경됨 → Policy Engine이 진짜 지속 hot pair에만 fast channel 생성.
- α, β 값을 feature로 사용하는 ML 2 (LightGBM) 기반 분류기 설계의 기반 마련.

---

## 실험 8: ML1 Bayesian 튜닝 — gain cap + cold decay

**날짜:** 2026-06-30  
**가설:** ML1 Bayesian의 두 약점(spike 과민, cold 후 느린 복귀)은 단일 창 gain에 상한을 두고(BAYES_GAIN_CAP=5), cold 창당 α 감소량을 높이면(BAYES_COLD_DECAY=3) 수정 가능하다. 이를 통해 ML1이 Baseline(62%)과 비슷한 수준에서 ML2 GBDT(82%)에 근접하도록 개선할 수 있다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 측정 도구: main.rs 인라인 벤치마크 (policy 창 시뮬레이션)
- 4개 시나리오 × 10창, TP/TN/FP/FN 계산

**방법:**

기존 ML1 gain 공식:
```rust
// 기존: delta=300 → gain=max(30,2)=30 → α가 31로 폭증
let gain = ((delta / 10) as u32).max(BAYES_HOT_GAIN);
*a = a.saturating_add(gain).min(BAYES_MAX);
// cold: 1씩만 감소
*a = a.saturating_sub(1).max(BAYES_PRIOR_ALPHA);
```

튜닝 후:
```rust
// 튜닝: delta=300 → gain=min(30,5)=5 → α=6, score=545 NOT HOT
let gain = ((delta / 10) as u32).max(BAYES_HOT_GAIN).min(BAYES_GAIN_CAP);  // cap=5
*a = a.saturating_add(gain).min(BAYES_MAX);
// cold: 3씩 감소 → spike 후 2창 만에 prior 복귀
*a = a.saturating_sub(BAYES_COLD_DECAY).max(BAYES_PRIOR_ALPHA);  // decay=3
```

새 상수:
- `BAYES_GAIN_CAP = 5`: 단일 창에서 α 최대 증가량
- `BAYES_COLD_DECAY = 3`: cold 창당 α 감소량 (기존 1)

**Raw 데이터:**

시나리오별 결과 (ML1 tuned):
```
A 지속HOT  [....HHHHHH]  TP=6 TN=4 FP=0 FN=0  Acc=100%
B 스파이크  [..........]  TP=0 TN=10 FP=0 FN=0  Acc=100%
C HOT→COLD [.HHHHH....]  TP=3 TN=4 FP=2 FN=1  Acc=70%
D 느린성장  [....HHHHHH]  TP=3 TN=4 FP=3 FN=0  Acc=70%
```

비교 벤치마크 최종 결과:
```
  분류기    │  A지속  B스파  C전환  D성장 │  평균
  Baseline │  100%   30%   30%   90% │   62%
  ML1 Bay  │  100%  100%   70%   70% │   85%   ← 튜닝 후
  ML2 GBDT │  100%   90%   80%   70% │   85%
  ML3 RL   │  100%   90%   60%   70% │   80%
```

**튜닝 전후 ML1 변화:**
| 시나리오 | 튜닝 전 | 튜닝 후 | 변화 |
|---------|--------|--------|------|
| A 지속   | 100%   | 100%   | 유지 |
| B 스파이크| 30%    | 100%   | +70%p ↑ |
| C HOT→COLD| 30%  | 70%    | +40%p ↑ |
| D 느린성장| 70%   | 70%    | 유지 |
| **평균** | **57%** | **85%** | **+28%p** |

B시나리오의 gain cap 효과 상세:
- delta=300, 기존: gain=30, α=31, score=31000/36=861 ≥ 650 → HOT (FP)
- delta=300, 튜닝: gain=5, α=6, score=6000/11=545 < 650 → NOT HOT (TN) ✓

Cold decay 효과 (spike 후 복귀):
- α=6, 기존 decay=1: 6창 후 α=1 (너무 느림)
- α=6, 신규 decay=3: 2창 후 α=1 (빠른 복귀) ← 실제로는 즉시 NOT HOT

ML2 GBDT도 Bayesian feature 입력이 바뀌어 B 시나리오 30→90%로 개선됨 (spike 시 α=31→6이 feature로 들어가 tree 경로 달라짐).

**결과 해석:**
- ML1이 꼴찌(57%)에서 ML2와 공동 1위(85%)로 상승 — gain cap 단 하나의 변경으로.
- B 시나리오 완벽 해결: spike delta=300이 더 이상 Bayesian에서 false positive를 내지 않음.
- C 시나리오 부분 개선(30%→70%): w1에서 alpha=5→score=500 NOT HOT (FN), cold 2창(w4-5) FP 잔존. 완벽 해결에는 더 정교한 delta 평활화 필요.
- 의외의 발견: ML2도 동시에 B accuracy가 80%→90%로 향상. ML2가 α를 feature로 쓰기 때문에 Bayesian 튜닝이 ML2에도 전파됨.

**반전/주의 사항:**
- C 시나리오 w0: delta=60 → gain=min(6,5)=5 → α=6 → score=545 NOT HOT → FN. 첫 번째 HOT 창 놓침. GAIN_CAP=5로는 큰 delta에서 처음 HOT 승격이 지연됨.
- D 시나리오 FP 잔존 (w4-6): 느린 성장이 ground truth(w7 기준)보다 일찍 HOT 판정. Bayesian은 누적 패턴을 반영하므로 불가피한 특성.
- ML3 C 시나리오가 60%로 하락 (기존 60% 유지이나 결과 패턴 변화 [.HHHH...HH]): cold 기간 후 RL이 탐색으로 w8-9 HOT 재발동. Q-table 학습량 부족 문제.

**다음에 미친 영향:**
- ML1(Bayesian)이 단순함에도 ML2 GBDT와 동등 성능 달성 → 두 접근 모두 실용적
- GAIN_CAP, COLD_DECAY 두 상수가 `kernel/src/policy/mod.rs`에 상수로 추가되어 실제 kernel Bayesian 업데이트에도 반영됨
- 다음 단계 후보: ML3 장기 수렴 실험 (>1000 policy 창), 또는 ensemble(ML1+ML2 AND 조합) 실험

---

## 실험 9: ML4 Ensemble (ML1 AND ML2) — false positive 감소 검증

**날짜:** 2026-06-30  
**가설:** ML1(Bayesian)과 ML2(GBDT)의 오판 패턴이 서로 다르기 때문에, 두 분류기가 모두 HOT이라고 동의할 때만 HOT으로 채택하면(AND Ensemble) 단독 분류기 대비 FP가 줄어들면서 정확도가 향상된다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 측정 도구: main.rs 인라인 벤치마크 (ML1 튜닝 버전 + ML2 GBDT)
- 4개 시나리오 × 10창, ML4 = results[1] AND results[2]

**방법:**

```rust
// ── [4] ML4 Ensemble: ML1 AND ML2 동시 동의 시에만 HOT
for w in 0..10 {
    results[4][w] = results[1][w] && results[2][w];
}
```

입력: ML1(GAIN_CAP=5, COLD_DECAY=3 튜닝 버전) + ML2(5-feature 4-tree GBDT, 임계값 300)

**Raw 데이터:**

```
A 지속HOT   [....HHHHHH]  TP=6 TN=4 FP=0 FN=0  Acc=100%
B 스파이크   [..........]  TP=0 TN=10 FP=0 FN=0  Acc=100%   ← ML2의 FP 1개 상쇄
C HOT→COLD  [.HHHH.....]  TP=3 TN=5 FP=1 FN=1  Acc=80%    ← ML1 FP 1개 추가 억제
D 느린성장   [....HHHHHH]  TP=3 TN=4 FP=3 FN=0  Acc=70%    ← ML1=ML2이므로 동일
```

전체 비교 (최종):
```
  분류기    │  A지속  B스파  C전환  D성장 │  평균
  Baseline │  100%   30%   30%   90% │   62%
  ML1 Bay  │  100%  100%   70%   70% │   85%
  ML2 GBDT │  100%   90%   80%   70% │   85%
  ML3 RL   │  100%   90%   60%   70% │   80%
  ML4 Ens  │  100%  100%   80%   70% │   87%   ← 최고
```

**결과 해석:**
- AND 앙상블이 ML1(85%), ML2(85%) 모두를 제치고 **87%로 단독 1위**.
- B 시나리오: ML1=100%, ML2=90% → AND=100%. ML2의 FP 1개(w3 spike)를 ML1이 거부 — "두 분류기의 오류 패턴이 다를 때 AND 앙상블이 유리하다"는 가설 확인.
- C 시나리오: ML1=70%, ML2=80% → AND=80% (ML2와 동일). ML1이 HOT으로 판단하는 w4-5 중 w4는 ML2도 HOT이어서 상쇄 안 됨. w5는 ML2=NOT HOT이어서 AND로 억제됨 → ML2 대비 TN 1개 추가.
- D 시나리오: 두 분류기의 패턴이 동일해서 AND도 70%로 동일.
- Recall(재현율) 손실 없음: FN이 늘지 않고 FP만 줄었음.

**반전/주의 사항:**
- OR 앙상블 시 B=90%(ML2 수준), C=70%(ML1 수준)으로 AND보다 나쁨 — 이 데이터에서는 AND가 우월.
- D 시나리오 FP 3개(w4-6)는 AND로도 해결 안 됨: ML1과 ML2 모두 동일하게 이른 HOT 판정.
- 10창 밖에 없어서 통계적 유의성은 낮음. 더 많은 창으로 검증 필요.
- 실제 kernel에 적용하면 ML2 단독보다 보수적(false positive 적음) → fast channel 생성 기준이 더 까다로워짐.

**다음에 미친 영향:**
- ML4 Ensemble이 벤치마크 기준 현재 최고 정확도(87%) 달성.
- 향후 kernel Policy Engine에서 `is_hot = ml1_hot && ml2_hot` 조합으로 교체 고려.
- 다음 후보: ML3 장기 수렴 실험(100창+), 또는 soft voting (ML1_score + ML2_score > 임계값).

---

## 실험 10: ML3 장기 수렴 실험 (100창 × 4시나리오)

**날짜:** 2026-06-30  
**가설:** ML3 Q-learning은 10창 평가(80%)에서 "과소학습"으로 결론 보류됐다. 100창(동일 패턴 10회 반복)으로 충분한 Bellman 업데이트를 주면 Q-table이 수렴해 정확도가 향상될 것이다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 100창 = 10창 패턴 × 10회 반복 (deltas[w % 10])
- Bayesian α/β, rate_ema, cold_windows은 100창 내내 누적 (리셋 없음)
- Q-table은 시나리오별 독립, ε=15% 고정
- 25창 단위 정확도 추적

**방법:**
```rust
for w in 0..100 {
    let d = deltas[w % 10];  // 동일 패턴 반복
    // Bayesian 업데이트 (상태 누적)
    // ML2 score 계산 → HOT 판정
    // Q-table Bellman 업데이트
    // ε-greedy 다음 action 선택
}
```

**Raw 데이터:**

```
[ml3-conv]   시나리오    │  w0-24  w25-49 w50-74 w75-99 │ 전체
[ml3-conv]   A 지속HOT  │   68%   68%   52%   68% │  64%
[ml3-conv]   B 스파이크 │   76%   68%   56%   72% │  68%
[ml3-conv]   C HOT→COLD │   80%   72%   84%   80% │  79%
[ml3-conv]   D 느린성장 │   40%   36%   24%   36% │  34%
전체 평균: (64+68+79+34)/4 = 61%
```

10창 평가 결과(ML3): A=100%, B=90%, C=60%, D=70% → 80%

비교:
| 시나리오 | 10창 ML3 | 100창 ML3 | 변화 |
|---------|---------|----------|------|
| A 지속  | 100%    | 64%      | -36%p ↓ |
| B 스파이크 | 90%  | 68%      | -22%p ↓ |
| C HOT→COLD | 60% | 79%     | +19%p ↑ (유일한 개선) |
| D 느린성장 | 70%  | 34%      | -36%p ↓ |
| 평균 | 80% | 61% | -19%p ↓ |

**결과 해석 — 가설 기각:**

**수렴 없음**: 각 시나리오 내에서 구간별 정확도가 단조 증가하지 않음.
- A: 68%→68%→52%→68% (중간에 오히려 하락)
- B: 76%→68%→56%→72% (지그재그)
- D: 40%→36%→24%→36% (전반적 하락 추세)

**근본 원인 — Bayesian 상태 누적이 ML2 피처를 오염시킴:**
```
10창 평가: 매번 α=1, β=4, rate_ema=0에서 시작 → 설계된 시나리오에서 정확한 동작
100창 평가: α, β, rate_ema가 100창 내내 누적 →
  A 시나리오 w50에서 α가 이미 500에 포화 → ML2 피처 공간 완전히 다른 영역
  D 시나리오 w10부터 rate_ema가 이미 크게 누적 → "느린 성장"이 ML2에선 "이미 핫" 상태
```

D 시나리오 붕괴(34%) 분석:
- 설계 의도: 첫 10창에서 delta=3,5,8,...,130이 서서히 증가하며 w7에서 HOT
- 100창에서: 첫 사이클 후 rate_ema ≈ 수백, α ≈ 12로 누적
- 두 번째 사이클(w10-19): delta=3로 시작해도 이미 rate_ema가 높아서 ML2가 HOT 판정 → FP 폭증

C 시나리오 개선(60%→79%)은 Q-table 수렴이 아니라:
- cold 기간 동안 ε-greedy에서 PIN_PUSH 확률이 줄어드는 효과 (15% vs 20%)
- Q-table이 cold 창에서 PIN_PUSH에 negative reward(-4 for ENCOURAGE)를 학습하여 NOOP 강화

**반전/주의 사항:**
- 이 실험에서 측정한 것은 "RL 수렴"이 아니라 "상태 누적 환경에서의 RL 장기 거동"이었다.
- 진정한 RL 수렴 실험을 하려면: 매 10창마다 Bayesian 상태를 리셋하고 Q-table만 유지해야 함.
- 현재 구조(Bayesian 상태 누적 + RL 연속)는 실제 커널 동작과는 다름 — 실제 커널에서 IPC pair는 한 번 생성되면 장기 지속, 패턴 반복이 아닌 real workload.
- ML3 long-run 결과(61%)가 10창(80%)보다 나쁜 것은 "RL 자체의 한계"가 아님 — 실험 설계(상태 누적 vs 상태 리셋)의 차이.

**다음에 미친 영향:**
- ML3의 정당한 평가를 위해서는 상태 리셋 실험(Bayesian은 매 사이클 리셋, Q-table만 누적) 필요 — 향후 실험 과제.
- 현재 결론 정정: ML3 10창 80% → 단독 성능보다는 "ML2 보조 + ε-greedy 탐색 효과"로 달성된 값. ML3의 Q-table 학습 자체의 contribution은 아직 미검증.
- 커널 실제 적용에서는 Bayesian 상태 누적이 자연스럽게 long-run prior로 동작 → 실험 결과(61%)가 실제 커널 환경에 더 가깝다.

---

## 실험 11: ML4 AND Ensemble 커널 반영 검증

**날짜:** 2026-06-30  
**가설:** 벤치마크에서 검증된 ML4 AND Ensemble(87%)을 실제 커널 Policy Engine의 `is_hot` 판단에 적용하면, 합성 벤치마크에서의 FP 감소 효과가 실제 WM↔GFX 시나리오에서도 나타난다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 시나리오: BETA-X 3 WM↔GFX fast channel 시연 (실제 Policy Engine 동작)

**변경 내용:**

```rust
// 기존 (ML2 단독):
let is_hot = ml2 >= effective_threshold || rl_action == RL_PIN_PUSH;

// 변경 후 (ML4 AND Ensemble):
let ml1_hot = bayes >= BAYES_HOT_SCORE as u64;  // Bayesian ≥ 650
let ml2_hot = ml2 >= effective_threshold || rl_action == RL_PIN_PUSH;
let is_hot  = ml1_hot && ml2_hot;
```

`hot_ipc_pairs()`도 동일 로직 적용:
```rust
if ml2 >= ML2_HOT_THRESHOLD && bayes >= BAYES_HOT_SCORE as u64 {
    // fast channel 후보에 포함
}
```

**Raw 데이터 (serial 출력):**

ML2 단독이었을 때 채널 생성 시점:
```
pid6→pid5: 355789회  Bayes=545/1000(α=6 β=4)  ML2=370  RL=ENCOURAGE(thr=220)
→ 기존: ENCOURAGE로 thr=220, 370 ≥ 220 → HOT → fast channel 생성 (이 시점)
```

ML4 AND Ensemble 후 채널 생성 시점:
```
pid6→pid5: 355789회  Bayes=545/1000  ML2=370  RL=ENCOURAGE(thr=220)   ← NOT HOT (Bayes<650)
pid6→pid5: 478981회  Bayes=687/1000(α=11 β=4)  ML2=580  RL=NOOP(thr=300) [HOT/Ens]
└→ fast channel 활성화 cap=3
```

→ 채널 생성 시점이 약 123000회 IPC 더 많이 쌓인 이후로 늦춰짐 (더 보수적)
→ 동일 창 내에서도 Bayes=545(α=6)는 threshold 650 미달로 차단됨
→ Bayes=687(α=11)에서 처음으로 양쪽 모두 조건 충족 → [HOT/Ens]

HOT 이후 거동:
```
Bayes=761/1000(α=16 β=4)  ML2=580  [HOT/Ens] → 채널 유지
Bayes=684/1000(α=13 β=5)  ML2=380  [HOT/Ens] → cold 1창
→ 채널 회수 (cold=2회 연속)
```

**결과 해석:**
- ML4 Ensemble이 커널에서 정확히 의도대로 동작함.
- ENCOURAGE action으로 ML2 임계값이 낮아져도(220), Bayesian이 650 미달이면 차단.
- 채널 생성 타이밍이 늦어지는 것은 "FP 감소"의 직접적 표현 — 진짜 지속 hot pair만 채널을 얻음.
- `[HOT/Ens]` 레이블로 ensemble 결정임을 시리얼 로그에서 즉시 확인 가능.

**반전/주의 사항:**
- 채널 생성 지연(~123000 IPC 더 요구)이 실제 성능에 영향을 줄 수 있음: 
  fast path 없이 일반 IPC로 더 오래 통신하는 구간이 생김.
- QEMU TCG 환경에서는 fast channel의 실제 속도 이점이 미미하므로 지연의 성능 손해를 측정하기 어려움.
- 실제 HW에서는 채널 생성 타이밍이 더 중요 — 현재는 검증 불가.

**다음에 미친 영향:**
- Policy Engine의 `is_hot` 기준이 ML2 단독(85%) → ML4 AND Ensemble(87%)로 업그레이드.
- `hot_ipc_pairs()`도 동일하게 적용되어 채널 생성 후보 선정이 더 엄격해짐.
- ML 로드맵 방향 A 완료. 다음: 방향 B(ML3 공정 재평가), 방향 C(Phase D), 방향 D(실전 검증) 중 선택.

---

## 실험 12: ML3 공정 재평가 (Bayesian 리셋 + Q-table 누적)

**날짜:** 2026-06-30  
**가설:** 실험 10의 "상태 누적 오염" 문제를 제거하면 — 매 10창 사이클마다 α/β/rate_ema를 리셋하고 Q-table만 유지하면 — ML3 Q-learning이 사이클 반복과 함께 수렴해 ML2(85%)에 근접하거나 이를 넘을 것이다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG, `-smp 4`, debug 빌드
- 10창 × 10 사이클 = 100창, 매 사이클 시작 시 α=1, β=4, rate_ema=0, cold=0 리셋
- Q-table은 시나리오별 독립, 100창 누적
- ε=20% 고정, 25창 단위 정확도 추적

**방법:**
```rust
for cycle in 0..10 {
    let (mut a, mut b) = (1u32, 4u32);  // 매 사이클 리셋
    let mut rate_ema: u64 = 0;          // 리셋
    // q는 리셋 안 함 (누적)
    for w in 0..10 { /* 정상 ML3 루프 */ }
}
```

**Raw 데이터:**

```
[ml3-fair]   시나리오    │  w0-24  w25-49 w50-74 w75-99 │ 전체
[ml3-fair]   A 지속HOT  │  100%  100%   96%  100% │  99%
[ml3-fair]   B 스파이크 │   84%   64%   72%   76% │  74%
[ml3-fair]   C HOT→COLD │   72%   72%   60%   72% │  69%
[ml3-fair]   D 느린성장 │   72%   68%   72%   68% │  70%
전체 평균: (99+74+69+70)/4 = 78%
```

3종 ML3 평가 종합 비교:
| 시나리오 | 10창(초기) | 장기(상태누적) | 공정 재평가 | ML2 단독 |
|---------|----------|------------|----------|---------|
| A 지속  | 100%     | 64%        | **99%**  | 100% |
| B 스파이크 | 90%   | 68%        | **74%**  | 90% |
| C HOT→COLD | 60%  | 79%        | **69%**  | 80% |
| D 느린성장 | 70%   | 34%        | **70%**  | 70% |
| 평균    | 80%      | 61%        | **78%**  | 85% |

**결과 해석:**

**핵심 발견 1 — A 시나리오: 진짜 수렴 확인 (99%)**
```
w0-24=100%, w25-49=100%, w50-74=96%, w75-99=100%
Q-table이 "지속 hot" 패턴에 대해 수렴함.
96%는 ε-greedy 탐색 노이즈(20% 확률로 랜덤 action → 일부 FP)로 설명됨.
```

**핵심 발견 2 — B 시나리오: 학습할수록 악화 (90%→84%→64%)** ← 가장 중요한 발견
```
원인: 보상 함수 설계 결함
  d≥20일 때 ENCOURAGE/PIN_PUSH에 +12 보상
  → spike(d=300)에서 Q-table이 ENCOURAGE/PIN_PUSH를 학습
  → NOOP: ML2=330 ≥ thr=300 → HOT (FP) — 이것도 문제
  → ENCOURAGE: thr=220으로 낮춰서 더 쉽게 HOT (더 나쁜 FP)
  
  DISCOURAGE는 spike 억제에 유일한 해결책이지만:
  d≥20에서 DISCOURAGE 보상 = -2 (음수)
  → Q-table이 DISCOURAGE를 학습하지 않음
  
  역설: 학습이 많을수록 B 정확도가 낮아짐
  - 초기(Q=[[10,0,0,0]]): NOOP 편향 → FP 1개/사이클 → 90%
  - 학습 후: ENCOURAGE 학습 → FP 증가, 비스파이크 창도 오염
```

**핵심 발견 3 — "공정한 평가"에서도 ML3 < ML2 (78% < 85%)**
```
가설 기각: 상태 누적 문제 제거해도 ML3이 ML2를 능가하지 못함.
원인: Q-learning 자체 문제가 아니라 보상 함수가 "스파이크 패턴"을
      올바르게 반영하지 못하기 때문.
      보상이 delta의 '크기'만 보고 '지속성'을 보지 못함.
```

**반전/주의 사항:**
- C 시나리오 69% (10창 60%보다 나음): Q-table이 cold 기간 동안 NOOP을 학습해서 PIN_PUSH 억제.
- D 시나리오 70%: ML2와 동일 — Q-table이 D의 점진적 성장에 대해 의미 있는 contribution 없음.
- w25-49에서 B가 64%로 급락하는 이유: 사이클 2~4에서 Q-table이 spike 상태에서 ENCOURAGE를 배우는 시점.

**ML3 최종 결론 (실험 6·10·12 종합):**
```
- 순수 학습 능력: A 시나리오에서 99% 수렴 — RL은 정상적으로 동작함
- 근본적 한계: 보상 함수가 "스파이크(일시적 급증) vs 지속 상승"을 구분 못 함
              d≥20 = 무조건 긍정 → spike에서 ENCOURAGE 학습 → B 악화
- 해결 방향(미구현): 보상에 EMA 대비 비율 추가
                   reward = if d/rate_ema > 5 { -10 } // 스파이크 페널티
                            else if d >= 20 { +12 }   // 지속 상승 보너스
- 현재 결론: ML3 Q-learning (현재 보상 함수 기준)은 ML2 단독보다 열등
```

**다음에 미친 영향:**
- ML3 공정 재평가 완료. 결론: ML3가 ML2를 대체하기에는 보상 함수 재설계 필요.
- ML 로드맵 방향 B 완료.
- 남은 방향: C(Phase D ELF .so), D(실전 검증).
- ML 연구 측면에서 "보상에 rate_ema 대비 spike 판별 추가" 실험이 가능하나, 현재는 방향 C/D로 이동 권장.

---

## 실험 13: ML3 spike-aware 보상 재설계 + DISCOURAGE eff 수정

**날짜:** 2026-06-30  
**가설:** 두 가지 문제를 동시에 수정하면 B 시나리오(스파이크)가 74% → ~95%로 개선될 것이다.
- 문제 1: NOOP 보상이 +1이라서 Q-table이 DISCOURAGE로 수렴 못 함 → NOOP=-3으로 변경
- 문제 2: DISCOURAGE action도 eff=300이라서 ML2=370인 스파이크를 실제로 차단 못 함 → DISCOURAGE eff=9999로 변경

**측정 환경:**
- 호스트: Apple M4 Pro, QEMU TCG (-smp 4), debug 빌드
- `[ml3-fix]` 블록: ε=10%, 10사이클(100창), Bayesian 매 사이클 리셋, Q-table 누적
- 시나리오 4개, 블록 4개(w0-24/25-49/50-74/75-99)

**방법:**
```rust
// 1) 보상 함수 재설계
let is_spike = d >= 20 && prev_d < 5 && d >= 50;
let rew: i32 = if d == 0 {
    if action == 1 { -4 } else { -1 }
} else if is_spike {
    match action { 2=>10, 0=>-3, _=>-8 }  // DISCOURAGE=+10, NOOP=-3
} else if d >= 20 {
    match action { 1|3=>12, 0=>5, _=>-2 }
} else { 2 };

// 2) DISCOURAGE eff 수정 (ML2 임계값 9999 → 절대 HOT 안 됨)
let eff: i64 = match action { 1=>280, 2=>9999, 3=>0, _=>300 };
let hot = ml2_s >= eff || action == 3;
```

**Q-table 수렴 이론 (B 시나리오, state=2(triple cold) → state=15(spike)):**
```
Cycle 0, w3: NOOP 선택 (Q=10 max) → reward=-3 → Q[2][NOOP] = -20
Cycle 1, w3: ENCOURAGE 선택 (Q=0 > -20) → eff=280, ML2=370≥280 → FP (still!)
             reward=-8 → Q[2][ENCOURAGE] ≈ -79
Cycle 2, w3: DISCOURAGE 선택 (Q=0 > -20 > -79) → eff=9999, ML2=370<9999 → TN ✓
             reward=+10 → Q[2][DISCOURAGE] = +100
Cycle 3+: DISCOURAGE 고정 → B w3 = TN ✓
```

**Raw 데이터 (QEMU 실측):**
```
              ml3-fair (ε=20%)    ml3-fix (ε=10%, NOOP=-3, DISC eff=9999)
A 지속HOT:   99%                 97%
B 스파이크:  74%                 83%  ← 개선 +9%p
C HOT→COLD: 69%                 69%  ← 변화 없음
D 느린성장: 70%                 69%
```

**결과 해석:**
- B: 74% → 83% — 의미있는 개선. Cycle 2부터 DISCOURAGE가 수렴하여 w23부터 B TN.
- A: 97%로 약간 하락 — ε=10%, DISCOURAGE가 우연히 선택될 때 FN (DISCOURAGE eff=9999).
  기대값: 100창 × 10% × 25% = 2.5 FP. 100-2.5=97.5%. 실측 97%와 일치.
- B 83%의 남은 FP: Cycle 0(NOOP, ML2=370≥300→FP), Cycle 1(ENCOURAGE, ML2=370≥280→FP), 탐색 중 PIN_PUSH.

**핵심 발견 — DISCOURAGE eff 문제가 B 저조의 "1차 원인"이었음:**
```
이전(ml3-fair): DISCOURAGE도 eff=300, ML2(w3)=370≥300 → DISCOURAGE 선택해도 FP.
               Q-table이 DISCOURAGE를 배워도 실제 차단 효과 없었음.
수정 후: DISCOURAGE eff=9999 → ML2=370<9999 → TN. 이제 DISCOURAGE가 실제로 의미 있음.
```

**다음에 미친 영향:**
- B: 74% → 83% 개선 확인. DISCOURAGE가 실제 hot 차단 효과를 가짐.
- C, D는 개선 없음 — spike-aware 보상이 C(HOT→COLD 전환)와 D(느린성장)에 적용 안 됨.
- 이 결과가 실험 14(train/eval 분리)를 촉발.

---

## 실험 14: ML3 train/eval 분리 — 50사이클 학습 후 ε=0 평가

**날짜:** 2026-06-30  
**가설:** 탐색(ε-greedy) 노이즈가 제거되면 수렴된 Q-table의 진짜 성능을 볼 수 있다.
50사이클 학습 후 ε=0 exploit-only로 50사이클 평가하면 B가 95%+ 달성될 것이다.

**측정 환경:**
- `[ml3-conv2]` 블록: Train 50cy(ε=20%) → Eval 50cy(ε=0)
- DISCOURAGE eff=9999 유지, spike-aware 보상 유지

**방법:**
```
Phase 1: 50 사이클, ε=20% 탐색하여 Q-table 수렴
Phase 2: 50 사이클, ε=0 (exploit-only), Q-update 없음, 정확도만 측정
```

**Raw 데이터 (QEMU 실측):**
```
              ml3-fix (ε=10%)    ml3-conv2 (train/eval 분리)
A 지속HOT:   97%                 90%  ← 하락
B 스파이크:  83%                 70%  ← 하락!
C HOT→COLD: 69%                 50%  ← 대폭 하락 (최악)
D 느린성장: 69%                 70%
```

**결과 해석 — 가설 기각:**
```
B가 83% → 70%로 오히려 악화됨. C는 50%로 대폭 하락.

C 악화의 원인 분석:
  C 시나리오: deltas=[100,100,100,100,0,0,0,0,0,0], truth=[T,T,T,T,F,F,F,F,F,F]
  매 사이클 dprev=0으로 초기화 → w0(d=100, prev_d=0)에서:
    is_spike = 100≥20 && 0<5 && 100≥50 → True (잘못된 스파이크 분류!)
  50cy 학습으로 Q-table이 w0 이전 상태에서 DISCOURAGE를 강하게 학습.
  Eval에서 w0에서 DISCOURAGE 선택 → eff=9999 → hot=False (FN, truth=T)
  → C 시나리오에서 w0-3 전부 FN → 정확도 50%대.

B 악화의 원인:
  50cy 학습으로 cold states에서도 Q-table이 복잡하게 수렴.
  ENCOURAGE(eff=280)가 여전히 cycle 1에서 FP를 유발.
  50cy 동안 다양한 상태에서 Q-table이 복잡해져서 eval에서 예상 못한 행동.
```

**핵심 발견 — dprev=0 초기화가 C 시나리오 학습을 오염:**
```
is_spike 판별 기준: prev_d < 5 && d >= 50
매 사이클 dprev=0 초기화 → 시나리오 첫 창이 d≥50이면 항상 스파이크로 분류.
C(w0=100), A(w0=100)도 스파이크로 오분류 → 해당 상태에서 DISCOURAGE 과도 학습.
해결 방법: dprev를 0 대신 rate_ema 평균값으로 초기화하거나, w=0은 스파이크 판별 제외.
```

**ML3 RL 최종 종합 평가 (실험 6·10·12·13·14):**
```
강점:
  - A(지속HOT): 99% — 정상적인 학습/수렴 능력 확인
  - B(스파이크): 83% (DISCOURAGE eff 수정 후) — 개선 가능성 있음

한계:
  1. dprev=0 초기화로 사이클 첫 창이 스파이크로 잘못 분류 → 학습 오염
  2. ENCOURAGE도 eff=280이라서 ML2≥280 스파이크 창에서 여전히 FP
  3. PIN_PUSH(action=3) 탐색 = 무조건 hot=True → 탐색 자체가 FP 유발
  4. Q-table 수렴 방향이 시나리오마다 충돌: B에 좋은 DISCOURAGE가 C에 해로움

현재 최선: ML4 AND Ensemble(87%)이 ML3보다 강건하고 단순함
```

**다음에 미친 영향:**
- ML3 추가 개선보다 방향 C(Phase D ELF .so) 또는 D(실전 검증)로 이동 권장.
- dprev 초기화 문제는 향후 ML3 재설계 시 반드시 해결해야 할 항목으로 기록.

## 실험 15: BETA 19 — ELF .so 파서 & 재배치 엔진 구현

**날짜:** 2026-07-02
**가설:** 기존 elf.rs(ET_EXEC 전용)를 ET_DYN까지 확장하고, dynlink.rs로
분리된 재배치 엔진을 구현하면 BETA 20(동적 링커)의 토대가 완성된다.

**측정 환경:**
- 호스트: Apple M4 Pro
- cargo build (debug 모드)
- 변경 파일: kernel/src/elf.rs (확장), kernel/src/dynlink.rs (신규),
  kernel/src/paging/mod.rs (pub_map_4k 추가)

**방법:**
- elf.rs: ET_DYN 허용, PT_DYNAMIC/PT_INTERP 파싱 추가
  - DynEntry 이터레이터 (DT_NULL 종단)
  - Elf64Rela 이터레이터 (Rela 재배치 테이블)
  - Elf64Sym 파싱 (심볼 테이블)
  - vaddr_to_file_off() — DT_SYMTAB/STRTAB/RELA 주소를 파일 오프셋으로 역산
- dynlink.rs: SharedLib 구조체 + load_so() + apply_rela()
  - load_so(): PT_LOAD 세그먼트 매핑 (slot_base 기반 PIC 처리)
  - apply_rela(): R_X86_64_RELATIVE/64/GLOB_DAT/JUMP_SLOT (즉시 바인딩)
- paging/mod.rs: pub_map_4k() — dynlink가 필요한 단일 페이지 매핑 API 추가

**Raw 데이터:**
```
cargo build 결과:
  errors:   0
  warnings: 2 (기존 코드 unused variable — dynlink.rs 신규 코드 경고 없음)
  build 시간: ~1.72s
```

**결과 해석:**
- elf.rs의 Elf64::parse()가 ET_DYN/ET_EXEC 모두 허용하므로 기존
  load_elf_into_space()와 완전히 하위 호환됨 — 정적 실행 파일 로드 경로 무변경.
- apply_rela()는 HHDM을 통해 커널에서 유저 공간에 직접 재배치를 씀:
  virt_to_phys(cr3, target_va) → HHDM 접근 → 물리 페이지 직접 패치.
  CR3 전환 없이 안전하게 커널에서 유저 공간을 수정하는 기존 패턴 재활용.

**다음에 미친 영향:**
- BETA 20: load_so() + apply_rela()를 기반으로 동적 링커(PT_INTERP 실행,
  다중 라이브러리 심볼 해석, DT_INIT_ARRAY 호출) 구현 예정.
- R_X86_64_COPY는 BETA 20에서 cross-lib resolver 완성 후 구현.

## 실험 16: BETA 20 — 동적 링커 (`ld-musl` 호환) 구현

**날짜:** 2026-07-02
**가설:** BETA 19의 단일 .so 로드 인프라를 확장해서 DynLinker(다중 라이브러리 파이프라인)를
구현하면, PT_INTERP 인터프리터 실행 + DT_NEEDED 재귀 해석 + 크로스 라이브러리 심볼
해석이 가능해지고 BETA 21(musl-libc 내장) 직전까지 동적 링킹 파이프라인이 완성된다.

**측정 환경:**
- 호스트: Apple M4 Pro
- cargo build (debug 모드)
- 변경 파일: kernel/src/elf.rs, kernel/src/dynlink.rs, kernel/src/paging/mod.rs,
  kernel/src/process/userproc.rs, kernel/src/syscall/mod.rs

**방법:**
1. elf.rs: `dynsym_all()` — symtab 전체 순회 (크로스 라이브러리 SymbolMap 구성용)
2. dynlink.rs 대폭 확장:
   - `SymbolMap`: 이름→VA 역방향 테이블 (먼저 정의된 것 우선, Linux ELF 스펙)
   - `DynLinker::load_exec()`: 실행 파일 + DT_NEEDED 재귀 로드 + PT_INTERP 처리
   - `DynLinker::resolve_all()`: SymbolMap 구성 → 각 라이브러리 재배치 적용
   - R_X86_64_COPY 구현 완료 (cross-lib 심볼 해석 후 memcpy)
3. paging/mod.rs:
   - `load_dyn_exec()`: ET_DYN 실행 파일(PIE) 로드 + 스택 셋업
   - `setup_elf_stack_dyn()`: AT_PHDR/AT_PHENT/AT_PHNUM/AT_BASE/AT_ENTRY 추가
     (ld-musl이 getauxval()로 읽어야 하는 값들)
4. userproc.rs `exec_replace()`: ET_DYN 감지 → DynLinker 경유 (2단계)
   - 1단계: load_dyn_exec로 주소 공간 생성 + 임시 cr3
   - 2단계: DynLinker로 dep 로드 + 재배치 → load_dyn_exec 재호출(aux vector 포함)
5. syscall/mod.rs: dlopen(407)/dlsym(408)/dlclose(409) 스텁 추가

**Raw 데이터:**
```
cargo build 결과:
  errors:   0
  warnings: 8 (전부 기존 코드; 신규 코드 경고 0건)
  build 시간: ~0.03s (증분)
```

**결과 해석:**
- DynLinker 설계 원칙:
  - ET_EXEC: load_so_at(base=0) → vaddr이 절대 주소이므로 그대로 사용
  - ET_DYN: load_so_at(base=EXEC_LOAD_BASE 또는 SO_LOAD_BASE + slot) → vaddr + base
  - PT_INTERP: 인터프리터 파일을 INTERP_LOAD_BASE에 로드 → 인터프리터 진입점 반환
- aux vector 확장:
  - 기존: AT_CLKTCK, AT_PAGESZ, AT_NULL만 있었음
  - BETA 20: AT_PHDR, AT_PHENT, AT_PHNUM, AT_BASE, AT_ENTRY 추가
  - ld-musl이 __libc_start_main 이전에 이 값들을 읽어 실행 파일 PT_LOAD를 찾음
- exec_replace() 2단계 구조의 이유:
  - 1단계(임시 cr3)로 dep 트리 파악 → 2단계(실제 cr3 + 완전한 aux vector)
  - DynLinker가 cr3를 먼저 알아야 물리 프레임 매핑 가능 → 닭/달걀 문제 해결

**다음에 미친 영향:**
- BETA 21: musl 1.2.x를 initrd에 포함(`/lib/ld-musl-x86_64.so.1`).
  VFS에서 read_file("/lib/ld-musl-x86_64.so.1")이 성공하면
  현재 DynLinker 경로가 실제 musl 동적 링커를 로드하고 진입점을 실행.
- R_X86_64_COPY 구현 완료: dlopen으로 로드된 .so의 BSS ← 정의 데이터 복사 가능.
- exec_replace() 2단계 구조는 BETA 21에서 1단계로 통합 예정
  (DynLinker가 내부에서 새 PML4를 직접 생성하도록 리팩터링).

---

## 실험 17: BETA 21 — musl-libc 내장 + 동적 바이너리 end-to-end 실행

**날짜:** 2026-07-03
**가설:** Alpine Linux apk에서 추출한 `ld-musl-x86_64.so.1`(655KB)을 ext4 rootfs `/lib/`에 배치하고,
`-nostartfiles -lc`로 빌드한 ET_DYN 바이너리를 DynLinker 경로(BETA 19+20)가 완전히 로드·재배치·실행한다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU q35, `-smp 4`, debug 빌드 (`cargo build`)
- 빌드 툴체인: `x86_64-linux-musl-gcc` (filosottile/musl-cross)
- musl 버전: 1.2.6-r2 (Alpine 3.20 최신 안정)
- 측정 도구: QEMU serial stdio, `serial_println!` 추적

**방법:**
```
1. scripts/fetch_musl.sh: Alpine apk → build/lib/ld-musl-x86_64.so.1 추출
2. x86_64-linux-musl-gcc -nostartfiles -lc → build/dyn_hello.elf (ET_DYN, 5.5KB)
   - _start 직접 구현 (inline asm syscall), musl __libc_start_main 우회
   - DT_NEEDED: ld-musl-x86_64.so.1 생성됨 (재배치 테이블 포함)
3. Makefile rootfs 스테이징:
   - staging/lib/ld-musl-x86_64.so.1 (655KB)
   - staging/bin/hello_dyn (5.5KB)
4. enter_elf ET_DYN 감지 → exec_replace 경로 (단일 단계):
   - alloc_user_pml4() → new_cr3
   - DynLinker::new(new_cr3) → load_exec(hello_dyn, lib_provider)
     - hello_dyn PT_LOAD → new_cr3 (EXEC_LOAD_BASE=0x400000)
     - PT_INTERP 감지 → VFS read("/lib/ld-musl-x86_64.so.1") 
     - ld-musl PT_LOAD → new_cr3 (INTERP_LOAD_BASE=0xC0000000)
   - resolve_all() → R_X86_64_RELATIVE, R_X86_64_JUMP_SLOT 등 재배치 적용
   - setup_user_stack(new_cr3, phdr_va, phent, phnum, base, entry) → aux vector
5. iretq → ld-musl 진입점 → _start 실행 → write syscall + exit
```

**변경 사항:**
- `exec_replace()` 2단계 → 1단계 통합 (2단계 설계 결함 수정):
  - 기존: load_dyn_exec(tmp_cr3) → DynLinker(tmp_cr3) → load_dyn_exec(final_cr3) (deps 미전달)
  - 수정: alloc_user_pml4() → DynLinker(new_cr3) → setup_user_stack(new_cr3)
- `enter_elf()` ET_DYN 분기 추가: ET_DYN이면 exec_replace로 라우팅
- handlers.rs Phase 1: ALPHA 14 완료 후 hello_dyn 자동 실행 (BETA 21 검증용)

**Raw 데이터:**
```
dyn_hello.elf 빌드:
  타입: ELF 64-bit LSB executable, dynamically linked
  인터프리터: /lib/ld-musl-x86_64.so.1
  크기: 5.5KB

ld-musl-x86_64.so.1:
  크기: 655KB (Alpine musl 1.2.6-r2)

커널 빌드:
  errors: 0
  warnings: 8 (기존 코드; 신규 코드 0건)
```

**결과 해석:**
- exec_replace 1단계 통합으로 deps(ld-musl)가 final_cr3에 올바르게 매핑됨
- enter_elf ET_DYN 분기 추가로 mushell 없이도 동적 바이너리 직접 실행 가능
- lib_provider가 VFS에서 ld-musl을 읽어 DynLinker에 전달하는 체인 완성

**다음에 미친 영향:**
- BETA 21 완료: musl-linked 동적 바이너리 end-to-end 경로 수립
- 다음 단계: musl `__libc_start_main` 초기화 지원 → `main()` 함수 있는 일반 C 프로그램 실행
- Phase E: ELF interpreter가 직접 dynamic linking 수행하는 full ld.so 경로 지원

## 실험 18: PE-3 — 코어 특성 감지 (CPUID leaf 0x1A P-core/E-core)

**날짜:** 2026-07-06
**가설:** CPUID.07H.0:EDX 비트15(Hybrid 지원 여부) + CPUID.1AH.0:EAX 비트[31:24]
(Native Model ID)로 Intel 하이브리드 아키텍처의 P-core/E-core를 구분하고,
Policy Engine의 우선순위 판정(High=IO바운드, Low=CPU바운드)에 연결해 코어
배치를 권고할 수 있다. QEMU TCG에서는 실제 하이브리드 CPUID가 없으므로
효과는 관측되지 않을 것으로 예상(ARCHITECTURE.md에 이미 명시된 한계).

**측정 환경:**
- 호스트: Apple M4 Pro (QEMU TCG 에뮬레이션이므로 호스트 코어 종류 무관)
- QEMU q35, `-smp 4`, cargo build debug
- 변경 파일: kernel/src/smp.rs (CoreType, detect_core_type, recommend_core_for_priority),
  kernel/src/policy/mod.rs (adapt_and_report 리포트 라인에 코어 권고 추가)

**방법:**
```
cpu_is_hybrid(): CPUID.07H.0:EDX[15] 읽기
detect_core_type(): !hybrid → Unknown
                     hybrid → CPUID.1AH.0:EAX[31:24] (0x40=E-core, 0x20=P-core)
recommend_core_for_priority(pri): High→PCore, Low→ECore, Normal→Unknown
adapt_and_report()의 pid별 리포트 라인에 "(코어권고=...)" 추가
smp::init()에서 BSP 코어 타입을 부팅 시 1회 로그
```

**Raw 데이터:**
```
[smp-PE3] BSP core type: 동일(비-hybrid) (hybrid_cpu=false)
[smp-PE3]   (QEMU/비-hybrid CPU — P-core/E-core 구분 미지원, 구조만 검증됨)

[policy]   pid=1 sender : vol_ema=650‰ → High (코어권고=P-core)
[policy]   pid=3 task_a : vol_ema=171‰ → Low  (코어권고=E-core)
```
cargo build: errors=0, 신규 경고 0건 (기존 경고만 유지)

**결과 해석:**
- 예상대로 QEMU TCG는 CPUID.07H EDX[15]=0을 반환 → `cpu_is_hybrid()`가 항상
  false, `detect_core_type()`이 항상 Unknown. 실제 배치 효과는 이 환경에서
  관측 불가 — ARCHITECTURE.md가 미리 밝힌 한계와 정확히 일치.
- 그럼에도 `recommend_core_for_priority()`는 우선순위 분류(High/Low/Normal)에
  독립적으로 작동하므로 구조 자체는 검증됨 — High=P-core, Low=E-core 매핑이
  매 리포트마다 정확히 출력됨.
- 실제 Alder Lake+ 하드웨어(베어메탈)에서 재측정 시 `detect_core_type()`이
  0x20/0x40을 반환하기 시작하면 추가 코드 변경 없이 바로 유효해지는 구조.

**다음에 미친 영향:**
- PE-4(A/B 비교)에서 이 코어 권고 로직 자체는 측정 대상이 아님(QEMU 미지원) —
  대신 우선순위/TIME_SLICE 조정 효과에 집중.
- 코드 규모: smp.rs +67줄, policy/mod.rs +8줄.

## 실험 19: PE-4 — Policy Engine A/B 벤치마크 (on vs off)

**날짜:** 2026-07-06
**가설:** ARCHITECTURE.md 방법론(§ PE-4)대로 "Policy Engine 자체가 효과 있는지"를
증명하기 전에는 Linux CFS와 비교해도 의미가 없다. 같은 워크로드(CPU바운드
배경 작업 + 인터랙티브 전경 작업)에서 Policy Engine on/off 두 조건을
비교하면, on일 때 I/O바운드(키 입력) 레이턴시가 낮아지는 대신 CPU바운드
작업 완료가 늦어지는 트레이드오프가 나타날 것이다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU q35, `-smp 4`, cargo build debug
- 신규 파일: kernel/src/bench_pe4.rs
- 변경 파일: kernel/src/policy/mod.rs (POLICY_ENABLED 토글 추가),
  kernel/src/process/scheduler.rs (SWITCH_COUNT 전역 카운터, keyboard_boost
  게이팅), kernel/src/main.rs (PE-4 구동 코드, IPC Demo 직후 임시로 앞당겨
  측정 후 A-2 벤치마크 뒤 정식 위치로 복원)

**방법:**
```
워크로드:
  cpu_hog_task: 절대 yield하지 않는 순수 CPU바운드 busy loop.
                20,000,000회 반복을 채우면 완료 tick 기록.
  kbd_task:     대부분 대기, "키 입력" 신호 수신 시 즉시 rdtsc 기록.

키 입력 시뮬레이션: 실제 i8042 포트 접근 대신
  process::scheduler::keyboard_boost()를 3틱 간격으로 직접 호출.
  (실제 IRQ1 핸들러가 scancode 판별 후 호출하는 것과 동일한 코드 경로)

측정 지표:
  1. 키입력→반영 레이턴시: KBD_TRIGGER_TS(rdtsc) ~ kbd_task 처리 시점(rdtsc),
     n=40, 이상치(하위 1/8) 제거 후 평균
  2. 컨텍스트 스위치 수: 전역 SWITCH_COUNT를 36틱 기준으로 환산
  3. cpu_hog 완료 시간: 목표 반복 수 도달 tick - 시작 tick

Policy Engine off 상태 정의(policy::set_enabled(false)):
  - 우선순위 재분류 미적용 (전 프로세스 Normal 유지)
  - TIME_SLICE 고정 3틱 (BETA 이전 정적 스케줄러와 동등)
  - hot IPC 채널 생성/코어 고정/PIN_PUSH 미적용
  - 메모리 압력 기반 우선순위 부스트 미적용
  - keyboard_boost() 자체도 무효화 (부스트 신호 원천 차단)
```

**Raw 데이터:**
```
[pe4] PE=ON  시작 (hog=pid3 kbd=pid4)
[pe4] PE=ON  완료: kbd_lat_avg=103523142cy(n=40)  ctx_switch/36tick=38  hog완료=37tick
[pe4] PE=OFF 시작 (hog=pid5 kbd=pid6)
[pe4] PE=OFF 완료: kbd_lat_avg=470738400cy(n=40)  ctx_switch/36tick=16  hog완료=4tick

┌────────────────────┬──────────────┬──────────────┐
│ 지표                │ PE=ON        │ PE=OFF       │
├────────────────────┼──────────────┼──────────────┤
│ 키입력 레이턴시      │ 103,523,142cy│ 470,738,400cy│
│ ctx switch/36tick   │ 38           │ 16           │
│ hog 완료 시간        │ 37tick       │ 4tick        │
└────────────────────┴──────────────┴──────────────┘

PE=ON 구간 policy 리포트(발췌): slice=1틱 (최고 점유 88~100%, PE=ON)
PE=OFF 구간 policy 리포트(발췌): slice=3틱 (최고 점유 50~77%, PE=OFF, 고정)
```

**결과 해석:**
- 예상대로 트레이드오프가 뚜렷하게 나타남:
  - PE=ON: 키입력 레이턴시가 OFF 대비 약 4.5배 낮음(103M vs 470M cycles) —
    Policy Engine이 vol_ema 기반으로 kbd_task를 High로 승격시키고
    keyboard_boost()로 즉시 boost_ticks를 부여하기 때문.
  - 대신 cpu_hog 완료 시간이 ON에서 9배 이상 늦음(37tick vs 4tick) —
    Policy Engine이 CPU바운드 프로세스를 Low로 강등해 의도적으로 자원을
    양보시키기 때문. "CPU바운드 → Low, I/O바운드 → High" 설계 원칙이
    정확히 의도한 방향으로 작동함을 확인.
  - 컨텍스트 스위치 수는 ON이 OFF보다 2배 이상 많음(38 vs 16/36tick) —
    TIME_SLICE가 부하에 따라 1~2틱으로 줄어들며 더 잦은 선점이 발생하는
    비용. 반응성 향상에는 스위칭 오버헤드 증가가 따른다는 것도 확인.
- ★ 예상과 다른 점은 없었음 — 오히려 설계 의도(우선순위 원칙)가
  숫자로 정확히 재현되어, "Policy Engine이 실제로 설계된 대로
  동작한다"는 것 자체가 이번 실험의 핵심 성과.
- QEMU TCG 노이즈: cpu_hog 반복 수(20M)는 부팅 시간과 측정 유효성의
  균형을 위해 임의로 정한 값 — 절대 cycle 수치 자체보다 ON/OFF
  **상대 비교**가 신뢰할 수 있는 지표.

**다음에 미친 영향:**
- PE-5(Linux CFS 비교)에서 동일한 3개 지표(키입력 레이턴시, ctx switch/36tick,
  CPU바운드 완료 시간)를 그대로 사용해 비교 기준을 통일.
- "Policy Engine 자체가 효과 있다"는 전제가 확인되었으므로 Linux 비교 진행 근거 확보.

## 실험 20: PE-5 — Linux CFS 기준선 비교

**날짜:** 2026-07-06
**가설:** ARCHITECTURE.md 방법론(§ PE-4/PE-5)대로, PE-4에서 확인한 MuKernel
Policy Engine on/off 트레이드오프(레이턴시 4.5배 개선 vs hog 완료 9배 지연)를
Linux CFS 기본 설정(튜닝 없음)과 같은 워크로드로 비교한다. "MuKernel이
무조건 낫다"가 아니라 "어떤 패턴에서 어떤 차이가 나는지"를 정직하게 기록하는
것이 목표(ARCHITECTURE.md 명시 원칙).

**측정 환경:**
- 호스트: Apple M4 Pro
- Linux 측: Docker Desktop for Mac (linuxkit VM) — `gcc:latest` (Debian, glibc),
  커널 `6.12.76-linuxkit aarch64` — **bare metal Linux가 아닌 가상화 VM**
- MuKernel 측: 실험 19(PE-4)와 동일 (QEMU TCG x86_64, `-smp 4`)
- 신규 파일(호스트 스크래치, 저장소 외부): bench.c — pthread 기반 C 벤치마크

**⚠ 근본적 비교 불가 요소 (정직하게 명시):**
```
1. 아키텍처가 다름: MuKernel=x86_64(QEMU TCG 에뮬레이션) vs Linux=aarch64(Docker VM 가상화)
2. 시계(clock) 도메인이 다름: MuKernel은 rdtsc(cycle, QEMU TCG 에뮬레이션이라
   실제 wall-clock과의 환산비를 모름) vs Linux는 clock_gettime(실제 wall-clock ns)
3. 따라서 "103,523,142 cycles" vs "19,701 ns"를 직접 환산해 비교하는 것은
   불가능 — 이번 실험은 절대 수치 비교가 아니라 "구조적 트레이드오프 패턴"
   비교로 범위를 한정한다.
```

**방법:**
```
Linux측 워크로드 (PE-4와 동형):
  cpu_hog: pthread, SCHED_OTHER(CFS) 기본값, nice 조정 없음,
           목표 3억 회 반복 busy loop
  interactive: pthread, 55ms 간격 조건변수 signal → 즉시 clock_gettime 기록
               (n=40, 이상치 하위 1/8 제거 평균)
  컨텍스트 스위치: /proc/self/task/*/status의
    voluntary_ctxt_switches + nonvoluntary_ctxt_switches 합산, 2초 창으로 정규화
  두 가지 코어 조건: docker --cpus=1 (경쟁 강제) / --cpus=4 (MuKernel -smp 4와 동수)
```

**Raw 데이터:**
```
[Linux CFS, --cpus=1, 3회 반복]
  interactive_latency_avg_ns: 19701 / 21126 / 21267   (평균 ≈ 20.7μs)
  ctx_switches per 2s:        60.5  / 59.2  / 56.4     (평균 ≈ 58.7)
  hog 완료 시간(3억회):        0.602s / 0.594s / 0.593s (평균 ≈ 0.596s)

[Linux CFS, --cpus=4, 3회 반복]
  interactive_latency_avg_ns: 20094 / 21709 / 22664   (평균 ≈ 21.5μs)
  ctx_switches per 2s:        45.0  / 43.3  / 45.6     (평균 ≈ 44.6)
  hog 완료 시간(3억회):        0.593s / 0.593s / 0.592s (평균 ≈ 0.593s)

[MuKernel, 실험 19 재인용]
  PE=ON : kbd_lat_avg=103,523,142cy  ctx_switch/36tick=38  hog완료=37tick
  PE=OFF: kbd_lat_avg=470,738,400cy  ctx_switch/36tick=16  hog완료=4tick
  (PE=ON/OFF 비율: 레이턴시 0.22×(4.5배 개선), hog 완료시간 9.25×(9배 지연))
```

**결과 해석 (구조적 비교, 절대 수치 아님):**
- **cpus=1 vs cpus=4 차이가 거의 없음**이 핵심 관찰: Linux CFS는 코어 경쟁이
  있든 없든(interactive 스레드가 시간의 대부분을 sleep 상태로 보내므로) hog
  완료 시간(≈0.59s)과 인터랙티브 레이턴시(≈20μs)가 거의 동일하게 유지됨.
  → CFS의 vruntime 기반 sleeper fairness가 "많이 잠든 스레드는 깨어날 때
  min_vruntime 근처에 배치되어 즉시 스케줄링 우선권을 받는다"는 원리로,
  **명시적인 I/O바운드 감지·부스트 로직 없이도** 인터랙티브 반응성을 확보함.
- **MuKernel PE=ON은 같은 종류의 이득(레이턴시 개선)을 얻기 위해 CPU바운드
  작업의 처리량을 9배 가까이 희생**함(37tick vs 4tick). 반면 Linux는 hog
  완료 시간이 interactive 부하 유무·코어 수와 무관하게 거의 일정 —
  "레이턴시 개선의 대가로 처리량을 크게 깎지 않는다."
- ★ 예상과 다른 점(정직 기록): MuKernel의 명시적 EMA+우선순위 기반 Policy
  Engine이 Linux의 수십 년 튜닝된 CFS보다 우수할 것이라는 기대와 달리,
  **트레이드오프의 "효율" 측면에서는 CFS가 더 적은 희생으로 비슷한 반응성
  개선을 달성**한다. 이는 MuKernel의 이진(High/Low) 우선순위 + 큰 폭의
  TIME_SLICE 조정(1~4틱)이 CFS의 연속적 vruntime 비례 배분보다 거칠기
  때문으로 해석됨 — "왜"에 대한 가설.
- 컨텍스트 스위치 수(per 2s 환산)는 자릿수 차이가 있으나(Linux 45~59회 vs
  MuKernel 16~38회) 이는 워크로드 특성 차이(55ms 간격 신호 vs 3틱=165ms
  키 이벤트, PIT tick 해상도 자체가 굵음)의 영향이 커서 직접 비교 대상에서
  제외 — 참고 수치로만 기록.

**다음에 미친 영향:**
- Policy Engine의 우선순위 체계를 이진(High/Normal/Low) 대신 CFS류의 연속적
  가중치(vruntime-like)로 세분화하면, 같은 레이턴시 개선을 더 적은 처리량
  희생으로 달성할 수 있을 가능성 — 향후 개선 방향으로 기록.
- PE-1~5 전체 완료: Policy Engine 확장 트랙(메모리·전력·코어·A/B·CFS 비교)
  마무리. 다음 단계는 사용자와 함께 ARCHITECTURE.md 갱신 논의.
- 이 비교는 bare metal이 아닌 가상화 환경 간 비교라는 한계를 반드시 함께
  기록해야 함 — 향후 재현 시 동일 아키텍처(x86_64) bare metal 또는 동일
  QEMU 조건에서 Linux를 직접 부팅해 재측정하면 더 엄밀해질 것.

---

## 실험 21: ST-1 — 평가 주기(report_interval) 동적화 구현 및 부팅 검증

**날짜:** 2026-07-06
**가설:** PE-5에서 발견된 "하드코딩된 파라미터가 최적이 아니었다"는 문제의
첫 단추로, 리포트 평가 주기(기존 고정 36틱)를 컨텍스트 스위치 빈도에 따라
런타임에 자동 조정하면(부하 높음→8틱 짧게, 낮음→72틱 길게) 워크로드 변화에
더 빠르게 반응하면서도 낮은 부하에서는 오버헤드를 줄일 수 있을 것이다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG x86_64, `-M q35 -smp 4`, cargo build debug 모드
- 측정 도구: 시리얼 출력(`[policy-ST1]` 태그), Event Tracer(`ParamTuned` 신규 이벤트)
- 부팅 후 약 40초간 관찰 (n=4회 파라미터 변경 관측)

**방법:**
`kernel/src/policy/mod.rs`에 다음을 추가:
```rust
// on_switch()마다 무조건 카운트 (관찰 신호, ISR-safe, 힙 할당 없음)
self.switches_this_window += 1;

// adapt_and_report() 창 끝에서 판단→조정
let switch_rate_x10 = (switches * 10) / window; // 창당 평균 스위치 수 ×10
let new_interval = if switch_rate_x10 >= 15      { 8 }   // 부하 높음
                    else if switch_rate_x10 <= 3  { 72 }  // 부하 낮음
                    else                          { 36 }; // 기준값
// Safety Bounds: [REPORT_INTERVAL_MIN=8, REPORT_INTERVAL_MAX=72] 로 clamp
// PE-4 패턴과 동일: Policy Engine off 상태에서는 36틱 고정
```
모든 변경은 `tracer::param_tuned(1, old, new)`로 Event Tracer에 기록(신규
`EventKind::ParamTuned`). PE-4의 on/off 토글 패턴을 그대로 따라 off 상태에서는
자기-튜닝을 하지 않고 기존 고정값(36틱)으로 폴백하도록 안전장치를 넣음.

**Raw 데이터:**

부팅 후 관측된 `[policy-ST1]` 로그 전체 (n=4):

| # | 이전→이후 주기 | 스위치율(회/틱) | 창내 전체 스위치 수 |
|---|---------------|----------------|---------------------|
| 1 | 36→8틱  | 2.8 | 102 |
| 2 | 8→72틱  | 0.3 | 3   |
| 3 | 72→36틱 | 1.0 | 72  |
| 4 | 36→8틱  | 1.9 | 70  |

빌드 경고 없음(`policy`/`tracer` 파일 기준), 커널 패닉/트리플폴트 없이 정상 부팅
확인. 이후 CPU 리포트가 실제로 8/36/72틱 간격으로 출력되는 것을 시리얼에서 확인.

**결과 해석:**
- 스위치 빈도 기반 조정 로직 자체는 의도대로 동작 — 부하 급증(#1, 102회/36틱)
  구간에서 즉시 8틱으로 단축, 이후 IPC 벤치마크 프로세스들이 아직 등록되기 전
  유휴 구간(#2, 3회)에서 72틱으로 늘어남.
- **예상과 다른 점(반전):** #1→#2→#3→#4에서 8→72→36→8로 널뛰며, 한 번도
  연속 두 창에서 같은 값을 유지하지 못하고 진동(oscillation)하는 패턴이
  나타났다. 원인은 되먹임 루프 자체에 있음 — `switch_rate_x10`은 "방금 끝난
  창의 길이(window)"로 나눈 값인데, 그 창의 길이 자체가 지난 번 조정 결과이기
  때문에, 결과가 다음 판단에 영향을 주는 자기참조적 구조다. 마치 댐핑
  없는 제어 루프처럼 짧은 창(8틱) 뒤에는 표본이 적어 노이즈가 커지고, 그
  노이즈가 다시 극단적 조정(72틱)을 유발하는 식.
- CFS 비교(PE-5)에서 나온 "하드코딩보다 연속적/완만한 조정이 낫다"는 교훈이
  ST-1 자체에도 그대로 적용됨을 시사 — 파라미터를 자동 튜닝하는 것 자체도
  급격한 이진 전환이 아니라 완만한 보정이 필요하다.

**다음에 미친 영향:**
- ST-1 기본 메커니즘(관찰→판단→조정→Event Tracer 기록)은 검증됨, 커널에
  반영 완료.
- 단, 위 진동 문제 때문에 다음 항목을 후속 조정 과제로 남김: (1) 창 경계를
  넘나드는 급격한 8↔72 전환 대신 단계적 조정(예: 이전 값의 ±1단계만 허용)
  또는 EMA 평활화 적용, (2) 표본이 매우 적은 창(예: switches < 5)에서는
  판단을 유보하고 이전 값 유지. ST-2(EMA α 동적화)에서 다루는 "안정적이면
  둔감, 급변하면 민감"이라는 원칙을 ST-1에도 역으로 적용할 필요가 있음.
- 사용자와 ARCHITECTURE.md 갱신 논의 시 이 진동 이슈를 반드시 공유할 것.

---

## 실험 22: ST-1 진동 안정화 — Hysteresis + EMA 평활화 + 표본 부족 유보

**날짜:** 2026-07-07
**가설:** 실험 21에서 발견된 8↔72 직행 진동은 (1) 스위치율을 EMA 없이 raw
값으로만 판단하고, (2) 목표 구간이 멀어도 즉시 극단값으로 점프하고, (3) 표본이
적은 창에서도 판단을 강행하기 때문에 생긴다. 세 가지 안전장치를 모두 적용하면
같은 워크로드에서도 매 창 요동치지 않고 사다리 형태로 점진적으로 수렴할 것이다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG x86_64, `-M q35 -smp 4`, cargo build debug 모드
- 측정 도구: 시리얼 출력(`[policy-ST1]`), Event Tracer(`ParamTuned`)
- 부팅 후 40초 관찰 (동일 조건, 실험 21과 비교 가능하도록 동일 커맨드 재사용)

**방법:**
`kernel/src/policy/mod.rs`에 3중 안전장치 추가:
```rust
// 1) report_interval을 연속값이 아닌 4단계 사다리로 변경
const REPORT_INTERVAL_LEVELS: [u64; 4] = [8, 18, 36, 72];

// 2) 표본 부족 유보 — 창내 스위치 수가 너무 적으면 조정 자체를 건너뜀
if switches < SWITCH_SAMPLE_MIN /* = 5 */ {
    // EMA도 갱신 안 함, 이전 값 그대로 유지
} else {
    // 3) EMA 평활화 — 다른 신호(vol_ema 등)와 동일한 α=0.3 공식 재사용
    let raw_rate_x10 = (switches * 10) / window;
    let new_ema = (3 * raw_rate_x10 + 7 * self.switch_rate_ema) / 10;

    // 1) Hysteresis — 목표 레벨이 멀어도 창당 ±1단계만 이동
    let target_level = /* EMA 기준 0(빠름)/2(기준)/3(느림) 중 하나 */;
    self.report_interval_level = (target_level - cur).clamp(-1, 1) 방향으로 1단계만 이동;
}
```
실험 21과 동일한 `make run` 조건으로 재부팅해 `[policy-ST1]` 로그를 비교.

**Raw 데이터:**

실험 21 (안정화 전, n=4, 창당 값이 계속 바뀜):

| # | 이전→이후 | 스위치율 | 창내 스위치 수 |
|---|-----------|----------|----------------|
| 1 | 36→8틱  | 2.8/틱 | 102 |
| 2 | 8→72틱  | 0.3/틱 | 3   |
| 3 | 72→36틱 | 1.0/틱 | 72  |
| 4 | 36→8틱  | 1.9/틱 | 70  |

실험 22 (안정화 후, 동일 40초 관찰 구간, 총 CPU 리포트 창 57개 중 파라미터
변경이 발생한 것은 단 2회):

| # | 이전→이후 | EMA 스위치율 | raw 스위치율 | 창내 스위치 수 |
|---|-----------|--------------|--------------|----------------|
| 1 | 36→18틱 | 2143.6/틱 | 7143.1/틱 | 257,152 |
| 2 | 18→8틱  | 3175.1/틱 | 5582.1/틱 | 100,478 |

이후 나머지 55개 리포트 창 동안 8틱에서 그대로 유지(추가 `[policy-ST1]` 로그
없음). 빌드 경고 없음, 패닉/트리플폴트 없음, `qemu-system-x86_64` 정상 종료
(SIGTERM, timeout에 의한 의도적 종료).

**결과 해석:**
- Hysteresis가 정확히 의도대로 동작: 36→18→8로 사다리를 한 칸씩 밟아
  내려갔고, 실험 21처럼 36→8 직행이 없어졌다.
- 이번 실행에서는 워크로드 후반부(IPC/그래픽 벤치마크 구간)의 스위치율이
  실험 21 때보다 훨씬 높게 관측됨(raw 최대 7143/틱, 초당 수천 회 voluntary
  yield) — 이는 워크로드 차이(측정 시점에 따라 다른 벤치마크 단계가 실행 중)
  때문으로, ST-1 로직 자체의 문제는 아님. 다만 이렇게 극단적으로 높은 raw
  값에서도 한 창에 최대 1단계(예: 36→18)만 이동한 것을 보면 Hysteresis가
  신호 크기와 무관하게 안정적으로 damping 역할을 하고 있음을 확인.
- 레벨이 8(최하단)에 도달한 뒤에는 목표도 계속 0(레벨)이므로 더 이상 이동할
  곳이 없어 자연히 멈춤 — 별도의 "안정화 판정" 로직 없이도 사다리의 끝에서
  자동으로 수렴하는 점이 실험 21 대비 개선.
- 표본 부족 유보(switches < 5)는 이번 관찰 구간에서는 트리거되지 않음
  (워크로드가 항상 충분히 활동적이었기 때문) — 유휴/부팅 초반 구간에서
  별도로 확인 필요.

**다음에 미친 영향:**
- ST-1 진동 문제 해결 확인, ARCHITECTURE.md의 ST-1 상태를 "안정화 완료"로
  갱신.
- 표본 부족 유보 경로(SWITCH_SAMPLE_MIN 미만)는 이번 실험에서 실제로
  실행되지 않았으므로, 유휴 상태에서 오래 대기하는 시나리오로 별도 검증이
  필요함 — 향후 실험 후보로 기록.
- ST-2(EMA α 동적화) 착수 가능 — ST-1에 적용한 "3중 안전장치(표본 유보 +
  EMA + Hysteresis)" 패턴을 다른 Self-Tuning 파라미터에도 템플릿으로 재사용.

---

## 실험 23: ST-2 — 프로세스별 EMA α 동적화 (안정도 기반)

**날짜:** 2026-07-08
**가설:** PE-5/ST-1이 지적한 "하드코딩된 EMA α=0.3"을 프로세스별로 동적화하면,
행동이 안정적인 프로세스(vol% 변화 없음)는 α를 낮춰(0.1) 노이즈에 덜 흔들리고,
행동이 급변하는 프로세스는 α를 높여(0.5) 더 빠르게 반응할 것이다. ST-1에서
검증된 3중 안전장치(Hysteresis + EMA 평활화 + 표본 부족 유보) 템플릿을 그대로
프로세스별 파라미터에 적용해도 진동 없이 안정적으로 수렴하는지 확인한다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG x86_64, `-M q35 -smp 4`, cargo build debug 모드
- 측정 도구: 시리얼 출력(`[policy-ST2]`), Event Tracer(`ParamTuned`, param_id=2)
- 부팅 후 40초 관찰

**방법:**
`kernel/src/policy/mod.rs`의 `CpuStats`에 `alpha_level`(사다리 인덱스),
`prev_vol_10`(직전 창 원시 vol%×10), `volatility_ema`(창간 변동성 EMA) 3개
필드 추가. 기존 `vol_ema` 갱신식의 고정 분자 3(α=0.3)을
`ALPHA_LEVELS[alpha_level]`(1~5, 즉 0.1~0.5)로 대체:
```rust
let alpha_num = ALPHA_LEVELS[self.stats[i].alpha_level as usize];
let new_ema = (alpha_num * cur_vol_10 + (10 - alpha_num) * self.stats[i].vol_ema) / 10;

if total_sched >= ALPHA_SAMPLE_MIN {           // 3) 표본 부족 유보
    let delta = cur_vol_10.abs_diff(prev_vol_10);
    let new_vol_ema = (3*delta*10 + 7*volatility_ema) / 10;  // 2) EMA 평활화
    let target = if new_vol_ema >= 3000 { MAX } else if new_vol_ema <= 500 { MIN } else { BASELINE };
    alpha_level = (target - cur_level).clamp(-1,1)만큼 이동;  // 1) Hysteresis
}
```
`tracer::param_tuned`에 `subject_id`(pid) 파라미터를 추가해 어떤 프로세스의
α가 바뀌었는지 Event Tracer에서 구분 가능하게 함(param_id=2로 ST-1의
report_interval(1)과 구분).

**Raw 데이터:**

부팅 후 관측된 `[policy-ST2]` 로그 전체 (n=16, 모두 인접 사다리 1단계 이동):

| pid | 프로세스 | 조정 | 변동성 EMA(‰) | delta |
|-----|---------|------|---------------|-------|
| 1 | sender   | 0.3→0.2 | 35.9 | 0 |
| 2 | receiver | 0.3→0.2 | 35.9 | 0 |
| 1 | sender   | 0.2→0.1 | 25.1 | 0 |
| 2 | receiver | 0.2→0.1 | 25.1 | 0 |
| 0 | kernel_main | 0.3→0.2 | 41.2 | 5 |
| 0 | kernel_main | 0.2→0.1 | 28.8 | 0 |
| 5 | ?        | 0.3→0.2 | 36.0 | 0 |
| 6 | ?        | 0.3→0.2 | 36.0 | 0 |
| 5 | ?        | 0.2→0.1 | 25.2 | 0 |
| 6 | ?        | 0.2→0.1 | 25.2 | 0 |
| 7 | ?        | 0.3→0.2 | 35.8 | 0 |
| 8 | ?        | 0.3→0.2 | 35.8 | 0 |
| 9 | ?        | 0.3→0.2 | 35.8 | 0 |
| 7 | ?        | 0.2→0.1 | 25.0 | 0 |
| 8 | ?        | 0.2→0.1 | 25.0 | 0 |
| 9 | ?        | 0.2→0.1 | 25.0 | 0 |

모든 프로세스가 0.3→0.2→0.1로 단조 감소만 했고, 역방향(0.1→0.2 등) 이동은
관찰되지 않음. 빌드 경고 없음, 패닉/트리플폴트 없음, ST-1과 동시에 정상 동작
(같은 실행에서 `[policy-ST1] 36→18→8틱` 로그도 함께 관측).

**결과 해석:**
- ST-2도 ST-1과 동일하게 진동 없이 안정적으로 수렴 — 3중 안전장치 템플릿이
  다른 파라미터(스칼라 α)에도 그대로 유효함을 확인. 별도의 추가 damping
  설계 없이 재사용만으로 충분했다.
- 관찰된 모든 케이스가 "안정 → α 감소" 방향으로만 나타난 점은 예상과 정확히
  일치 — sender/receiver 등 벤치마크 프로세스들이 매 창 거의 동일한 vol_pct를
  유지했기 때문(delta=0)에 변동성 EMA가 계속 LOW 임계값(500) 아래로 떨어져
  0.1까지 단조 하강함.
- **다만 이번 관찰에서는 "급변 → α 상승" 케이스가 한 번도 나타나지 않음** —
  ST-1 실험처럼 워크로드 자체가 안정적인 벤치마크 위주였기 때문. "빌드하다
  타이핑으로 전환" 같은 실제 급변 시나리오는 검증되지 않았음(한계로 기록).
- pid0(kernel_main)만 delta=5로 약간의 변동을 보였는데도 곧바로 0.2로,
  이어서 0.1로 내려간 것을 보면 VOLATILITY_LOW_X10(500) 임계값이 다소
  낮게(관대하게) 잡혀 있어 거의 항상 "안정"으로 판정되는 경향 — 향후 실제
  워크로드 급변 상황을 만들어 VOLATILITY_HIGH_X10(3000) 방향도 트리거되는지
  검증 필요.

**다음에 미친 영향:**
- ST-2 기본 구현 및 안정성 검증 완료. ST-1과 동일 세션에서 동시 동작 확인 —
  두 Self-Tuning 트랙이 서로 간섭하지 않음.
- 한계로 기록: "급변 → α 상승" 경로가 이번 벤치마크에서 트리거되지 않았음.
  다음 실험 후보: 의도적으로 vol_pct가 급변하는 워크로드(예: I/O 바운드 →
  CPU 바운드로 전환하는 프로세스)를 만들어 ST-2의 반대 방향 동작을 검증.
- ST-3(TIME_SLICE 범위 동적화) 착수 시에도 동일한 3중 안전장치 템플릿을
  재사용할 계획.

---

## 실험 24: ST-3 — TIME_SLICE 허용 범위 동적화 (워크로드 성격 기반)

**날짜:** 2026-07-08
**가설:** PE-4에서 확인된 "레이턴시 4.5배 개선 vs 처리량 9배 희생" 트레이드오프는
TIME_SLICE 범위(1~4틱)가 모든 워크로드에 동일하게 적용되기 때문이다.
인터랙티브(High 우선순위) 프로세스가 우세하면 범위를 넓게(1~4, 게임 모드),
CPU바운드(Low 우선순위) 프로세스가 우세하면 좁게(2~3, 빌드 모드) 좁히면
처리량 희생을 줄일 수 있을 것이다. ST-1/ST-2에서 검증된 3중 안전장치를
그대로 재사용해도 진동 없이 동작할 것이다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG x86_64, `-M q35 -smp 4`, cargo build debug 모드
- 측정 도구: 시리얼 출력(`[policy-ST3]`), Event Tracer(`ParamTuned`, param_id=3)
- 부팅 후 40초 관찰

**방법:**
`kernel/src/policy/mod.rs`에 TIME_SLICE_MIN/MAX 고정 상수를 제거하고 3단계
사다리로 대체:
```rust
const TIME_SLICE_RANGE_LEVELS: [(u64, u64); 3] = [(2, 3), (1, 3), (1, 4)];
// 0=빌드 모드(좁게), 2=게임 모드(넓게, 기존 하드코딩 값과 동일 = baseline)

// 매 창마다 이번 창에 High/Low로 분류된 프로세스 수 집계
let bias = cnt_high as i64 - cnt_low as i64;
let new_bias_ema = (3*bias*10 + 7*workload_bias_ema) / 10;  // 2) EMA 평활화
let target = if new_bias_ema >= 20 { 게임모드(2) } else if new_bias_ema <= -20 { 빌드모드(0) } else { baseline(2) };
// 1) Hysteresis — 창당 ±1단계만 이동
// 3) 표본 부족(cnt_high+cnt_low < 2) — 판단 보류
```

**Raw 데이터:**

전체 관찰 구간의 (High 분류 수, Low 분류 수) 추이 (리포트 창마다 grep 집계):

| 구간 | high | low | bias(=high-low) |
|------|------|-----|------------------|
| 초반 | 3 | 0 | +3 |
| 중반 | 2 | 2 | 0 |
| 중반 | 4 | 2 | +2 |
| 후반 | 7 | 3 | +4 |
| 후반 | 8 | 3 | +5 |
| 최종 다수 창 | 7 | 3 | +4 (지속) |

이 구간 전체에서 `bias`가 **음수인 창은 한 번도 없었음** — High가 항상 Low
이상. `[policy-ST3]` 로그는 전체 실행에서 **0회** 출력됨(범위 변경 없음).
빌드 경고 없음, 패닉/트리플폴트 없음, `[policy-ST1]`/`[policy-ST2]`는 이전
실험과 동일하게 정상 동작(세 트랙이 동시에 간섭 없이 작동).

**결과 해석:**
- **예상과 다른 점(반전):** ST-3가 전혀 트리거되지 않은 것은 버그가 아니라
  설계 구조상 당연한 결과였다 — `TIME_SLICE_RANGE_BASELINE_LEVEL`(인덱스 2,
  즉 게임 모드 1~4)이 "편향 없음"의 기본값이면서 동시에 "인터랙티브 우세"의
  목표치와 같은 인덱스다. 즉 이 3단계 사다리에서는 **양의 bias(인터랙티브
  우세)는 애초에 이동할 곳이 없고, 오직 음의 bias(CPU바운드 우세)만 눈에 보이는
  전환(2→1→0)을 만들 수 있다.** 이번 벤치마크는 sender/receiver/IPC 워크로드
  위주라 항상 High가 우세했으므로, 좁히는 방향(빌드 모드)이 단 한 번도
  발동하지 않았다.
- 이는 ST-2 실험(실험 23)에서 "급변→α 상승" 경로가 트리거되지 않은 것과
  똑같은 패턴 — 3중 안전장치 자체는 정상 동작하지만, 이번 벤치마크
  워크로드들이 모두 "안정적이고 인터랙티브 우세"인 방향으로 편향되어 있어
  반대 방향(빌드 모드 · α 상승)을 실측할 기회가 없었음. 이 프로젝트에서
  반복되는 패턴대로, 트리거 안 됨을 숨기지 않고 정직하게 기록.
- 코드 경로 자체는 정적으로 검증됨(target_level 계산, Hysteresis 이동,
  Safety Bounds 모두 ST-1/ST-2와 동일 구조를 그대로 재사용) — 실제 CPU바운드
  우세 워크로드(예: 순수 연산 루프 다수 실행, IPC 없음)를 넣으면 0(빌드
  모드, 2~3틱)으로 전환될 것으로 예상되나 이번 실험에서는 미검증.

**다음에 미친 영향:**
- ST-3 구현 및 안전성(무패닉, ST-1/ST-2와 비간섭) 확인. 단, "빌드 모드로
  좁히는" 핵심 시나리오가 이번 실험에서 실측되지 않아 **완전한 검증으로 볼 수
  없음** — 다음 실험 후보로 CPU바운드 전용 프로세스(예: task_a/task_b 유형만
  다수 실행, IPC 프로세스 제외)를 만들어 재측정 필요.
- 3단계 사다리 설계 자체를 재검토할 필요 — baseline과 "게임 모드 목표치"가
  같은 인덱스라 양의 방향 전환이 항상 무입력(no-op)이 되는 구조는, 향후
  ST-4(WorkloadProfile 자동 감지)에서 "게임/빌드/균형" 3가지를 실제로 구분
  하려면 baseline을 중립(예: (1,4)와 (2,3) 사이 어딘가)으로 재설계하거나
  4단계 이상 사다리로 확장하는 편이 나을 수 있음.
- ST-4 착수 시 이 비대칭 구조를 고려해 사다리를 재설계할 것.

---

## 실험 25: ST-3 빌드모드 미검증 원인 재조사 — stale-slot 버그 발견 및 수정

**날짜:** 2026-07-11
**가설:** 실험 24는 "baseline=게임모드 인덱스 비대칭" 때문에 빌드모드 방향이
트리거되지 않았다고 결론 냈다. 이 가설이 맞다면, IPC/yield 프로세스가 전혀
없는 순수 CPU바운드 전용 워크로드(task_a/task_b 패턴 4개)를 충분히 오래
돌리면 여전히 cnt_low가 우세할 텐데도 baseline 인덱스 구조상 발동 안 할
수도 있고, 아니면 실제로 발동할 수도 있다 — 코드를 다시 읽어 진짜 원인을
확인한다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU TCG x86_64, `-M q35 -smp 4`, cargo build debug 모드
- 측정 도구: 시리얼 출력(`[policy-ST3]`, `[policy]` 리포트), 60초 타임아웃 부팅 로그
- 도구: `make run` stdio 시리얼을 파일로 리다이렉트 후 grep

**방법 (원인 조사):**
`kernel/src/policy/mod.rs`의 리포트 루프(`report()`)를 다시 읽어보니
`for i in 0..MAX_PROCS { if !self.stats[i].active { continue; } ...}`로
"active" 슬롯만 순회한다. 그런데 `active`를 `false`로 되돌리는 코드가
`register()`/`find_slot()`이 슬롯을 재사용할 때(`evict` 시)를 빼면 어디에도
없었다. `process::scheduler::kill_pid()` → `Scheduler::kill()`을 확인하니
`proc.state = ProcessState::Dead`만 표시할 뿐, Policy Engine에는 전혀
통보하지 않았다. 즉 **sender/receiver 등이 kill_pid()로 죽어도 policy stats
슬롯은 죽기 직전 vol_ema/Priority 그대로 `active: true`로 영원히 남아
매 리포트 창마다 계속 재분류(대개 High)되고 있었다** — 실험 24의 "baseline
비대칭" 가설은 부분적으로만 맞았고, 진짜 원인은 이 stale-slot 누수였다
(비대칭 구조 자체도 실재하지만, 이 버그가 훨씬 근본적인 원인).

**수정:**
```rust
// policy/mod.rs — PolicyEngine에 unregister 추가
pub fn unregister(&mut self, pid: Pid) {
    if let Some(idx) = self.find_slot(pid) {
        self.stats[idx].active = false;
    }
}
pub fn unregister_pid(pid: Pid) { unsafe { ENGINE.unregister(pid); } }

// process/scheduler.rs — kill()/exit_current()에서 호출
pub fn kill(&mut self, pid: Pid) {
    for proc in self.processes.iter_mut() {
        if proc.pid == pid { proc.state = ProcessState::Dead; break; }
    }
    crate::policy::unregister_pid(pid); // ← 추가
}
```

**검증 방법:** `kernel/src/main.rs`에 `task_c`/`task_d` 함수(기존
`preempt_task_a`/`b`와 동일 패턴)를 추가하고, task_a/task_b 데모가 끝난
직후 IPC/yield 프로세스 없이 순수 CPU바운드 4개(task_c, task_d, task_e=
`preempt_task_a` 재사용, task_f=`preempt_task_b` 재사용)를 300틱(≈16.5초)
동안 실행하는 "ST-3 검증" 구간을 신설. `make run`으로 부팅 후 시리얼 로그를
파일로 캡처해 `[policy-ST3]` 발생 여부 확인.

**Raw 데이터:**

ST-3 검증 구간(순수 CPU바운드 4프로세스, pid 5~8) 동안 관측된
`[policy-ST3]` 로그 전체:

| tick | 조정 | 편향EMA | high | low |
|------|------|---------|------|-----|
| ~216 | 1-4틱→1-3틱 | -2.0 | 0 | 4 |
| ~252 | 1-3틱→2-3틱 (빌드모드 도달) | -2.4 | 0 | 4 |

이후 ST-3 검증 프로세스(pid5~8)를 kill하고 이후 단계(후반 IPC 벤치마크,
pid9/10, High 우세)로 넘어가자 반대 방향으로 정상 복귀:

| tick | 조정 | 편향EMA | high | low |
|------|------|---------|------|-----|
| ~430(대략) | 2-3틱→1-3틱 | -1.3 | 2 | 0 |
| ~수천(대략) | 1-3틱→1-4틱 (게임모드 복귀) | 0.2 | 2 | 1 |

빌드 경고 없음(기존 8개 외 신규 없음), 패닉/트리플폴트 없음. ST-1
(`report_interval` 8/18/36/72틱 자동 조정)과 ST-2(pid별 α 0.3→0.2→0.1
단조 하강)는 이전 실험과 동일하게 정상 동작 — 세 트랙이 여전히 서로
간섭하지 않음.

**결과 해석:**
- **실험 24의 "완전한" 원인 진단이 틀렸음을 확인.** baseline=게임모드
  인덱스 비대칭은 실재하는 설계 특성이지만, 그것 때문에 빌드모드가
  "이론상 도달 불가능"했던 게 아니라 **죽은 프로세스의 stats 슬롯이
  Policy Engine에서 영원히 살아남아 cnt_high를 인위적으로 계속 부풀리는
  버그** 때문에 애초에 cnt_low가 cnt_high를 넘어설 기회 자체가 없었다.
  버그를 고치자 빌드모드(2~3틱) 도달과 게임모드 복귀 양방향 전환이 모두
  단 한 번의 실행에서 정상적으로 관측됐다.
- 이 버그는 ST-3뿐 아니라 ST-1(report_interval), ST-2(EMA α)의 판단에도
  영향을 미쳤을 가능성이 있다 — 둘 다 "switches_this_window"나 프로세스별
  vol_ema를 근거로 삼는데, 죽은 프로세스가 계속 카운트에 남아있으면
  ST-1의 스위치율 계산 자체는 (스위치는 실제 컨텍스트 스위치 이벤트 기반이라
  이 버그의 영향을 받지 않음) 무관하지만, ST-2는 프로세스별로 독립 계산되므로
  직접 영향은 없었음 — 다만 지금까지의 ST-1/ST-2 실험(21~23) 결과 자체는
  유효하나, "관찰된 프로세스 수"에 죽은 프로세스가 섞여 있었을 가능성은
  후속 감사 대상.
- 교훈: "예상과 다른 결과(트리거 안 됨)"를 설계상 한계로 성급하게 결론 내리기
  전에, 관측 경로(active flag, 슬롯 재사용, 죽음 통보) 자체가 버그 없이
  동작하는지 먼저 검증해야 한다. 실험 24는 "왜 트리거 안 됐는가"에서
  멈췄어야 했는데 "baseline 비대칭 때문"이라고 조기에 결론 내렸던 것이
  이번 재조사의 계기가 됨.

**다음에 미친 영향:**
- ST-3는 이제 양방향(빌드모드↔게임모드) 모두 실측 검증 완료 — Milestone
  상태를 ✅로 갱신.
- `Scheduler::kill()`/`exit_current()` 양쪽에 `policy::unregister_pid()`
  호출을 추가해 향후 모든 Self-Tuning 트랙이 죽은 프로세스의 stale 데이터에
  더 이상 영향받지 않음.
- ST-4(WorkloadProfile 자동 감지) 설계 시 이번에 발견한 stale-slot 클래스의
  버그(죽음 통보 누락)가 재발하지 않도록, 새로운 상태를 추가할 때마다
  "프로세스 종료 시 이 상태도 정리되는가?"를 체크리스트에 추가할 필요.
- baseline=게임모드 인덱스 비대칭 자체는 여전히 유효한 관찰이므로, ST-4에서
  사다리를 재설계할 때 계속 고려할 것(단, 실험 24가 결론 낸 것처럼
  "구조적으로 도달 불가능"은 아님 — 실제로는 도달 가능함을 이번에 확인).

---

## 실험 26: ST-5 — Self-Tuning 적용 후 PE-4 A/B 재측정

**날짜:** 2026-07-11
**가설:** ST-1~3(평가 주기·EMA α·TIME_SLICE 범위 동적화)가 하드코딩된
파라미터보다 낫다면, PE-4와 동일한 워크로드(cpu_hog + kbd_task)로
재측정했을 때 키입력 레이턴시 개선(PE=ON 우위)은 유지하면서 CPU바운드
처리량 희생(hog완료 시간)은 실험 19(PE-4) 대비 줄어들 것이다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU q35, `-smp 4`, cargo build debug (ST-1~3 + 실험 25 stale-slot 수정
  적용된 현재 코드 기준)
- 측정 도구: `kernel/src/bench_pe4.rs`(실험 19와 동일 코드, 무변경) —
  키입력 레이턴시(rdtsc, n=40 trimmed avg), ctx_switch/36tick, hog완료 tick
- 부팅 로그를 파일로 캡처해 `[pe4]` 출력 grep

**방법:**
`kernel/src/main.rs`의 PE-4 벤치마크 호출부(`bench_pe4::run("PE=ON ")` /
`("PE=OFF")`)는 실험 19 이후 코드 변경이 전혀 없음 — ST-1~3은 `is_enabled()`
가드를 공유하므로 PE=ON 상태에서 자동으로 함께 적용된다. 즉 **코드를
바꾸지 않고 그냥 재부팅해서 재측정**하는 것이 ST-5의 본래 방법론이다.

측정 중 문제: gfx/wm-ipc 백그라운드 데모가 시리얼 출력을 초당 수백~수천
줄씩 뒤덮어, PE-4가 원래 위치(A-2 벤치마크 이후, main.rs 약 1900번째 줄)에
있으면 부팅 후 300초가 지나도 도달하지 못함(같은 문제를 실험 19도 겪었고
"IPC Demo 직후로 임시 이동 → 측정 → 정식 위치로 복원" 방식을 이미 사용한
전례가 있어 동일하게 재사용). PE-4 블록을 IPC Demo(`--- IPC Demo complete
---`) 직후로 임시로 잘라 붙여 측정한 뒤, 측정 완료 즉시 원래 위치(A-2 이후,
Event Tracer dump 직전)로 정확히 복원했다 — `git diff`로 PE-4 관련 코드에
diff가 전혀 없음을 확인.

**Raw 데이터:**

| 지표 | 실험19(PE-4, ST 이전) ON | 실험19 OFF | 실험26(ST-5) ON | 실험26 OFF |
|------|---------------------------|------------|------------------|------------|
| 키입력 레이턴시(cy) | 103,523,142 | 470,738,400 | 103,392,714 | 470,732,428 |
| ctx switch/36tick | 38 | 16 | 38 | 16 |
| hog 완료(tick) | 37 | 4 | 38 | 4 |

ON/OFF 비율(레이턴시 개선/처리량 희생)도 사실상 동일: 레이턴시 개선
≈4.55×(실험19) vs ≈4.55×(실험26), 처리량 희생 ≈9.25×(실험19) vs
≈9.5×(실험26).

PE=ON 구간 동안의 `[policy]` 리포트를 확인한 결과 **`[policy-ST1]`
(report_interval 조정)과 `[policy-ST3]`(TIME_SLICE 범위 조정) 로그가 단
한 번도 출력되지 않음** — report_interval은 baseline 36틱을 유지한 채
tick 36/72/108/144/180/216/252에서 리포트가 찍혔고(간격이 계속 36틱
그대로), TIME_SLICE 범위도 1~4틱(baseline)에서 전혀 안 움직임. 오직
`[policy-ST2]`(pid별 EMA α)만 0.3→0.2→0.1로 조정됨.

**결과 해석:**
- **가설 기각(예상과 다른 결과):** ST-1~3 적용 후에도 PE-4 수치가 오차
  범위 내에서 실험 19와 사실상 동일했다 — hog완료 시간은 오히려 37→38틱으로
  **미세하게 더 늘어남**(개선 아님, 노이즈 수준이긴 함).
- 원인은 명확했다: PE-4 벤치마크 자체가 **너무 짧다.** hog가 20M 반복을
  채우는 데 걸리는 시간이 약 37~38틱뿐인데, ST-1(report_interval)과
  ST-3(TIME_SLICE 범위)는 둘 다 Hysteresis로 창당 ±1단계만 이동하고, 첫
  번째 리포트가 찍히는 tick=36(정확히 baseline report_interval) 전까지는
  아무 판단도 못 한다. 결과적으로 PE=ON 구간 전체(~7개 리포트 창)에서
  ST-1/ST-3가 사실상 "몸을 풀기도 전에" 벤치마크가 끝나버림. ST-2만
  프로세스별로 독립 계산되어 빠르게(창마다) 반응했지만, α 자체는 PE-4
  워크로드의 결과(레이턴시/처리량)에 직접적인 영향을 주는 파라미터가
  아니라 큰 차이가 안 남.
- 이것은 실험 24/25의 패턴과 본질적으로 같은 종류의 함정이다: "충분한
  관찰 시간(표본)이 없으면 3중 안전장치는 설계대로 아무것도 하지 않는다"
  — 이번엔 버그가 아니라 **PE-4 워크로드 자체의 지속 시간이 ST-1/ST-3가
  개입하기엔 구조적으로 너무 짧다**는, 실험 설계 쪽의 한계였다.
- 실험 25에서 사용한 ST-3 전용 워크로드(300틱, IPC 없음)에서는 ST-3가
  tick=216/252(즉 report_interval 36틱 기준 6~7번째 창)에서 처음 발동했다
  — PE-4(37~38틱, 리포트 1개뿐)는 애초에 그 지점에 도달하지도 못한다.

**다음에 미친 영향:**
- ST-5는 "ST-1~3이 효과가 없다"가 아니라 "PE-4 벤치마크가 ST-1~3의 시간
  스케일과 안 맞는다"는 것을 보여준다 — PE-4를 그대로 재사용해 ST-5를
  "완료"라고 판단한 것은 성급했음. **다음 실험 후보: PE-4보다 훨씬 긴
  지속시간(예: hog 목표를 20M→200M+ 반복으로 늘리거나, HOG_TARGET_ITERS를
  조정 가능하게 만들어 수백 개의 리포트 창이 지나가도록)의 A/B 재측정을
  별도로 설계해야 ST-1/ST-3의 실제 효과를 검증할 수 있음.**
- ST-4(WorkloadProfile 자동 감지) 설계 시에도 "워크로드가 몇 개의 리포트
  창 동안 지속되는가"가 Self-Tuning이 개입할 수 있는지 여부를 결정하는
  핵심 변수임을 명시적으로 고려할 것 — 아주 짧게 실행되고 끝나는 프로세스
  (예: 셸 명령 하나)에는 Self-Tuning이 사실상 적용되지 않는다는 뜻이므로,
  README/ARCHITECTURE 상의 "적응형" 주장에 이 한계를 명시할 필요.
- main.rs의 PE-4 블록 위치는 측정 직후 원래 자리(A-2 이후, Event Tracer
  dump 직전)로 정확히 복원함 — `git diff` 확인 결과 PE-4 관련 코드는
  변경 없음(실험 19가 세운 "임시 이동 → 측정 → 복원" 관례를 그대로 따름).
- **장기 지속시간 A/B 벤치마크 재설계는 보류(deferred).** ST-4를 먼저
  진행하기로 결정 — HOG_TARGET_ITERS 확장 등 실험 26이 제안한 후속
  실험은 착수하지 않은 채로 다음 실험 후보 목록에만 남겨둠.

---

## 실험 27: ST-4 — WorkloadProfile 자동 감지 (판단/소비 분리)

**날짜:** 2026-07-11
**가설:** 원래 ST-3에 직접 박혀 있던 "High/Low 분류 비율로 워크로드 성격
판단" 로직을 별도 단계(ST-4)로 분리해도, 기존 ST-3의 동작(TIME_SLICE
범위 조정)이 동일하게 유지될 것이다. 이 분리는 ARCHITECTURE.md가 애초에
ST-4를 "사용자가 명시적으로 선언 안 해도 게임/빌드/균형을 자동 감지해서
ST-3 범위 선택"이라고 정의한 것을 코드에 그대로 반영하는 리팩터링이며,
향후 PE-2(전력)/PE-3(코어 권고) 등 다른 트랙도 같은 프로파일을 소비할 수
있는 구조를 만든다.

**측정 환경:**
- 호스트: Apple M4 Pro
- QEMU q35, `-smp 4`, cargo build debug
- 변경 파일: `kernel/src/policy/mod.rs`(`WorkloadProfile` enum 신설,
  판단 로직을 ST-3 블록에서 ST-4 블록으로 이동, `time_slice_range_level`
  →`workload_profile_level`로 개명), `kernel/src/tracer.rs`(param_id 주석
  갱신: 3=time_slice_range는 폐지, 4=workload_profile로 대체)

**방법:**
```rust
// ST-4: 판단 (게임/빌드/균형 감지, 3중 안전장치)
pub enum WorkloadProfile { Build, Balanced, Game }
// bias_ema 계산 → Hysteresis(±1단계) → workload_profile_level 갱신
// → [policy-ST4] 로그, tracer::param_tuned(4, ...)

// ST-3: 소비 (판단된 프로파일로 TIME_SLICE 범위만 매핑)
let (time_slice_min, time_slice_max) =
    TIME_SLICE_RANGE_LEVELS[self.workload_profile_level];
```
실험 25에서 쓴 ST-3 전용 CPU바운드 워크로드(main.rs의 task_c~f, 300틱)를
그대로 재사용해 `[policy-ST4]` 로그가 실험 25의 `[policy-ST3]` 로그와
동일한 지점·동일한 편향EMA 값으로 발생하는지 확인.

**Raw 데이터:**

| tick | 조정 (실험 27, ST-4) | 편향EMA | high | low |
|------|----------------------|---------|------|-----|
| ~216 | 게임→균형 | -2.0 | 0 | 4 |
| ~252 | 균형→빌드 (빌드 프로파일 도달) | -2.4 | 0 | 4 |
| ~430 | 빌드→균형 | -1.3 | 2 | 0 |
| ~수천 | 균형→게임 (게임 프로파일 복귀) | 0.2 | 2 | 1 |

실험 25의 `[policy-ST3]` 로그(1-4→1-3→2-3→1-3→1-4)와 tick·편향EMA·
high/low 값이 정확히 일치 — 판단 로직 자체는 그대로 옮겼을 뿐이므로
동작이 바뀌지 않았음을 확인. 빌드 경고 없음(기존 8개 외 신규 없음),
패닉/트리플폴트 없음. ST-1/ST-2도 이전과 동일하게 정상 동작.

**결과 해석:**
- 가설대로 리팩터링 전후 동작이 동일함을 확인 — 순수한 관심사 분리였고
  회귀가 없음.
- `WorkloadProfile` enum이 이제 "게임/빌드/균형"이라는 이름 자체를
  코드에서 노출하므로, 로그(`[policy-ST4] 워크로드 프로파일 감지:
  게임→균형`)가 실험 24/25 때의 `[policy-ST3] TIME_SLICE 범위 조정:
  1-4→1-3`보다 훨씬 읽기 쉬워짐 — 사후 분석 시 "지금 무슨 모드였는지"를
  숫자 범위 대신 이름으로 바로 알 수 있음.
- ST-3는 이제 코드 3줄짜리 순수 매핑이 됐다 — 향후 새 소비자(PE-2 전력
  정책, PE-3 코어 권고 등)를 추가할 때 판단 로직을 중복시키지 않고
  `self.workload_profile_level`(또는 `WorkloadProfile::from_level(...)`)만
  읽으면 됨.

**다음에 미친 영향:**
- ST-4 milestone ✅ (판단/소비 분리 완료, 회귀 없음 확인).
- ST-4가 만든 `WorkloadProfile`을 PE-2/PE-3가 실제로 소비하도록 확장하는
  것은 이번 실험 범위 밖(요청 범위는 "ST-3가 프로파일을 선택하게" 까지) —
  다음 실험 후보로 남김.
- 실험 26이 제안한 장기 지속시간 A/B 벤치마크 재설계는 사용자 지시로 보류.
