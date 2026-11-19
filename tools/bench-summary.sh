#!/usr/bin/env bash
# Extrae las líneas "bench: nombre = valor" de un log de QEMU y las muestra
# como tabla Markdown (en la CI se agrega al resumen del job).
#
# Uso: tools/bench-summary.sh build/boot.log [título]
set -euo pipefail

LOG="${1:?uso: tools/bench-summary.sh LOG [título]}"
TITLE="${2:-Benchmarks}"

echo "### $TITLE"
echo
echo "| Medida | Valor | Unidad |"
echo "|---|---:|---|"
grep -ao 'bench: [a-z_]* = [0-9]*' "$LOG" | while read -r _ name _ value; do
    unit="${name##*_}"
    echo "| ${name%_*} | $value | $unit |"
done
echo
echo "QEMU sin KVM (TCG) en la CI: sirve para comparar entre PRs, no como valor absoluto."
