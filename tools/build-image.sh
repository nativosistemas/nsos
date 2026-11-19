#!/usr/bin/env bash
# Construye una imagen ISO booteable (BIOS y UEFI) con Limine + kernel nsos.
#
# Uso: tools/build-image.sh [debug|release]
# Variables:
#   CMDLINE   línea de comandos del kernel (por defecto vacía)
#   ISO       ruta de salida (por defecto build/nsos.iso)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROFILE="${1:-debug}"
CMDLINE="${CMDLINE:-}"
ISO="${ISO:-$ROOT/build/nsos.iso}"
LIMINE_BRANCH="v10.x-binary"
LIMINE_DIR="$ROOT/build/limine"

cd "$ROOT"

# 1. Kernel
if [[ "$PROFILE" == "release" ]]; then
    cargo build -p nsos-kernel --target x86_64-unknown-none --release
else
    cargo build -p nsos-kernel --target x86_64-unknown-none
fi
KERNEL="$ROOT/target/x86_64-unknown-none/$PROFILE/nsos-kernel"

# 1b. Cells: siempre en release (son chicas y así el benchmark es realista).
# Se compilan aparte, con sus propios RUSTFLAGS (sin el modelo de código del
# kernel) y su propio directorio de salida.
CELLS=(ping pong intruso virtcon hola init fragil dormilona contador-v1 contador-v2 cliente)
CELL_PKGS=()
for c in "${CELLS[@]}"; do CELL_PKGS+=(-p "cell-$c"); done
RUSTFLAGS="-C relocation-model=static" cargo build --release \
    --target x86_64-unknown-none --target-dir "$ROOT/target/cells" "${CELL_PKGS[@]}"
CELL_DIR="$ROOT/target/cells/x86_64-unknown-none/release"

# 2. Limine (binarios precompilados + herramienta del host)
if [[ ! -d "$LIMINE_DIR" ]]; then
    git clone --quiet --depth=1 --branch="$LIMINE_BRANCH" \
        https://github.com/limine-bootloader/limine.git "$LIMINE_DIR"
fi
if [[ ! -x "$LIMINE_DIR/limine" ]]; then
    make -s -C "$LIMINE_DIR" >/dev/null
fi

# 3. Árbol de la ISO
ISO_ROOT="$ROOT/build/iso_root"
rm -rf "$ISO_ROOT"
mkdir -p "$ISO_ROOT/boot/limine" "$ISO_ROOT/EFI/BOOT"
cp "$KERNEL" "$ISO_ROOT/boot/nsos-kernel"
mkdir -p "$ISO_ROOT/boot/cells"
MODULES=""
for c in "${CELLS[@]}"; do
    cp "$CELL_DIR/$c" "$ISO_ROOT/boot/cells/$c"
    MODULES+="    module_path: boot():/boot/cells/$c"$'\n'
done
cat > "$ISO_ROOT/boot/limine/limine.conf" <<EOF
timeout: 0
serial: yes

/nsos
    protocol: limine
    path: boot():/boot/nsos-kernel
    cmdline: $CMDLINE
$MODULES
EOF
cp "$LIMINE_DIR"/limine-bios.sys "$LIMINE_DIR"/limine-bios-cd.bin \
   "$LIMINE_DIR"/limine-uefi-cd.bin "$ISO_ROOT/boot/limine/"
cp "$LIMINE_DIR"/BOOTX64.EFI "$ISO_ROOT/EFI/BOOT/"

# 4. ISO híbrida
mkdir -p "$(dirname "$ISO")"
xorriso -as mkisofs -R -r -J \
    -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
    -hfsplus -apm-block-size 2048 \
    --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image \
    --protective-msdos-label \
    "$ISO_ROOT" -o "$ISO" 2>/dev/null
"$LIMINE_DIR/limine" bios-install "$ISO" 2>/dev/null

echo "imagen: $ISO"
