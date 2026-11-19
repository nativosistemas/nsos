#!/usr/bin/env bash
# Test de humo: construye la imagen en modo test, arranca en QEMU sin
# pantalla y verifica que el kernel llega a "arranque OK" y sale con éxito.
#
# Uso: tools/smoke-test.sh [--uefi]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ISO="$ROOT/build/nsos-test.iso"
LOG="$ROOT/build/smoke-test.log"
TIMEOUT="${TIMEOUT:-60}"

CMDLINE="test-exit" ISO="$ISO" "$ROOT/tools/build-image.sh" debug

set +e
start=$(date +%s%N)
ISO="$ISO" timeout "$TIMEOUT" "$ROOT/tools/run-qemu.sh" "$@" >"$LOG" 2>&1 </dev/null
status=$?
end=$(date +%s%N)
set -e

cat "$LOG"
echo "---"
echo "tiempo total QEMU (incluye firmware y bootloader): $(( (end - start) / 1000000 )) ms"

# isa-debug-exit: código 0x10 -> estado (0x10<<1)|1 = 33
if [[ $status -ne 33 ]]; then
    echo "FALLO: QEMU salió con estado $status (se esperaba 33)" >&2
    exit 1
fi
if ! grep -q "nsos: arranque OK" "$LOG"; then
    echo "FALLO: no apareció la marca de arranque" >&2
    exit 1
fi
echo "OK: el kernel arrancó y salió limpiamente"
