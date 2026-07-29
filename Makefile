# Makefile — MuKernel 빌드 & 실행
#
# 환경: macOS (Apple Silicon / aarch64)
# 타겟: x86_64 베어메탈 (QEMU 에뮬레이션)
# 부트: limine UEFI (macOS에서는 BIOS 인스톨러를 실행할 수 없으므로 UEFI 사용)
#
# 사전 준비:
#   brew install xorriso    # ISO 이미지 생성 도구
#   rustup toolchain install nightly   # (kernel/rust-toolchain.toml이 자동 처리)

# ==================== 경로 설정 ====================

# 커널 ELF 바이너리 (cargo가 생성하는 위치)
KERNEL_ELF := kernel/target/x86_64-unknown-none/debug/kernel

# MuShell 유저 프로세스 ELF (ALPHA 15)
MUSHELL_ELF := build/mushell.elf
MUSHELL_DIR := user/mushell

# ALPHA 16 패키지 ELF들
SYSINFO_ELF := build/sysinfo.elf
MUECHO_ELF  := build/muecho.elf
MUCAT_ELF   := build/mucat.elf
# BETA 7 패키지 ELF들
MULS_ELF    := build/muls.elf
MUPWD_ELF   := build/mupwd.elf
PKG_ELFS    := $(SYSINFO_ELF) $(MUECHO_ELF) $(MUCAT_ELF) $(MULS_ELF) $(MUPWD_ELF)

# BETA 21: musl-linked 동적 바이너리 + 런타임
MUSL_SO     := build/lib/ld-musl-x86_64.so.1
DYN_HELLO   := build/dyn_hello.elf
# 커널이 include_bytes!로 박아넣는 musl 정적 테스트 바이너리
MUSL_HELLO  := build/musl_hello.elf
MUSL_UNAME  := build/musl_uname.elf

# ALPHA 17: 8x8 비트맵 폰트 바이너리 (scripts/gen_font.py 생성)
FONT_BIN := build/font8x8.bin

# ISO 관련
ISO_DIR    := build/iso_root
ISO        := build/mukernel.iso

# limine 바이너리 릴리즈 (git clone으로 다운로드)
LIMINE_DIR := limine

