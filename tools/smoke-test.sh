#!/usr/bin/env bash
# Tests en QEMU:
#   1. arranque + tests internos del kernel (ktest), debe salir con éxito;
#   2. test negativo: una escritura en .rodata debe terminar en #PF y pánico
#      controlado (comprueba W^X y la ruta de excepciones).
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
    CMDLINE="$cmdline" ISO="$iso" "$ROOT/tools/build-image.sh" debug >/dev/null
    set +e
    local start end status
    start=$(date +%s%N)
    ISO="$iso" timeout "$TIMEOUT" "$ROOT/tools/run-qemu.sh" "$@" >"$log" 2>&1 </dev/null
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
    echo "OK [$name]"
}

# isa-debug-exit: éxito 0x10 -> 33, fallo 0x11 -> 35
run_case boot "test-exit" 33 "ktest: .* 0 fallos" "$@"
run_case wx-fault "test-wx-fault" 35 "escritura sin permiso" "$@"
echo "OK: todos los tests en QEMU pasaron"
