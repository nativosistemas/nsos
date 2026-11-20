#!/usr/bin/env bash
# Tests en QEMU:
#   1. arranque + tests internos del kernel (ktest), debe salir con éxito;
#   2. test negativo: una escritura en .rodata debe terminar en #PF y pánico
#      controlado (comprueba W^X y la ruta de excepciones);
#   2b. test negativo de SMAP: que el kernel lea una página de usuario
#      directamente debe terminar en #PF (la CPU hace cumplir que solo se
#      copie por `usercopy`);
#   3. migración entre dos QEMU por un enlace autenticado sobre virtio-net
#      (tools/migrate-test.sh), y la prueba negativa con otra clave;
#   3b. almacenamiento propio: escribir, caída simulada, kill -9 a QEMU y un
#      disco ajeno que no se toca (tools/store-test.sh; solo con BIOS);
#   4. benchmarks (bandera `bench`, kernel en release): solo la cell `bench`
#      corriendo; ver tools/bench-summary.sh.
# En el caso 1 además se exige que el texto de la cell `hola` haya salido
# por la consola virtio (driver `virtcon` en user-space).
#
# Con ARCH=aarch64 (QEMU `virt`, UEFI) corren los casos 1, 2 y 2b (con PAN
# en vez de SMAP) y los benchmarks; no hay migración ni consola virtio
# todavía (falta PCI en ARM64). La salida de QEMU es por semihosting.
#
# Uso: tools/smoke-test.sh [--uefi]
#      ARCH=aarch64 tools/smoke-test.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TIMEOUT="${TIMEOUT:-60}"
ARCH="${ARCH:-x86_64}"
export ARCH
EXTRA=""
[[ "$ARCH" == "aarch64" ]] && EXTRA=" semihosting"

# run_case NOMBRE CMDLINE ESTADO_ESPERADO PATRÓN [args de run-qemu...]
run_case() {
    local name="$1" cmdline="$2" want="$3" pattern="$4"
    shift 4
    local iso="$ROOT/build/nsos-$ARCH-$name.img" log="$ROOT/build/$name.log"
    local vlog="$ROOT/build/$name-virtcon.log"
    [[ "$ARCH" == "x86_64" ]] && iso="$ROOT/build/nsos-$name.iso"
    [[ "$ARCH" == "aarch64" ]] && log="$ROOT/build/$ARCH-$name.log"
    rm -f "$vlog"
    CMDLINE="$cmdline$EXTRA" ISO="$iso" "$ROOT/tools/build-image.sh" "${PROFILE:-debug}" >/dev/null
    set +e
    local start end status
    start=$(date +%s%N)
    ISO="$iso" VIRTCON_LOG="$vlog" timeout "$TIMEOUT" "$ROOT/tools/run-qemu.sh" "$@" >"$log" 2>&1 </dev/null
    status=$?
    end=$(date +%s%N)
    set -e
    echo "=== $name (QEMU ${status}, $(( (end - start) / 1000000 )) ms incluyendo firmware) ==="
    cat "$log"
    if [[ $status -ne $want ]]; then
        echo "FALLO [$name]: QEMU salió con estado $status (se esperaba $want)" >&2
        return 1
    fi
    if ! grep -q "$pattern" "$log"; then
        echo "FALLO [$name]: no apareció \"$pattern\"" >&2
        return 1
    fi
    if [[ -n "${VIRTCON_PATTERN:-}" ]] && ! grep -q "$VIRTCON_PATTERN" "$vlog" 2>/dev/null; then
        echo "FALLO [$name]: la consola virtio no contiene \"$VIRTCON_PATTERN\"" >&2
        return 1
    fi
    echo "OK [$name]"
}

# isa-debug-exit (x86_64) o semihosting (aarch64): éxito 33, fallo 35
if [[ "$ARCH" == "aarch64" ]]; then
    TIMEOUT="${TIMEOUT_ARM:-180}"
    run_case boot "test-exit" 33 "la memoria de las cells se liberó ... ok" "$@"
    grep -q "ktest: .* 0 fallos" "$ROOT/build/$ARCH-boot.log"
    if grep -q "FALLO" "$ROOT/build/$ARCH-boot.log"; then
        echo "FALLO [boot]: algún test falló" >&2
        exit 1
    fi
    run_case wx-fault "test-wx-fault" 35 "escritura sin permiso" "$@"
    run_case pan-fault "test-smap-fault" 35 "lectura sin permiso" "$@"
    PROFILE=release run_case bench "bench" 33 "bench: ipc_ida_y_vuelta_ns" "$@"
    echo "OK: todos los tests en QEMU (aarch64) pasaron"
    exit 0
fi

VIRTCON_PATTERN="hola desde una cell" run_case boot "test-exit" 33 "ktest: .* 0 fallos" "$@"
run_case wx-fault "test-wx-fault" 35 "escritura sin permiso" "$@"
run_case smap-fault "test-smap-fault" 35 "lectura sin permiso" "$@"
for variant in "" --clave-distinta; do
    log="$ROOT/build/migra${variant}.log"
    # shellcheck disable=SC2086
    "$ROOT/tools/migrate-test.sh" $variant "$@" >"$log" 2>&1 || {
        cat "$log"
        exit 1
    }
    tail -n 1 "$log"
done
# Almacenamiento propio (una vez, con BIOS): ver tools/store-test.sh.
if [[ $# -eq 0 ]]; then
    log="$ROOT/build/almacen.log"
    "$ROOT/tools/store-test.sh" >"$log" 2>&1 || {
        cat "$log"
        exit 1
    }
    grep "^almacen:\|^OK" "$log"
fi
PROFILE=release run_case bench "bench" 33 "bench: ipc_ida_y_vuelta_ns" "$@"
echo "OK: todos los tests en QEMU pasaron"
