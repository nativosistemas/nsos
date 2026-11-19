.PHONY: all iso run run-uefi test smoke smoke-uefi lint verify kani clean

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
	cargo clippy -p nsos-abi -p nsos-kcore --all-targets -- -D warnings
	cargo clippy -p nsos-kernel --target x86_64-unknown-none -- -D warnings
	RUSTFLAGS="-C relocation-model=static" cargo clippy --target x86_64-unknown-none \
		--target-dir target/cells -p nsos-rt -p cell-ping -p cell-pong -p cell-intruso -p cell-virtcon -p cell-hola -p cell-init -p cell-fragil -p cell-dormilona -p cell-contador-v1 -p cell-contador-v2 -p cell-cliente -p cell-contador-roto -- -D warnings

# Demostraciones en Lean y regeneración de las trazas del modelo
verify:
	cd verification/lean && lake build && \
		lake exe tracegen ../../crates/kcore/tests/data/cspace_model.trace
	cargo test -p nsos-kcore --test model_traces

# Pruebas de Kani sobre el código Rust (ver verification/kani.md)
kani:
	cargo kani -p nsos-abi
	cargo kani -p nsos-kcore

clean:
	cargo clean
	rm -rf build
