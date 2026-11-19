.PHONY: all iso run run-uefi test smoke smoke-uefi lint clean

all: iso

iso:
	tools/build-image.sh debug

run: iso
	tools/run-qemu.sh

run-uefi: iso
	tools/run-qemu.sh --uefi

# Tests unitarios en el host (ABI y lógica portable)
test:
	cargo test

# Arranque real en QEMU
smoke:
	tools/smoke-test.sh

smoke-uefi:
	tools/smoke-test.sh --uefi

lint:
	cargo fmt --check
	cargo clippy -p nsos-abi --all-targets -- -D warnings
	cargo clippy -p nsos-kernel --target x86_64-unknown-none -- -D warnings
	RUSTFLAGS="-C relocation-model=static" cargo clippy --target x86_64-unknown-none \
		--target-dir target/cells -p nsos-rt -p cell-ping -p cell-pong -p cell-intruso -- -D warnings

clean:
	cargo clean
	rm -rf build
