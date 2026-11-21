.PHONY: all iso run run-uefi run-arm run-almacen test smoke smoke-uefi smoke-arm smoke-almacen demo-almacen lint verify kani clean

all: iso

iso:
	tools/build-image.sh debug

run: iso
	tools/run-qemu.sh

run-uefi: iso
	tools/run-qemu.sh --uefi

# ARM64: QEMU virt con UEFI (ver docs/arm64.md)
run-arm:
	ARCH=aarch64 tools/build-image.sh debug
	ARCH=aarch64 tools/run-qemu.sh

# Almacenamiento propio (docs/almacenamiento.md), con un disco que sobrevive
# entre arranques. MODO: sin-fin (escribe hasta Ctrl-A X), verificar,
# escribir:N, caer:N, ajeno.
MODO ?= sin-fin
ALMACEN_DISCO ?= build/almacen.img
run-almacen:
	CMDLINE="almacen=$(MODO)" ISO=build/nsos-almacen.iso tools/build-image.sh debug
	@test -f $(ALMACEN_DISCO) || truncate -s 4M $(ALMACEN_DISCO)
	ISO=build/nsos-almacen.iso tools/run-qemu.sh \
		-drive if=none,id=alm,format=raw,file=$(ALMACEN_DISCO) -device virtio-blk-pci,drive=alm

# Tests unitarios en el host (ABI y lógica portable)
test:
	cargo test

# Arranque real en QEMU
smoke:
	tools/smoke-test.sh

# Almacenamiento: escribir, caída simulada, kill -9 a QEMU y disco ajeno
smoke-almacen:
	tools/store-test.sh

demo-almacen:
	tools/store-test.sh 10

smoke-uefi:
	tools/smoke-test.sh --uefi

smoke-arm:
	ARCH=aarch64 tools/smoke-test.sh

lint:
	cargo fmt --check
	cargo clippy -p nsos-abi -p nsos-kcore -p nsos-link -p nsos-store --all-targets -- -D warnings
	cargo clippy -p nsos-kernel --target x86_64-unknown-none -- -D warnings
	cargo clippy -p nsos-kernel --target aarch64-unknown-none-softfloat -- -D warnings
	RUSTFLAGS="-C relocation-model=static" cargo clippy --target x86_64-unknown-none \
		--target-dir target/cells -p nsos-rt -p cell-ping -p cell-pong -p cell-intruso -p cell-virtcon -p cell-hola -p cell-init -p cell-fragil -p cell-dormilona -p cell-contador-v1 -p cell-contador-v2 -p cell-cliente -p cell-contador-roto -p cell-caos -p cell-espera -p cell-bench -p cell-enlace -p cell-origen -p cell-destino -p cell-virtnet -p cell-virtblk -p cell-almacen -p cell-escritor -p cell-curioso -- -D warnings
	RUSTFLAGS="-C relocation-model=static" cargo clippy --target aarch64-unknown-none-softfloat \
		--target-dir target/cells -p nsos-rt -p cell-ping -p cell-pong -p cell-intruso -p cell-virtcon -p cell-hola -p cell-init -p cell-fragil -p cell-dormilona -p cell-contador-v1 -p cell-contador-v2 -p cell-cliente -p cell-contador-roto -p cell-caos -p cell-espera -p cell-bench -p cell-enlace -p cell-origen -p cell-destino -p cell-virtnet -p cell-virtblk -p cell-almacen -p cell-escritor -p cell-curioso -- -D warnings
	tools/audit-unsafe.sh

# Demostraciones en Lean y regeneración de las trazas del modelo
verify:
	cd verification/lean && lake build && \
		lake exe tracegen ../../crates/kcore/tests/data/cspace_model.trace
	cargo test -p nsos-kcore --test model_traces

# Pruebas de Kani sobre el código Rust (ver verification/kani.md)
kani:
	cargo kani -p nsos-abi
	cargo kani -p nsos-kcore
	cargo kani -p nsos-link
	cargo kani -p nsos-store

clean:
	cargo clean
	rm -rf build
