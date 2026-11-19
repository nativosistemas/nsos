#!/usr/bin/env bash
# Hito 10: migración de una cell entre dos instancias de QEMU.
#
# La máquina A (`migrar-origen`) corre `contador-v1` y su `cliente`; la B
# (`migrar-destino`) recibe el servicio. Las une el segundo puerto serie
# (COM2) de cada una, conectado por un socket Unix: A lo abre como servidor
# y espera a que B se conecte antes de arrancar.
#
# Las dos tienen que salir con éxito, y el cliente de A tiene que haber
# visto el contador sin saltos aunque al final lo atendió B.
#
# Uso: tools/migrate-test.sh
#
# Solo con BIOS: OVMF usa COM2 como consola y los dos firmwares se
# mandarían su salida (ver tools/smoke-test.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TIMEOUT="${TIMEOUT:-90}"
SOCK="$ROOT/build/migra.sock"
mkdir -p "$ROOT/build"

for side in origen destino; do
    CMDLINE="migrar-$side" ISO="$ROOT/build/nsos-migra-$side.iso" \
        "$ROOT/tools/build-image.sh" "${PROFILE:-debug}" >/dev/null
done

rm -f "$SOCK"
start=$(date +%s%N)
ISO="$ROOT/build/nsos-migra-origen.iso" VIRTCON_LOG="$ROOT/build/migra-origen-virtcon.log" \
    timeout "$TIMEOUT" "$ROOT/tools/run-qemu.sh" "$@" -serial "unix:$SOCK,server=on,wait=on" \
    >"$ROOT/build/migra-origen.log" 2>&1 </dev/null &
pid_a=$!
for _ in $(seq 100); do
    [[ -S "$SOCK" ]] && break
    sleep 0.1
done
ISO="$ROOT/build/nsos-migra-destino.iso" VIRTCON_LOG="$ROOT/build/migra-destino-virtcon.log" \
    timeout "$TIMEOUT" "$ROOT/tools/run-qemu.sh" "$@" -serial "unix:$SOCK" \
    >"$ROOT/build/migra-destino.log" 2>&1 </dev/null &
pid_b=$!
set +e
wait "$pid_a"; status_a=$?
wait "$pid_b"; status_b=$?
set -e
end=$(date +%s%N)
rm -f "$SOCK"

fail=0
for side in origen destino; do
    echo "=== migra-$side ==="
    cat "$ROOT/build/migra-$side.log"
done
echo "=== migración: QEMU A ${status_a}, QEMU B ${status_b}, $(( (end - start) / 1000000 )) ms ==="
# isa-debug-exit: éxito 0x10 -> 33
[[ $status_a -eq 33 ]] || { echo "FALLO [migra]: la máquina A salió con $status_a" >&2; fail=1; }
[[ $status_b -eq 33 ]] || { echo "FALLO [migra]: la máquina B salió con $status_b" >&2; fail=1; }
check() {
    grep -q "$2" "$ROOT/build/migra-$1.log" || { echo "FALLO [migra]: falta \"$2\" en $1" >&2; fail=1; }
}
check origen "migración: OK"
check origen "cliente: ahora me atiende la versión 2"
check origen "cliente: .* pedidos sin saltos"
check origen "origen: contador-roto rechazó; rollback"
check destino "migración: OK"
check destino "contador-v2: estado restaurado"
[[ $fail -eq 0 ]] && echo "OK [migra]"
exit $fail
