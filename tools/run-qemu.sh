#!/usr/bin/env bash
# Arranca la imagen en QEMU con la consola serie en la terminal.
#
# Uso: tools/run-qemu.sh [--uefi] [args extra de QEMU...]
# Salir: Ctrl-A X
#
# La consola virtio (que maneja la cell `virtcon`) se vuelca en
# $VIRTCON_LOG (por defecto build/virtcon.log).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ISO="${ISO:-$ROOT/build/nsos.iso}"
VIRTCON_LOG="${VIRTCON_LOG:-$ROOT/build/virtcon.log}"

FIRMWARE=()
if [[ "${1:-}" == "--uefi" ]]; then
    shift
    OVMF="${OVMF:-}"
    for c in /usr/share/ovmf/OVMF.fd /usr/share/OVMF/OVMF_CODE.fd /usr/share/qemu/OVMF.fd; do
        [[ -z "$OVMF" && -f "$c" ]] && OVMF="$c"
    done
    [[ -z "$OVMF" ]] && { echo "no se encontró OVMF; definí OVMF=/ruta/OVMF.fd" >&2; exit 1; }
    FIRMWARE=(-bios "$OVMF")
fi

ACCEL=()
[[ -w /dev/kvm ]] && ACCEL=(-enable-kvm -cpu host)

exec qemu-system-x86_64 \
    -M q35 -m 256M -smp 2 \
    "${ACCEL[@]}" "${FIRMWARE[@]}" \
    -cdrom "$ISO" \
    -serial mon:stdio -display none \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -chardev file,id=vc0,path="$VIRTCON_LOG" \
    -device virtio-serial-pci,id=vser0 \
    -device virtconsole,chardev=vc0,bus=vser0.0 \
    -no-reboot \
    "$@"
