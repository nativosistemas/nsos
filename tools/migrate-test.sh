#!/usr/bin/env bash
# Migración de una cell entre dos instancias de QEMU por un enlace
# autenticado sobre virtio-net.
#
# La máquina A (`migrar-origen`) corre `contador-v1` y su `cliente`; la B
# (`migrar-destino`) recibe el servicio. Las une una red virtual (un socket
# Unix, `-netdev stream`). Las dos ISO llevan la misma clave compartida
# (`enlace.key`, generada en cada corrida).
#
# Caso normal: las dos tienen que salir con éxito, y el cliente de A tiene
# que haber visto el contador sin saltos aunque al final lo atendió B.
#
# Con --clave-distinta (prueba negativa): B tiene otra clave, como un
# impostor. Ninguna punta puede autenticar a la otra: B no recibe ningún
# estado, no arranca ninguna versión y termina con error; A rechaza las
# migraciones (rollback) y el servicio nunca sale de A.
#
# Uso: tools/migrate-test.sh [--clave-distinta] [--uefi]
#      ARCH=aarch64 tools/migrate-test.sh [--clave-distinta]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TIMEOUT="${TIMEOUT:-120}"
# En ARM64 (QEMU `virt`, UEFI) la salida con estado es por semihosting.
EXTRA=""
[[ "${ARCH:-x86_64}" == "aarch64" ]] && EXTRA=" semihosting" && TIMEOUT="${TIMEOUT_ARM:-240}"
NEGATIVE=0
if [[ "${1:-}" == "--clave-distinta" ]]; then
    NEGATIVE=1
    shift
fi
NAME=migra
[[ $NEGATIVE -eq 1 ]] && NAME=migra-impostor
SOCK="$ROOT/build/$NAME.sock"
mkdir -p "$ROOT/build"

KEY_A="$ROOT/build/$NAME-a.key"
KEY_B="$ROOT/build/$NAME-b.key"
head -c 32 /dev/urandom >"$KEY_A"
if [[ $NEGATIVE -eq 1 ]]; then
    head -c 32 /dev/urandom >"$KEY_B"
else
    cp "$KEY_A" "$KEY_B"
fi
CMDLINE="migrar-origen$EXTRA" LINK_KEY="$KEY_A" ISO="$ROOT/build/nsos-$NAME-origen.iso" \
    "$ROOT/tools/build-image.sh" "${PROFILE:-debug}" >/dev/null
CMDLINE="migrar-destino$EXTRA" LINK_KEY="$KEY_B" ISO="$ROOT/build/nsos-$NAME-destino.iso" \
    "$ROOT/tools/build-image.sh" "${PROFILE:-debug}" >/dev/null
rm -f "$KEY_A" "$KEY_B"

net() {
    printf '%s\n' -netdev "stream,id=n0,server=$1,addr.type=unix,addr.path=$SOCK" \
        -device "virtio-net-pci,netdev=n0,romfile="
}
rm -f "$SOCK"
start=$(date +%s%N)
mapfile -t NET_A < <(net on)
ISO="$ROOT/build/nsos-$NAME-origen.iso" VIRTCON_LOG="$ROOT/build/$NAME-origen-virtcon.log" \
    timeout "$TIMEOUT" "$ROOT/tools/run-qemu.sh" "$@" "${NET_A[@]}" \
    >"$ROOT/build/$NAME-origen.log" 2>&1 </dev/null &
pid_a=$!
for _ in $(seq 100); do
    [[ -S "$SOCK" ]] && break
    sleep 0.1
done
mapfile -t NET_B < <(net off)
ISO="$ROOT/build/nsos-$NAME-destino.iso" VIRTCON_LOG="$ROOT/build/$NAME-destino-virtcon.log" \
    timeout "$TIMEOUT" "$ROOT/tools/run-qemu.sh" "$@" "${NET_B[@]}" \
    >"$ROOT/build/$NAME-destino.log" 2>&1 </dev/null &
pid_b=$!
set +e
wait "$pid_a"; status_a=$?
wait "$pid_b"; status_b=$?
set -e
end=$(date +%s%N)
rm -f "$SOCK"

fail=0
for side in origen destino; do
    echo "=== $NAME-$side ==="
    cat "$ROOT/build/$NAME-$side.log"
done
echo "=== $NAME: QEMU A ${status_a}, QEMU B ${status_b}, $(( (end - start) / 1000000 )) ms ==="
check() {
    grep -q "$2" "$ROOT/build/$NAME-$1.log" || { echo "FALLO [$NAME]: falta \"$2\" en $1" >&2; fail=1; }
}
check_absent() {
    ! grep -q "$2" "$ROOT/build/$NAME-$1.log" || { echo "FALLO [$NAME]: no debería aparecer \"$2\" en $1" >&2; fail=1; }
}
# isa-debug-exit: éxito 0x10 -> 33, fallo 0x11 -> 35
if [[ $NEGATIVE -eq 0 ]]; then
    [[ $status_a -eq 33 ]] || { echo "FALLO [$NAME]: la máquina A salió con $status_a" >&2; fail=1; }
    [[ $status_b -eq 33 ]] || { echo "FALLO [$NAME]: la máquina B salió con $status_b" >&2; fail=1; }
    check origen "enlace: sesión autenticada"
    check destino "enlace: sesión autenticada"
    check origen "migración: OK"
    check origen "cliente: ahora me atiende la versión 2"
    check origen "cliente: .* pedidos sin saltos"
    check origen "origen: contador-roto rechazó; rollback"
    check destino "migración: OK"
    check destino "contador-v2: estado restaurado"
    # El driver de red durmió en IRQ_PENDING (hito 12) en las dos máquinas.
    check origen "virtnet: termino ([1-9][0-9]* interrupciones)"
    check destino "virtnet: termino ([1-9][0-9]* interrupciones)"
else
    [[ $status_a -eq 35 ]] || { echo "FALLO [$NAME]: la máquina A salió con $status_a (se esperaba 35)" >&2; fail=1; }
    [[ $status_b -eq 35 ]] || { echo "FALLO [$NAME]: la máquina B salió con $status_b (se esperaba 35)" >&2; fail=1; }
    check origen "enlace: no se pudo autenticar"
    check destino "enlace: no se pudo autenticar"
    check destino "descarto una trama (etiqueta inválida)"
    check origen "origen: contador-roto rechazó; rollback"
    check_absent origen "ahora me atiende la versión 2"
    check_absent destino "llegó un estado"
    check_absent destino "arranco contador"
fi
[[ $fail -eq 0 ]] && echo "OK [$NAME]"
exit $fail
