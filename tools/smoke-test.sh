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
#   4. benchmarks (bandera `bench`, kernel en release): solo la cell `bench`
#      corriendo; ver tools/bench-summary.sh.
# En el caso 1 además se exige que el texto de la cell `hola` haya salido
# por la consola virtio (driver `virtcon` en user-space).
#
# Uso: tools/smoke-test.sh [--uefi]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TIMEOUT="${TIMEOUT:-60}"

# run_case NOMBRE CMDLINE ESTADO_ESPERADO PATRÓN [args de run-qemu...]
run_case() {
    local name="$1" cmdline="$2" want="$3" pattern="$4"
    shift 4
    local iso="$ROOT/build/nsos-$name.iso" log="$ROOT/build/$name.log"
    local vlog="$ROOT/build/$name-virtcon.log"
    rm -f "$vlog"
    CMDLINE="$cmdline" ISO="$iso" "$ROOT/tools/build-image.sh" "${PROFILE:-debug}" >/dev/null
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

# isa-debug-exit: éxito 0x10 -> 33, fallo 0x11 -> 35
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
PROFILE=release run_case bench "bench" 33 "bench: ipc_ida_y_vuelta_ns" "$@"
echo "OK: todos los tests en QEMU pasaron"
