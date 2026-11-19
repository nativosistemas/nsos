# nsos

Sistema operativo experimental en Rust: un microkernel con *cells* aisladas
que se comunican por IPC basado en capacidades. Toma ideas de Singularity
(aislamiento por lenguaje, SIPs, canales) y de Midori (async, AOT,
pragmatismo), con la MMU como segunda barrera. Es un prototipo para
investigar en QEMU, no un reemplazo de Linux.

## Estado

Hito 1: el kernel arranca en QEMU x86_64 (BIOS y UEFI) con Limine e imprime
por el puerto serie. El ABI de IPC y capacidades está especificado e
implementado como crate testeable. Ver [hoja de ruta](docs/hoja-de-ruta.md).

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
make smoke       # arranca en QEMU y verifica la salida (lo mismo que la CI)
make lint        # rustfmt + clippy
```

Salida esperada:

```
nsos 0.1.0 (ABI v1)
bootloader: Limine 10.x
...
memoria utilizable: 253 MiB (+1 MiB recuperable del bootloader)
nsos: arranque OK
```

## Documentación

* [Arquitectura](docs/arquitectura.md)
* [ABI de IPC y modelo de capacidades](docs/abi-ipc-capacidades.md)
* [Hoja de ruta](docs/hoja-de-ruta.md)
* [Registro de `unsafe`](docs/unsafe.md)
