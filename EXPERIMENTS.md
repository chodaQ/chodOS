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
