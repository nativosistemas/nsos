# nsos

Sistema operativo experimental en Rust: un microkernel con *cells* aisladas
que se comunican por IPC basado en capacidades. Toma ideas de Singularity
(aislamiento por lenguaje, SIPs, canales) y de Midori (async, AOT,
pragmatismo), con la MMU como segunda barrera. Es un prototipo para
investigar en QEMU, no un reemplazo de Linux.

> **In English.** nsos is an experimental microkernel written in Rust.
> Programs run as isolated *cells* in user mode, each with its own address
> space and capability table, and talk only through capability-based IPC
> channels. It draws on Singularity and Midori, with the MMU as a second
> barrier. It boots on QEMU (x86_64 and ARM64) and has virtio drivers as
> cells, live update with rollback, migration between machines and its own
> copy-on-write object store. Isolation properties are verified: the design
> is modelled and proved in Lean, and critical kernel code is checked with
> Kani. Documentation and commit messages are in Spanish; identifiers are
> in English. Contributions are welcome: see `CONTRIBUTING.md`.

## Estado

El kernel arranca en QEMU x86_64 (BIOS y UEFI) con Limine, tiene tablas de
páginas propias (W^X), heap, APIC timer y scheduler apropiativo. Corre
**cells en anillo 3**, cada una con su espacio de direcciones y su tabla de
capacidades, que se comunican por canales IPC y se pasan capacidades. Una
cell que viola el aislamiento es terminada sin afectar al kernel. Una cell
puede dormir sobre varios canales y cells a la vez con `wait`. Hay
drivers como cells (virtio) que duermen hasta su interrupción, actualización en vivo con rollback y migración
de un servicio entre dos QEMU, y un **almacenamiento propio** (objetos con
versión, copy-on-write, acceso por capacidades; ver
[docs/almacenamiento.md](docs/almacenamiento.md)). También corre en **ARM64** (QEMU `virt`, con device tree y PCI:
los mismos drivers, almacén y migración; ver [docs/arm64.md](docs/arm64.md)). Ver [hoja de ruta](docs/hoja-de-ruta.md),
[modelo de amenazas](docs/amenazas.md) y [cómo contribuir](CONTRIBUTING.md)
(incluye la regla de verificación formal).

## Requisitos

* Rust estable, con la versión fija en `rust-toolchain.toml` (incluye el target
  `x86_64-unknown-none`).
* `qemu-system-x86_64`, `xorriso`, `make`, `git`, un compilador C (para la
  herramienta `limine`, que se descarga sola la primera vez, en un commit fijo).
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
[ping] ping: 4 mensajes fuera de protocolo: pong los descartó, soltó la capacidad y sigue en Ready
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
* [Contratos de canal tipados](docs/contratos.md)
* [Hoja de ruta](docs/hoja-de-ruta.md)
* [Almacenamiento propio](docs/almacenamiento.md)
* [Registro de `unsafe`](docs/unsafe.md)
* [Verificación formal en Lean](verification/lean/README.md)
* [Pruebas de Kani sobre el código Rust](verification/kani.md)

## Licencia

Se distribuye bajo cualquiera de estas dos licencias, a elección de quien lo
use:

* [Apache License, Version 2.0](LICENSE-APACHE)
* [Licencia MIT](LICENSE-MIT)

Salvo que se indique lo contrario, toda contribución enviada para incluirse en
nsos, según se define en la licencia Apache-2.0, queda bajo esas mismas dos
licencias, sin términos ni condiciones adicionales.
