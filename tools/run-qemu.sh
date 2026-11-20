#!/usr/bin/env bash
# Arranca la imagen en QEMU con la consola serie en la terminal.
#
# Uso: tools/run-qemu.sh [--uefi] [args extra de QEMU...]
#      ARCH=aarch64 tools/run-qemu.sh [args extra de QEMU...]   (siempre UEFI)
# Salir: Ctrl-A X
#
# La consola virtio (que maneja la cell `virtcon`) se vuelca en
# $VIRTCON_LOG (por defecto build/virtcon.log).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ "${ARCH:-x86_64}" == "aarch64" ]]; then
    ISO="${ISO:-$ROOT/build/nsos-aarch64.img}"
    [[ "${1:-}" == "--uefi" ]] && shift
    AAVMF="${AAVMF:-}"
    for c in /usr/share/qemu-efi-aarch64/QEMU_EFI.fd /usr/share/AAVMF/AAVMF_CODE.fd \
             /usr/share/edk2/aarch64/QEMU_EFI.fd; do
        [[ -z "$AAVMF" && -f "$c" ]] && AAVMF="$c"
    done
    [[ -z "$AAVMF" ]] && { echo "no se encontró QEMU_EFI.fd; definí AAVMF=/ruta" >&2; exit 1; }
    # Cortex-A76 (la CPU de la Pi 5, ARMv8.2 con PAN) por defecto: con
    # `-cpu max` el firmware UEFI de Ubuntu 2025.11 se cuelga antes de
    # cargar Limine. ARM_CPU=cortex-a72 emula la Pi 4 (sin PAN).
    # Semihosting para salir con un estado.
    # La imagen de arranque va en un disco NVMe: así el único virtio-blk es
    # el del almacén (si se agrega uno), como en una Pi que arranca de la SD.
    VIRTCON_LOG="${VIRTCON_LOG:-$ROOT/build/virtcon.log}"
    exec qemu-system-aarch64 \
        -M virt,gic-version=2,acpi=off -cpu "${ARM_CPU:-cortex-a76}" -m 256M \
        -bios "$AAVMF" \
        -drive if=none,format=raw,file="$ISO",id=hd0 \
        -device nvme,drive=hd0,serial=nsos-boot \
        -serial mon:stdio -display none \
        -chardev file,id=vc0,path="$VIRTCON_LOG" \
        -device virtio-serial-pci,id=vser0 \
        -device virtconsole,chardev=vc0,bus=vser0.0 \
        -semihosting-config enable=on,target=native \
        -no-reboot \
        "$@"
fi

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

# Sin KVM, `-cpu max` emula todas las extensiones (SMEP, SMAP, ...).
ACCEL=(-cpu max)
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
