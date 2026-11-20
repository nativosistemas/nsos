# nsos

Sistema operativo experimental en Rust: un microkernel con *cells* aisladas
que se comunican por IPC basado en capacidades. Toma ideas de Singularity
(aislamiento por lenguaje, SIPs, canales) y de Midori (async, AOT,
pragmatismo), con la MMU como segunda barrera. Es un prototipo para
investigar en QEMU, no un reemplazo de Linux.

## Estado

El kernel arranca en QEMU x86_64 (BIOS y UEFI) con Limine, tiene tablas de
páginas propias (W^X), heap, APIC timer y scheduler apropiativo. Corre
**cells en anillo 3**, cada una con su espacio de direcciones y su tabla de
capacidades, que se comunican por canales IPC y se pasan capacidades. Una
cell que viola el aislamiento es terminada sin afectar al kernel. Hay
drivers como cells (virtio), actualización en vivo con rollback y migración
de un servicio entre dos QEMU, y un **almacenamiento propio** (objetos con
versión, copy-on-write, acceso por capacidades; ver
[docs/almacenamiento.md](docs/almacenamiento.md)). También corre en **ARM64** (QEMU `virt`,
ver [docs/arm64.md](docs/arm64.md)). Ver [hoja de ruta](docs/hoja-de-ruta.md),
[modelo de amenazas](docs/amenazas.md) y [cómo contribuir](CONTRIBUTING.md)
(incluye la regla de verificación formal).

## Requisitos

* Rust estable (lo fija `rust-toolchain.toml`, incluye el target
  `x86_64-unknown-none`).
* `qemu-system-x86_64`, `xorriso`, `make`, `git`, un compilador C (para la
  herramienta `limine`, que se descarga sola la primera vez).
* Opcional: OVMF para arrancar por UEFI.
* Para ARM64: `qemu-system-aarch64`, el firmware UEFI de QEMU
  (`qemu-efi-aarch64`), `mtools` y `dosfstools`.

En Debian/Ubuntu: `sudo apt install qemu-system-x86 xorriso ovmf build-essential`.

## Uso

```sh
make run         # construye la ISO y arranca en QEMU (salir: Ctrl-A X)
make run-uefi    # ídem con firmware UEFI
make test        # tests unitarios en el host
make smoke       # arranca en QEMU, corre los tests internos y un test de fallo (como la CI)
make run-arm     # ARM64: QEMU virt con UEFI
make smoke-arm   # ARM64: tests en QEMU virt (como la CI)
make lint        # rustfmt + clippy
make verify      # demostraciones en Lean + tests derivados del modelo (requiere elan)
make kani        # pruebas de Kani sobre abi, kcore, link y store (requiere kani-verifier)
make smoke-almacen  # almacenamiento: caídas simuladas, kill -9 a QEMU y un disco FAT
make run-almacen    # almacenamiento a mano: escribe sin parar; cortá con Ctrl-A X
make run-almacen MODO=verificar  # ...y al rearrancar muestra qué versión quedó
```

Salida esperada:

```
nsos 0.1.0 (ABI v1)
bootloader: Limine 10.8.5
cpu: GDT, TSS e IDT cargadas
mm: 244 MiB libres de 255 MiB gestionados (...); tablas de páginas propias, W^X
mm: heap del kernel de 8192 KiB
apic: timer a 100 Hz, TSC ~2150 MHz
arranque del kernel: 16 ms (desde kmain, build release)
nsos: arranque OK
```

Después arrancan las cells de ejemplo:

```
[ping] ping: 10 ecos verificados
[ping] ping: transferí un extremo; mi handle viejo ya es inválido
[pong] pong: recibí por el canal transferido: "hola por el canal nuevo"
[intruso] intruso: debug_log con puntero al kernel   -> Err(InvalidArgs) (bloqueado)
cell "intruso": excepción 14 ... el kernel la termina
```

Con `memmap` en la línea de comandos (`CMDLINE="memmap" make run`) imprime
además el mapa de memoria de Limine.

## Documentación

* [Arquitectura](docs/arquitectura.md)
* [ABI de IPC y modelo de capacidades](docs/abi-ipc-capacidades.md)
* [Hoja de ruta](docs/hoja-de-ruta.md)
* [Almacenamiento propio](docs/almacenamiento.md)
* [Registro de `unsafe`](docs/unsafe.md)
* [Verificación formal en Lean](verification/lean/README.md)
* [Pruebas de Kani sobre el código Rust](verification/kani.md)
