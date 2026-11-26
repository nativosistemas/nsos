#!/usr/bin/env bash
# Auditoría de `unsafe` (corre en la CI).
#
# Comprueba que:
#   1. todo archivo con `unsafe` (bloque, fn, impl o extern) está en la tabla
#      de docs/unsafe.md, y que la tabla no nombra archivos que ya no lo usan;
#   2. crates/abi, crates/kcore, crates/contract, crates/link y crates/store
#      siguen con #![forbid(unsafe_code)];
#   3. toda cell (salvo el runtime) declara #![deny(unsafe_code)].
# Imprime la cantidad de usos por archivo para seguir su evolución en los PRs.
#
# Uso: tools/audit-unsafe.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
fail=0

# Usos reales: se ignoran comentarios y los nombres de los lints.
uses() {
    grep -nE '\bunsafe[[:space:]]*(\{|fn\b|impl\b|extern\b)' "$1" \
        | grep -vE '^[0-9]+:[[:space:]]*//' || true
}

mapfile -t files < <(find kernel/src cells crates -name '*.rs' -not -path '*/target/*' | sort)
mapfile -t registered < <(grep -oE '^\| `[^`]+\.rs`' docs/unsafe.md | sed -E 's/^\| `([^`]+)`/\1/' | sort -u)

declare -A in_table=()
for f in "${registered[@]}"; do in_table[$f]=1; done

echo "| Archivo | Usos de unsafe |"
echo "|---|---:|"
total=0
declare -A seen=()
for f in "${files[@]}"; do
    n=$(uses "$f" | wc -l)
    [[ $n -eq 0 ]] && continue
    seen[$f]=1
    total=$((total + n))
    echo "| \`$f\` | $n |"
    if [[ -z "${in_table[$f]:-}" ]]; then
        echo "FALLO: $f usa unsafe y no está en docs/unsafe.md" >&2
        fail=1
    fi
done
echo "| **total** | **$total** |"

for f in "${registered[@]}"; do
    if [[ -z "${seen[$f]:-}" ]]; then
        echo "FALLO: docs/unsafe.md nombra $f, que ya no usa unsafe" >&2
        fail=1
    fi
done

for c in crates/{abi,kcore,contract,link,store}/src/lib.rs; do
    if ! grep -q '^#!\[forbid(unsafe_code)\]' "$c"; then
        echo "FALLO: $c perdió #![forbid(unsafe_code)]" >&2
        fail=1
    fi
done

for m in cells/*/src/main.rs; do
    if ! grep -q '^#!\[deny(unsafe_code)\]' "$m"; then
        echo "FALLO: $m no declara #![deny(unsafe_code)]" >&2
        fail=1
    fi
done

if [[ $fail -ne 0 ]]; then
    exit 1
fi
echo
echo "OK: todo unsafe está registrado en docs/unsafe.md"
