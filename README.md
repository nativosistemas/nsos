# nsos

Sistema operativo experimental en Rust: un microkernel con *cells* aisladas
que se comunican por IPC basado en capacidades. Toma ideas de Singularity
(aislamiento por lenguaje, SIPs, canales) y de Midori (async, AOT,
pragmatismo), con la MMU como segunda barrera. Es un prototipo para
investigar en QEMU, no un reemplazo de Linux.

## Estado

El kernel arranca en QEMU x86_64 (BIOS y UEFI) con Limine, construye sus
propias tablas de páginas (W^X), tiene heap, APIC timer y un scheduler
apropiativo de hilos de kernel. El ABI de IPC y capacidades está
especificado e implementado como crate testeable. Siguiente: cells en anillo
3 e IPC. Ver [hoja de ruta](docs/hoja-de-ruta.md).

## Requisitos

* Rust estable (lo fija `rust-toolchain.toml`, incluye el target
  `x86_64-unknown-none`).
* `qemu-system-x86_64`, `xorriso`, `make`, `git`, un compilador C (para la
  herramienta `limine`, que se descarga sola la primera vez).
* Opcional: OVMF para arrancar por UEFI.

En Debian/Ubuntu: `sudo apt install qemu-system-x86 xorriso ovmf build-essential`.

## Uso

```sh
make run         # construye la ISO y arranca en QEMU (salir: Ctrl-A X)
make run-uefi    # ídem con firmware UEFI
make test        # tests unitarios en el host
make smoke       # arranca en QEMU, corre los tests internos y un test de fallo (como la CI)
make lint        # rustfmt + clippy
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

Con `memmap` en la línea de comandos (`CMDLINE="memmap" make run`) imprime
además el mapa de memoria de Limine.

## Documentación

* [Arquitectura](docs/arquitectura.md)
* [ABI de IPC y modelo de capacidades](docs/abi-ipc-capacidades.md)
* [Hoja de ruta](docs/hoja-de-ruta.md)
* [Registro de `unsafe`](docs/unsafe.md)
