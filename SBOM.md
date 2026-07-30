# SBOM — 소프트웨어 자재명세서

chodOS가 사용·배포하는 제3자 소프트웨어 목록입니다.
본 프로젝트가 직접 작성한 코드의 라이선스는 [MIT](LICENSE)입니다.

조사 기준일: 2026-07-30 / 버전 출처: `kernel/Cargo.lock`

---

## 1. Rust 크레이트 (커널 바이너리에 링크됨)

### 직접 의존성

| # | 이름 | 버전 | 라이선스 | 저장소 | 사용 목적 |
|---|---|---|---|---|---|
| 1 | limine | 0.6.5 | MIT OR Apache-2.0 | https://github.com/robotman2412/limine-boot-rust | Limine 부트로더 프로토콜 구조체 정의 (메모리맵·프레임버퍼 수신) |
| 2 | linked_list_allocator | 0.10.6 | Apache-2.0 OR MIT | https://github.com/phil-opp/linked-list-allocator | 커널 힙 할당자 (`#[global_allocator]`) |
| 3 | spin | 0.9.8 | MIT | https://github.com/mvdnes/spin-rs | 스핀락 기반 동기화 (VFS 노드 내부 가변성, 전역 상태 보호) |
| 4 | ext4-view | 0.9.3 | MIT OR Apache-2.0 | https://github.com/nicholasbishop/ext4-view-rs | no_std ext4 읽기 전용 드라이버 |

### 전이 의존성

| # | 이름 | 버전 | 라이선스 | 저장소 | 유입 경로 |
|---|---|---|---|---|---|
| 5 | bitflags | 2.13.0 | MIT OR Apache-2.0 | https://github.com/bitflags/bitflags | ext4-view |
| 6 | crc | 3.4.0 | MIT OR Apache-2.0 | https://github.com/mrhooray/crc-rs | ext4-view (메타데이터 체크섬) |
| 7 | crc-catalog | 2.5.0 | MIT OR Apache-2.0 | https://github.com/akhilles/crc-catalog | crc |
| 8 | lock_api | 0.4.14 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot | spinning_top |
| 9 | scopeguard | 1.2.0 | MIT OR Apache-2.0 | https://github.com/bluss/scopeguard | lock_api |
| 10 | spinning_top | 0.2.5 | MIT OR Apache-2.0 | https://github.com/rust-osdev/spinning_top | linked_list_allocator |

Rust 표준 컴포넌트(`core`, `alloc`, `compiler_builtins`)는 `build-std`로 툴체인
소스에서 함께 빌드되며, Rust 프로젝트와 동일하게 MIT OR Apache-2.0입니다.

---

## 2. 배포물에 포함되는 제3자 바이너리

Cargo 의존성은 아니지만 **빌드 산출물(ISO/커널)에 실제로 담겨 배포**되므로
함께 명시합니다.

| # | 이름 | 버전 | 라이선스 | 저장소 | 포함 형태 |
|---|---|---|---|---|---|
| 11 | Limine 부트로더 | v8.7.0-binary | **BSD-2-Clause** | https://github.com/limine-bootloader/limine | ISO의 `BOOTX64.EFI`, `limine-bios.sys` 등. `make limine-fetch`가 upstream에서 받아옴 |
| 12 | musl libc | 1.2.5 (Alpine apk) | MIT | https://musl.libc.org | `ld-musl-x86_64.so.1`이 `rootfs.ext4`에 포함되고, 그 이미지가 `include_bytes!`로 커널에 임베드됨 (BETA 21 동적 링킹 데모용) |

musl로 정적 링크한 테스트 바이너리(`user/musl-test/hello`, `uname_test`)도
musl 코드를 포함하므로 같은 MIT 조건이 적용됩니다.

---

## 3. 라이선스 충돌 검토

| 항목 | 결과 |
|---|---|
| 본 프로젝트 라이선스 | MIT |
| 카피레프트(GPL/LGPL/AGPL) 포함 여부 | **없음** |
| 최대 제약 조건 | BSD-2-Clause (Limine) — 저작권 고지 및 면책조항 보존 의무 |
| MIT와의 양립성 | **충돌 없음.** MIT·Apache-2.0·BSD-2-Clause는 모두 허용적(permissive) 라이선스이며 상호 결합에 제약이 없음 |

**준수 사항:** BSD-2-Clause(Limine)와 MIT(musl 등)는 바이너리 형태로
재배포할 때 저작권 고지와 면책조항을 함께 제공해야 합니다. 배포물에 각
구성요소의 라이선스 원문을 포함해야 하며, 현재 Limine은 `make limine-fetch`로
받아올 때 `limine/LICENSE`가 함께 내려옵니다.

**Apache-2.0 관련:** dual 라이선스(MIT OR Apache-2.0) 크레이트는 MIT를
선택해 사용하므로, Apache-2.0의 특허 조항·NOTICE 요구사항은 적용되지
않습니다.

---

## 4. 저장소에 남아 있으나 빌드에 쓰이지 않는 것

| 경로 | 상태 |
|---|---|
| `rust-fs-ext4/` | 커널은 crates.io의 `ext4-view`를 사용하며 이 디렉토리를 참조하지 않음. 중첩 git 저장소(gitlink)로 잘못 커밋돼 clone 시 빈 디렉토리만 생성됨 — 정리 대상 |

---

## 재현 방법

```bash
# 버전 목록
grep -A2 '^\[\[package\]\]' kernel/Cargo.lock | grep -E '^name|^version'

# 각 크레이트의 라이선스·저장소 (로컬 레지스트리 캐시)
find ~/.cargo/registry/src -maxdepth 2 -type d -name '<크레이트>-<버전>' \
  -exec grep -m1 -E '^(license|repository)' {}/Cargo.toml \;
```
