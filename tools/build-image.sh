#!/usr/bin/env bash
# Construye una imagen booteable con Limine + kernel nsos.
#
# x86_64: ISO híbrida (BIOS y UEFI). aarch64: imagen de disco con una
# partición FAT (ESP) para UEFI (QEMU `virt`, y luego Raspberry Pi con
# firmware UEFI).
#
# Uso: tools/build-image.sh [debug|release]
# Variables:
#   ARCH      x86_64 (por defecto) o aarch64
#   CMDLINE   línea de comandos del kernel (por defecto vacía)
#   ISO       ruta de salida (por defecto build/nsos.iso o build/nsos-aarch64.img)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROFILE="${1:-debug}"
ARCH="${ARCH:-x86_64}"
CMDLINE="${CMDLINE:-}"
# Limine es de confianza (docs/amenazas.md): se fija un commit de la rama de
# binarios y se comprueba antes de usarlo. Actualizar en un PR aparte.
LIMINE_URL="https://github.com/limine-bootloader/limine.git"
LIMINE_COMMIT="7a9013f305de0bea1f6310e76e7baba30499fef0" # v10.8.5, rama v10.x-binary
LIMINE_DIR="$ROOT/build/limine"

case "$ARCH" in
    x86_64)
        TARGET=x86_64-unknown-none
        ISO="${ISO:-$ROOT/build/nsos.iso}"
        ;;
    aarch64)
        TARGET=aarch64-unknown-none-softfloat
        ISO="${ISO:-$ROOT/build/nsos-aarch64.img}"
        ;;
    *)
        echo "ARCH desconocida: $ARCH (x86_64 o aarch64)" >&2
        exit 1
        ;;
esac

cd "$ROOT"

# Cells: cada carpeta de `cells/` menos el runtime `rt`.
CELLS=()
for d in cells/*/; do
    c=$(basename "$d")
    [[ "$c" == rt ]] || CELLS+=("$c")
done

# 1. Kernel
if [[ "$PROFILE" == "release" ]]; then
    cargo build -p nsos-kernel --target "$TARGET" --release
else
    cargo build -p nsos-kernel --target "$TARGET"
fi
KERNEL="$ROOT/target/$TARGET/$PROFILE/nsos-kernel"

# 1b. Cells: siempre en release (son chicas y así el benchmark es realista).
# Se compilan aparte, con sus propios RUSTFLAGS (sin el modelo de código del
# kernel) y su propio directorio de salida.
CELL_PKGS=()
for c in "${CELLS[@]}"; do CELL_PKGS+=(-p "cell-$c"); done
RUSTFLAGS="-C relocation-model=static" cargo build --release \
    --target "$TARGET" --target-dir "$ROOT/target/cells" "${CELL_PKGS[@]}"
CELL_DIR="$ROOT/target/cells/$TARGET/release"

# 2. Limine (binarios precompilados + herramienta del host)
if [[ -d "$LIMINE_DIR" ]] && \
    [[ "$(git -C "$LIMINE_DIR" rev-parse HEAD 2>/dev/null)" != "$LIMINE_COMMIT" ]]; then
    echo "Limine en $LIMINE_DIR no es $LIMINE_COMMIT: se vuelve a descargar" >&2
    rm -rf "$LIMINE_DIR"
fi
if [[ ! -d "$LIMINE_DIR" ]]; then
    git init --quiet "$LIMINE_DIR"
    git -C "$LIMINE_DIR" fetch --quiet --depth=1 "$LIMINE_URL" "$LIMINE_COMMIT"
    git -C "$LIMINE_DIR" checkout --quiet --detach FETCH_HEAD
fi
if [[ "$(git -C "$LIMINE_DIR" rev-parse HEAD)" != "$LIMINE_COMMIT" ]] || \
    [[ -n "$(git -C "$LIMINE_DIR" status --porcelain --untracked-files=no)" ]]; then
    echo "FALLO: Limine no coincide con el commit fijado $LIMINE_COMMIT" >&2
    exit 1
fi
if [[ ! -x "$LIMINE_DIR/limine" ]]; then
    make -s -C "$LIMINE_DIR" >/dev/null
fi

# 3. Árbol de la imagen
ISO_ROOT="$ROOT/build/iso_root-$ARCH"
rm -rf "$ISO_ROOT"
mkdir -p "$ISO_ROOT/boot/limine" "$ISO_ROOT/EFI/BOOT"
cp "$KERNEL" "$ISO_ROOT/boot/nsos-kernel"
mkdir -p "$ISO_ROOT/boot/cells"
MODULES=""
for c in "${CELLS[@]}"; do
    cp "$CELL_DIR/$c" "$ISO_ROOT/boot/cells/$c"
    MODULES+="    module_path: boot():/boot/cells/$c"$'\n'
done
# Clave compartida del enlace entre máquinas (32 bytes), si se pide. La usa
# el kernel para configurar la cell `enlace` (ver tools/migrate-test.sh).
if [[ -n "${LINK_KEY:-}" ]]; then
    cp "$LINK_KEY" "$ISO_ROOT/boot/enlace.key"
    MODULES+="    module_path: boot():/boot/enlace.key"$'\n'
fi
cat > "$ISO_ROOT/boot/limine/limine.conf" <<EOF
timeout: 0
serial: yes

/nsos
    protocol: limine
    path: boot():/boot/nsos-kernel
    cmdline: $CMDLINE
$MODULES
EOF
if [[ "$ARCH" == "aarch64" ]]; then
    # 4. Imagen FAT de 64 MiB (ESP) con el árbol entero.
    cp "$LIMINE_DIR"/BOOTAA64.EFI "$ISO_ROOT/EFI/BOOT/"
    mkdir -p "$(dirname "$ISO")"
    rm -f "$ISO"
    truncate -s 64M "$ISO"
    mkfs.fat -F 32 -n NSOS "$ISO" >/dev/null
    mcopy -s -i "$ISO" "$ISO_ROOT"/* ::/
    echo "imagen: $ISO"
    exit 0
fi

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