# QEMU UEFI 펌웨어 (Homebrew qemu에 포함되어 있음)
OVMF_CODE  := /opt/homebrew/Cellar/qemu/$(shell qemu-system-x86_64 --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')/share/qemu/edk2-x86_64-code.fd

# ext4 루트 파일시스템 이미지 (커널에 embed됨, ALPHA 8)
ROOTFS_IMG     := build/rootfs.ext4
ROOTFS_STAGING := build/rootfs_staging
# macOS: brew install e2fsprogs
MKE2FS := $(shell \
	command -v mke2fs 2>/dev/null \
	|| echo /opt/homebrew/opt/e2fsprogs/sbin/mke2fs)

# VirtIO Block 디스크 이미지 (ALPHA 10, 1MB, raw format)
DISK_IMG := build/disk.img

# ==================== 기본 타겟 ====================

.PHONY: all run run-gui clean limine-fetch kernel rootfs disk font

all: $(ISO)

# ==================== limine 다운로드 ====================

# limine 바이너리 릴리즈를 git clone으로 가져옴.
# v8.x-binary 브랜치에는 소스 없이 컴파일된 바이너리만 있음:
#   - limine-bios.sys      : BIOS 부트 지원 파일
#   - limine-bios-cd.bin   : BIOS CD-ROM 부팅용 El Torito 이미지
#   - limine-uefi-cd.bin   : UEFI CD-ROM 부팅용 El Torito 이미지
#   - BOOTX64.EFI          : UEFI x86_64 부트로더
#   - BOOTIA32.EFI         : UEFI i386 부트로더 (32비트 UEFI용)
# limine 부트로더 바이너리 배포본을 받아온다.
#
# 주의: 디렉토리($(LIMINE_DIR))가 아니라 그 안의 실제 파일을 타겟으로 삼는다.
# 예전에는 디렉토리를 타겟으로 썼는데, limine이 중첩 git 저장소(gitlink)로
# 잘못 커밋돼 있어서 저장소를 clone하면 "빈 limine 디렉토리"가 생겼고,
# make가 디렉토리 존재만 보고 받아오기를 건너뛴 뒤 cp에서 실패했다
# (CI 도입하며 발견). 파일을 기준으로 삼으면 내용이 없을 때 다시 받는다.
$(LIMINE_DIR)/limine-bios.sys:
	@echo "[limine] Fetching limine binary release..."
	@rm -rf $(LIMINE_DIR)
	git clone https://github.com/limine-bootloader/limine.git \
		--branch=v8.x-binary \
		--depth=1 \
		$(LIMINE_DIR)
	@echo "[limine] Done."

$(LIMINE_DIR): $(LIMINE_DIR)/limine-bios.sys

limine-fetch: $(LIMINE_DIR)

# ==================== 커널 빌드 ====================

# kernel/.cargo/config.toml과 rust-toolchain.toml이 자동으로
# nightly toolchain + x86_64-unknown-none 타겟을 선택함.
#
# FORCE: 소스 파일 변경을 감지해서 항상 cargo에게 판단을 맡김.
#        cargo가 캐시를 확인하므로 실제 변경이 없으면 재빌드 안 함.
# ==================== ext4 루트 파일시스템 이미지 (ALPHA 8) ====================
#
# 커널이 `include_bytes!`로 이 이미지를 embed함.
# 사전 준비: brew install e2fsprogs
#
# -O ^metadata_csum,^64bit,^has_journal : 호환성을 위해 최신 ext4 기능 비활성화
#   → ext4-view 같은 순수 Rust 구현체와 호환 보장

.PHONY: rootfs
rootfs: $(ROOTFS_IMG)

$(ROOTFS_IMG): $(MUSL_SO) $(DYN_HELLO)
	@echo "[ext4] Building root filesystem image..."
	@test -x "$(MKE2FS)" || \
		(echo "ERROR: e2fsprogs not found. Run: brew install e2fsprogs" && exit 1)
	@rm -rf $(ROOTFS_STAGING) && mkdir -p \
		$(ROOTFS_STAGING)/etc \
		$(ROOTFS_STAGING)/var/log \
		$(ROOTFS_STAGING)/home \
		$(ROOTFS_STAGING)/lib \
		$(ROOTFS_STAGING)/bin
	@printf 'NAME=MuKernel\nVERSION=0.1.0-alpha\nPRETTY_NAME=MuKernel 0.1 Alpha\n' \
		> $(ROOTFS_STAGING)/etc/os-release
	@printf 'Hello from ext4!\nThis file lives on a real disk image.\n' \
		> $(ROOTFS_STAGING)/etc/motd
	@printf 'mukernel-alpha\n' \
		> $(ROOTFS_STAGING)/etc/hostname
	@printf '[boot] kernel started\n[alpha8] ext4 mounted\n' \
		> $(ROOTFS_STAGING)/var/log/boot.log
	@printf 'Welcome to MuKernel!\n' \
		> $(ROOTFS_STAGING)/home/welcome.txt
	@# BETA 21: musl 동적 링커 + 테스트 바이너리
	@cp $(MUSL_SO) $(ROOTFS_STAGING)/lib/ld-musl-x86_64.so.1
	@cp $(DYN_HELLO) $(ROOTFS_STAGING)/bin/hello_dyn
	@echo "[ext4] /lib/ld-musl-x86_64.so.1 포함 ($(shell wc -c < $(MUSL_SO) | tr -d ' ') bytes)"
	@echo "[ext4] /bin/hello_dyn 포함 ($(shell wc -c < $(DYN_HELLO) | tr -d ' ') bytes)"
	@$(MKE2FS) -t ext4 -d $(ROOTFS_STAGING) -F -q \
		-L "mukernel-root" \
		-O ^metadata_csum,^64bit,^has_journal \
		$(ROOTFS_IMG) 1M 2>/dev/null || \
	$(MKE2FS) -t ext2 -d $(ROOTFS_STAGING) -F -q \
		-L "mukernel-root" \
		$(ROOTFS_IMG) 1M
	@echo "[ext4] Image ready: $(ROOTFS_IMG) ($$(wc -c < $(ROOTFS_IMG) | tr -d ' ') bytes)"

# ==================== VirtIO Block 디스크 이미지 (ALPHA 10) ====================

disk: $(DISK_IMG)

$(DISK_IMG):
	@echo "[disk] Creating VirtIO block device image (1MB)..."
	@mkdir -p build
	@dd if=/dev/zero of=$(DISK_IMG) bs=512 count=2048 2>/dev/null
	@printf 'MuKernel VirtIO Disk\n' | dd of=$(DISK_IMG) bs=512 count=1 conv=notrunc 2>/dev/null
	@echo "[disk] Image ready: $(DISK_IMG)"

# ==================== 커널 빌드 ====================

# ==================== MuShell 유저 프로세스 (ALPHA 15) ====================

.PHONY: mushell
mushell: $(MUSHELL_ELF)

$(MUSHELL_ELF): $(wildcard $(MUSHELL_DIR)/src/*.rs) $(MUSHELL_DIR)/Cargo.toml $(MUSHELL_DIR)/mushell.ld
	@mkdir -p build
	@echo "[mushell] Building MuShell (x86_64-unknown-none, release)..."
	cd $(MUSHELL_DIR) && cargo build --release
	cp $(MUSHELL_DIR)/target/x86_64-unknown-none/release/mushell $(MUSHELL_ELF)
	@echo "[mushell] Built: $(MUSHELL_ELF) ($$(wc -c < $(MUSHELL_ELF) | tr -d ' ') bytes)"

# ==================== ALPHA 16: 패키지 ELF 빌드 ====================

.PHONY: pkgs
pkgs: $(PKG_ELFS)

$(SYSINFO_ELF): $(wildcard user/pkgs/sysinfo/src/*.rs) user/pkgs/sysinfo/Cargo.toml user/pkgs/sysinfo/pkg.ld
	@mkdir -p build
	@echo "[mukg] Building sysinfo..."
	cd user/pkgs/sysinfo && cargo build --release
	cp user/pkgs/sysinfo/target/x86_64-unknown-none/release/sysinfo $(SYSINFO_ELF)
	@echo "[mukg] Built: $(SYSINFO_ELF) ($$(wc -c < $(SYSINFO_ELF) | tr -d ' ') bytes)"

$(MUECHO_ELF): $(wildcard user/pkgs/muecho/src/*.rs) user/pkgs/muecho/Cargo.toml user/pkgs/muecho/pkg.ld
	@mkdir -p build
	@echo "[mukg] Building muecho..."
	cd user/pkgs/muecho && cargo build --release
	cp user/pkgs/muecho/target/x86_64-unknown-none/release/muecho $(MUECHO_ELF)
	@echo "[mukg] Built: $(MUECHO_ELF) ($$(wc -c < $(MUECHO_ELF) | tr -d ' ') bytes)"

$(MUCAT_ELF): $(wildcard user/pkgs/mucat/src/*.rs) user/pkgs/mucat/Cargo.toml user/pkgs/mucat/pkg.ld
	@mkdir -p build
	@echo "[mukg] Building mucat..."
	cd user/pkgs/mucat && cargo build --release
	cp user/pkgs/mucat/target/x86_64-unknown-none/release/mucat $(MUCAT_ELF)
	@echo "[mukg] Built: $(MUCAT_ELF) ($$(wc -c < $(MUCAT_ELF) | tr -d ' ') bytes)"

$(MULS_ELF): $(wildcard user/pkgs/muls/src/*.rs) user/pkgs/muls/Cargo.toml user/pkgs/muls/pkg.ld
	@mkdir -p build
	@echo "[mukg] Building muls..."
	cd user/pkgs/muls && cargo build --release
	cp user/pkgs/muls/target/x86_64-unknown-none/release/muls $(MULS_ELF)
	@echo "[mukg] Built: $(MULS_ELF) ($$(wc -c < $(MULS_ELF) | tr -d ' ') bytes)"

$(MUPWD_ELF): $(wildcard user/pkgs/mupwd/src/*.rs) user/pkgs/mupwd/Cargo.toml user/pkgs/mupwd/pkg.ld
	@mkdir -p build
	@echo "[mukg] Building mupwd..."
	cd user/pkgs/mupwd && cargo build --release
	cp user/pkgs/mupwd/target/x86_64-unknown-none/release/mupwd $(MUPWD_ELF)
	@echo "[mukg] Built: $(MUPWD_ELF) ($$(wc -c < $(MUPWD_ELF) | tr -d ' ') bytes)"

# ==================== 커널 빌드 ====================

# ==================== ALPHA 17: 8x8 폰트 바이너리 ====================

font: $(FONT_BIN)

$(FONT_BIN): scripts/gen_font.py
	@mkdir -p build
	python3 scripts/gen_font.py

# ==================== BETA 21: musl 런타임 + 동적 바이너리 ====================

# musl 정적 테스트 바이너리 (ALPHA 14 ELF 로더 / BETA 21 데모용).
#
# 이 두 파일은 커널이 include_bytes!로 컴파일 타임에 박아넣기 때문에
# (kernel/src/pkg.rs, kernel/src/main.rs) 커널 빌드보다 반드시 먼저 있어야 한다.
# 예전에는 생성 규칙 없이 로컬 build/ 안에만 존재해서, 저장소를 새로 clone하면
# 커널이 아예 빌드되지 않는 상태였다 (CI 도입하면서 발견).
#
# musl-cross가 있으면 소스에서 직접 빌드하고, 없으면 저장소에 함께 커밋해 둔
# 사전 빌드본을 사용한다 — 어느 환경에서든 clone 직후 빌드가 되도록.
$(MUSL_HELLO): user/musl-test/hello.c user/musl-test/hello
	@mkdir -p build
	@MUSL_GCC=""; \
	for c in x86_64-linux-musl-gcc /opt/homebrew/bin/x86_64-linux-musl-gcc; do \
		if command -v $$c >/dev/null 2>&1; then MUSL_GCC=$$c; break; fi; \
	done; \
	if [ -n "$$MUSL_GCC" ]; then \
		$$MUSL_GCC -static -o $(MUSL_HELLO) user/musl-test/hello.c; \
		echo "[musl] $(MUSL_HELLO) 소스에서 빌드 완료"; \
	else \
		cp user/musl-test/hello $(MUSL_HELLO); \
		echo "[musl] musl-cross 없음 → 사전 빌드본 사용: $(MUSL_HELLO)"; \
	fi

$(MUSL_UNAME): user/musl-test/uname_test.c user/musl-test/uname_test
	@mkdir -p build
	@MUSL_GCC=""; \
	for c in x86_64-linux-musl-gcc /opt/homebrew/bin/x86_64-linux-musl-gcc; do \
		if command -v $$c >/dev/null 2>&1; then MUSL_GCC=$$c; break; fi; \
	done; \
	if [ -n "$$MUSL_GCC" ]; then \
		$$MUSL_GCC -static -o $(MUSL_UNAME) user/musl-test/uname_test.c; \
		echo "[musl] $(MUSL_UNAME) 소스에서 빌드 완료"; \
	else \
		cp user/musl-test/uname_test $(MUSL_UNAME); \
		echo "[musl] musl-cross 없음 → 사전 빌드본 사용: $(MUSL_UNAME)"; \
	fi

# musl .so: scripts/fetch_musl.sh가 빌드 (Alpine apk에서 추출)
$(MUSL_SO):
	@echo "[musl] musl 런타임 없음 → fetch_musl.sh 실행..."
	@mkdir -p build/lib
	bash scripts/fetch_musl.sh

# musl-linked 동적 테스트 바이너리
# 폴백 경로에서 $(MUSL_HELLO)를 복사하므로 그것도 선행 조건이다.
$(DYN_HELLO): user/musl-test/hello_dyn_start.c $(MUSL_SO) $(MUSL_HELLO)
	@echo "[musl] 동적 바이너리 빌드 시도..."
	@MUSL_GCC=""; \
	for c in x86_64-linux-musl-gcc /opt/homebrew/bin/x86_64-linux-musl-gcc; do \
		if command -v $$c >/dev/null 2>&1; then MUSL_GCC=$$c; break; fi; \
	done; \
	if [ -n "$$MUSL_GCC" ]; then \
		$$MUSL_GCC -nostartfiles \
			-Wl,--dynamic-linker,/lib/ld-musl-x86_64.so.1 \
			-o $(DYN_HELLO) user/musl-test/hello_dyn_start.c -lc; \
		echo "[musl] $(DYN_HELLO) 빌드 완료"; \
	else \
		echo "[musl] musl-cross 없음 → musl_hello.elf로 대체 (정적)"; \
		cp build/musl_hello.elf $(DYN_HELLO); \
	fi

# ==================== 커널 빌드 ====================

.PHONY: kernel
kernel: $(ROOTFS_IMG) $(MUSHELL_ELF) $(PKG_ELFS) $(FONT_BIN) $(MUSL_HELLO) $(MUSL_UNAME)
	@echo "[cargo] Building kernel (x86_64-unknown-none, debug)..."
	cd kernel && cargo build
	@echo "[cargo] Build complete: $(KERNEL_ELF)"

$(KERNEL_ELF): kernel

# ==================== ISO 이미지 생성 ====================
#
# UEFI 부팅을 위한 El Torito 하이브리드 ISO 구조:
#
# iso_root/
# ├── boot/
# │   ├── kernel              ← 우리 커널 ELF
# │   └── limine/
# │       ├── limine.conf     ← 부트로더 설정
# │       ├── limine-bios.sys     ← BIOS 부트 지원 (BIOS 모드용)
# │       ├── limine-bios-cd.bin  ← BIOS El Torito 이미지
# │       └── limine-uefi-cd.bin  ← UEFI El Torito 이미지
# └── EFI/
#     └── BOOT/
#         ├── BOOTX64.EFI     ← UEFI 부트로더 (x86_64)
#         └── BOOTIA32.EFI    ← UEFI 부트로더 (i386)

$(ISO): $(KERNEL_ELF) $(LIMINE_DIR)/limine-bios.sys limine.conf
	@echo "[iso] Creating ISO directory structure..."
	mkdir -p $(ISO_DIR)/boot/limine
	mkdir -p $(ISO_DIR)/EFI/BOOT

	# 커널 바이너리 복사
	cp $(KERNEL_ELF) $(ISO_DIR)/boot/kernel

	# limine 설정 파일 복사
	cp limine.conf $(ISO_DIR)/boot/limine/limine.conf

	# limine BIOS 파일 (BIOS 모드 지원용 — UEFI만 쓴다면 없어도 되지만 넣어둠)
	cp $(LIMINE_DIR)/limine-bios.sys    $(ISO_DIR)/boot/limine/
	cp $(LIMINE_DIR)/limine-bios-cd.bin $(ISO_DIR)/boot/limine/
	cp $(LIMINE_DIR)/limine-uefi-cd.bin $(ISO_DIR)/boot/limine/

	# UEFI 부트 파일 (EFI 시스템 파티션 구조)
	cp $(LIMINE_DIR)/BOOTX64.EFI  $(ISO_DIR)/EFI/BOOT/
	cp $(LIMINE_DIR)/BOOTIA32.EFI $(ISO_DIR)/EFI/BOOT/

	@echo "[iso] Running xorriso to create bootable ISO..."
	# xorriso: ISO 9660 이미지 생성 도구
	# -as mkisofs: mkisofs 호환 모드
	# -b: BIOS El Torito 부팅 이미지
	# -no-emul-boot: 디스크 에뮬레이션 없이 직접 부팅 이미지 사용
	# -boot-load-size 4: 부트 섹터 로드 크기 (4 × 512바이트 = 2KB)
	# -boot-info-table: 부트 이미지에 ISO 위치 정보 패치
	# --efi-boot: UEFI El Torito 부팅 이미지
	# -efi-boot-part --efi-boot-image: UEFI FAT 파티션 자동 생성
	# --protective-msdos-label: GPT 보호를 위한 MBR 레이블
	xorriso -as mkisofs \
		-b boot/limine/limine-bios-cd.bin \
		-no-emul-boot -boot-load-size 4 -boot-info-table \
		--efi-boot boot/limine/limine-uefi-cd.bin \
		-efi-boot-part --efi-boot-image \
		--protective-msdos-label \
		$(ISO_DIR) -o $(ISO)

	# BIOS 부트 섹터를 ISO에 설치
	# 참고: macOS에서 limine 설치 바이너리가 aarch64용이 아닐 수 있음.
	# UEFI로만 부팅한다면 이 단계는 선택사항.
	# $(LIMINE_DIR)/limine bios-install $(ISO) 2>/dev/null || \
	#     echo "[warn] limine bios-install skipped (UEFI only mode)"

	@echo "[iso] ISO created: $(ISO)"
	@ls -lh $(ISO)

# ==================== QEMU 실행 ====================

## UEFI 모드로 실행 (macOS에서 권장)
run: $(ISO) $(DISK_IMG)
	@echo "[qemu] Booting $(ISO) with UEFI..."
	@echo "[qemu] Serial output below (Ctrl+A, X to quit):"
	@echo "--------------------------------------------"
	qemu-system-x86_64 \
		-M q35 \
		-smp 4 \
		-cdrom $(ISO) \
		-drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
		-drive if=none,id=vd0,format=raw,file=$(DISK_IMG) \
		-device virtio-blk-pci,drive=vd0,disable-modern=on \
		-netdev user,id=net0 \
		-device virtio-net-pci,netdev=net0,disable-modern=on \
		-serial stdio \
		-m 256M \
		-no-reboot \
		-no-shutdown \
		-display none
	# 옵션 설명:
	# -M q35         : Q35 칩셋 에뮬레이션 (최신 PCIe + UEFI 지원)
	# -smp 4         : 4코어 에뮬레이션 (BETA 5 SMP 지원)
	# -cdrom         : CD-ROM 드라이브에 ISO 마운트
	# -drive pflash  : UEFI 펌웨어 플래시 메모리 (edk2-x86_64-code.fd)
	# -serial stdio  : COM1 시리얼 출력 → 호스트 터미널 (이게 우리의 println!)
	# -m 256M        : RAM 256MB
	# -no-reboot     : 트리플 폴트 시 QEMU 종료 (재부팅 대신)
	# -no-shutdown   : 셧다운 명령에도 QEMU 종료 안 함 (디버깅용)
	# -display none  : 그래픽 창 없음 (시리얼만 사용)

## GUI 모드: 그래픽 창 표시 (ALPHA 17 — -display none 제거)
run-gui: $(ISO) $(DISK_IMG)
	@echo "[qemu] Booting $(ISO) with UEFI + GUI display..."
	@echo "[qemu] Ctrl+Alt+G: 마우스 해제  Ctrl+A X: QEMU 종료"
	@echo "--------------------------------------------"
	qemu-system-x86_64 \
		-M q35 \
		-smp 4 \
		-cdrom $(ISO) \
		-drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
		-drive if=none,id=vd0,format=raw,file=$(DISK_IMG) \
		-device virtio-blk-pci,drive=vd0,disable-modern=on \
		-netdev user,id=net0 \
		-device virtio-net-pci,netdev=net0,disable-modern=on \
		-serial stdio \
		-m 256M \
		-no-reboot \
		-no-shutdown \
		-vga std

## BIOS 모드로 실행 (참고용, macOS에서는 limine bios-install 필요)
run-bios: $(ISO)
	qemu-system-x86_64 \
		-cdrom $(ISO) \
		-serial stdio \
		-m 256M \
		-no-reboot \
		-no-shutdown \
		-display none

## 디버그 모드: GDB 대기 (포트 1234)
## 다른 터미널에서: gdb kernel/target/.../kernel → target remote :1234
run-debug: $(ISO)
	qemu-system-x86_64 \
		-M q35 \
		-cdrom $(ISO) \
		-drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
		-serial stdio \
		-m 256M \
		-no-reboot \
		-no-shutdown \
		-display none \
		-s -S
	# -s: GDB 서버를 포트 1234에서 시작
	# -S: 시작 즉시 멈춤 (GDB가 연결할 때까지 대기)

# ==================== 정리 ====================

clean:
	@echo "[clean] Removing build artifacts..."
	rm -rf build/
	cd kernel && cargo clean
	cd $(MUSHELL_DIR) && cargo clean 2>/dev/null || true
	cd user/pkgs/sysinfo && cargo clean 2>/dev/null || true
	cd user/pkgs/muecho  && cargo clean 2>/dev/null || true
	cd user/pkgs/mucat   && cargo clean 2>/dev/null || true
	cd user/pkgs/muls    && cargo clean 2>/dev/null || true
	cd user/pkgs/mupwd   && cargo clean 2>/dev/null || true
	@echo "[clean] Done."
	# font8x8.bin은 build/ 디렉토리에 있으므로 rm -rf build/로 이미 삭제됨

# limine 디렉토리도 제거 (재다운로드 필요)
clean-all: clean
	rm -rf $(LIMINE_DIR)
