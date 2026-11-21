#!/usr/bin/env bash
# Almacenamiento propio en QEMU: driver virtio-blk (`virtblk`), almacén
# (`almacen`) y dos clientes (`escritor`, dueño 1, y `curioso`, dueño 2),
# sobre un disco que sobrevive entre arranques (un archivo).
#
# 1. Disco nuevo: se formatea y el escritor escribe 3 versiones de su objeto
#    (16 KiB). El curioso no ve ese objeto por su raíz y, con una copia de
#    solo lectura, lo lee pero no lo puede escribir.
# 2. Caída simulada: el almacén se "corta" justo antes del superbloque de la
#    segunda escritura. Al rearrancar tiene que estar la versión 4 (la
#    última confirmada), entera.
# 3. Caídas de verdad: se arranca escribiendo sin parar y se mata QEMU
#    (`kill -9`) en un momento al azar, varias veces. Después de cada una,
#    al rearrancar, la versión recuperada tiene que ser la última que el
#    escritor vio confirmada (o la siguiente, si se confirmó justo antes del
#    corte) y estar entera.
# 4. Disco ajeno (prueba negativa): un disco con un sector de arranque FAT.
#    El almacén se niega a usarlo y el archivo queda idéntico.
#
# Uso: tools/store-test.sh [rondas de kill -9, por defecto 3]
#      ARCH=aarch64 tools/store-test.sh [rondas]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROUNDS="${1:-3}"
TIMEOUT="${TIMEOUT:-120}"
# En ARM64 (QEMU `virt`, UEFI) la salida con estado es por semihosting.
EXTRA=""
[[ "${ARCH:-x86_64}" == "aarch64" ]] && EXTRA=" semihosting" && TIMEOUT="${TIMEOUT_ARM:-240}"
DISK="$ROOT/build/almacen-test.img"
mkdir -p "$ROOT/build"

iso() { echo "$ROOT/build/nsos-almacen-${1%%:*}.iso"; }
for mode in escribir:3 caer:2 verificar sin-fin ajeno; do
    CMDLINE="almacen=$mode$EXTRA" ISO="$(iso "$mode")" "$ROOT/tools/build-image.sh" "${PROFILE:-debug}" >/dev/null
done

drive() {
    printf '%s\n' -drive "if=none,id=alm,format=raw,file=$1" \
        -device "virtio-blk-pci,drive=alm"
}

fail() {
    echo "FALLO [almacen]: $*" >&2
    exit 1
}

# boot MODO DISCO LOG: arranca y espera que QEMU salga con éxito (33).
boot() {
    local mode="$1" disk="$2" log="$3" status
    mapfile -t D < <(drive "$disk")
    set +e
    ISO="$(iso "$mode")" timeout "$TIMEOUT" "$ROOT/tools/run-qemu.sh" "${D[@]}" >"$log" 2>&1 </dev/null
    status=$?
    set -e
    if [[ $status -ne 33 ]]; then
        cat "$log"
        fail "modo $mode: QEMU salió con $status (se esperaba 33)"
    fi
}

need() {
    grep -q "$2" "$1" || { cat "$1"; fail "falta \"$2\" en $(basename "$1")"; }
}

recovered() {
    sed -n 's/.*recuperé "contador" en la versión \([0-9]*\): consistente.*/\1/p' "$1" | tail -n 1
}

# 1. Disco nuevo.
rm -f "$DISK"
truncate -s 4M "$DISK"
LOG="$ROOT/build/almacen-1-escribir.log"
boot escribir:3 "$DISK" "$LOG"
need "$LOG" "almacen: disco vacío, formateado"
need "$LOG" "escritor: escribí la versión 3"
need "$LOG" "curioso: mi \"contador\" es mío"
need "$LOG" "curioso: escribir con la copia de solo lectura: rechazado ... ok"
need "$LOG" "escritor: el curioso leyó mi objeto pero no pudo cambiarlo"
# El driver de disco durmió en IRQ_PENDING (hito 12).
need "$LOG" "virtblk: termino ([1-9][0-9]* interrupciones)"
echo "almacen: disco nuevo, 3 versiones escritas; el curioso no pudo tocarlas"

# 2. Caída simulada antes del superbloque.
LOG="$ROOT/build/almacen-2-caer.log"
boot caer:2 "$DISK" "$LOG"
need "$LOG" "escritor: escribí la versión 4"
need "$LOG" "CAÍDA SIMULADA en la escritura 2"
LOG="$ROOT/build/almacen-3-verificar.log"
boot verificar "$DISK" "$LOG"
[[ "$(recovered "$LOG")" == 4 ]] || { cat "$LOG"; fail "después de la caída simulada se esperaba la versión 4"; }
echo "almacen: caída simulada en la versión 5: al rearrancar está la 4, entera"

# 3. kill -9 en momentos al azar.
for i in $(seq "$ROUNDS"); do
    LOG="$ROOT/build/almacen-kill-$i.log"
    mapfile -t D < <(drive "$DISK")
    ISO="$(iso sin-fin)" "$ROOT/tools/run-qemu.sh" "${D[@]}" >"$LOG" 2>&1 </dev/null &
    pid=$!
    for _ in $(seq 600); do
        grep -q "escritor: escribí la versión" "$LOG" 2>/dev/null && break
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
    done
    sleep "0.$(( RANDOM % 10 ))$(( RANDOM % 10 ))"
    sleep "$(( RANDOM % 2 ))"
    # Matar al proceso de QEMU (run-qemu.sh hace exec).
    kill -9 "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    last=$(sed -n 's/.*escritor: escribí la versión \([0-9]*\).*/\1/p' "$LOG" | tail -n 1)
    [[ -n "$last" ]] || { cat "$LOG"; fail "ronda $i: el escritor no llegó a escribir"; }
    VLOG="$ROOT/build/almacen-kill-$i-verificar.log"
    boot verificar "$DISK" "$VLOG"
    got=$(recovered "$VLOG")
    if [[ "$got" != "$last" && "$got" != "$((last + 1))" ]]; then
        cat "$VLOG"
        fail "ronda $i: la última versión confirmada era $last y se recuperó \"$got\""
    fi
    echo "almacen: ronda $i: QEMU muerto con kill -9 después de la versión $last; al rearrancar está la $got, entera"
done

# 4. Disco ajeno: no se toca.
FAT="$ROOT/build/almacen-ajeno.img"
rm -f "$FAT"
truncate -s 4M "$FAT"
# Sin la firma 0x55AA: con ella, el BIOS intentaría arrancar desde ese disco.
printf '\xEB\x58\x90MSDOS5.0' | dd of="$FAT" conv=notrunc status=none
printf 'FAT32   ' | dd of="$FAT" bs=1 seek=82 conv=notrunc status=none
before=$(sha256sum "$FAT" | cut -d' ' -f1)
LOG="$ROOT/build/almacen-ajeno.log"
boot ajeno "$FAT" "$LOG"
need "$LOG" "almacen: el disco tiene otra cosa"
[[ "$(sha256sum "$FAT" | cut -d' ' -f1)" == "$before" ]] || fail "el disco ajeno cambió"
echo "almacen: disco con FAT: no se toca (idéntico byte a byte)"
echo "OK [almacen]"
