# Arquitectura de nsos

nsos es un sistema operativo experimental en Rust que combina ideas de
**Singularity** (aislamiento por lenguaje, microkernel, IPC por canales con
contratos) y **Midori** (pragmatismo, concurrencia async, compilación AOT).
Es un prototipo de investigación para QEMU, no un reemplazo de Linux.

## Capas

```
+-----------------------------------------------------------+
|  cells de aplicación     cells de servicio    supervisor  |   anillo 3
|  (hello, benchmarks)     (consola, fs...)     (init)      |   un espacio de
|                                                           |   direcciones c/u
|  drivers como cells: virtio-console, virtio-blk, ...      |
+------------------------- syscall -------------------------+
|  microkernel: scheduler · memoria · IPC · capacidades     |   anillo 0
|  (+ consola serie de depuración)                          |
+-----------------------------------------------------------+
|  Limine (BIOS/UEFI)  ·  QEMU x86_64 (luego ARM64)         |
+-----------------------------------------------------------+
```

### Kernel (anillo 0)

Solo cuatro responsabilidades:

1. **Scheduling**: hilos de cells, multinúcleo, prioridad simple + handoff en
   `channel_call`.
2. **Memoria**: allocador de marcos físicos, tablas de páginas por cell,
   objetos `Memory`.
3. **IPC**: canales con mensajes de tamaño fijo y transferencia de
   capacidades.
4. **Capacidades**: CSpace por cell, validación de derechos.

Todo lo demás (drivers, sistema de archivos, red, políticas de reinicio) vive
en cells.

### Cells (anillo 3)

Una *cell* es el equivalente a un SIP de Singularity: un proceso aislado,
compilado AOT a código nativo Rust, sin memoria compartida y que solo se
comunica por canales. Usa `no_std` + `alloc` y un runtime pequeño con executor
async sobre la syscall `wait`.

## Aislamiento en dos capas

| Capa | Mecanismo | Qué protege |
|---|---|---|
| Lenguaje | Rust seguro, `#![forbid(unsafe_code)]` en cells de usuario | errores de memoria dentro de la cell |
| Hardware | espacio de direcciones propio por cell (MMU), anillo 3 | cells maliciosas, `unsafe` mal usado, Spectre entre cells |

Singularity confiaba solo en el lenguaje (SIPs en un único espacio de
direcciones). Después de Spectre eso no alcanza, por eso nsos mantiene la MMU
como barrera obligatoria y usa el lenguaje como defensa adicional y como
herramienta de productividad.

## Arranque (hito 1)

1. El firmware (BIOS o UEFI) carga **Limine** desde la ISO.
2. Limine carga el ELF del kernel en la mitad alta
   (`0xffffffff80000000`), arma un mapeo directo de la memoria física (HHDM)
   y rellena las *requests* del kernel (`kernel/src/boot/limine.rs`).
3. `kmain` inicializa COM1, valida la revisión del protocolo, imprime el mapa
   de memoria y escribe `nsos: arranque OK`.
4. Con `test-exit` en la línea de comandos, sale de QEMU por
   `isa-debug-exit` con código de éxito (usado por los tests de humo y la CI).

## Organización del repositorio

```
crates/abi/        ABI kernel↔cells (no_std, sin unsafe, testeable en host)
kernel/            microkernel (x86_64-unknown-none)
  src/arch/        código dependiente de arquitectura
  src/boot/        protocolo Limine
docs/              especificaciones y documentación
tools/             construcción de imagen, QEMU, tests de humo
```

Más adelante: `cells/` (runtime y cells de ejemplo), `tests/` (aislamiento),
`bench/` (IPC y arranque).

## Decisiones

* **Limine** en vez de un loader UEFI propio: arranca en BIOS y UEFI, entrega
  memoria en mitad alta y mapa de memoria, y deja más tiempo para el kernel.
  Se puede reemplazar sin tocar el resto (solo `boot/`).
* **Rust estable**: sin features nightly. Por eso los bindings de Limine son
  propios (la crate oficial 0.6 exige nightly).
* **Mensajes de tamaño fijo (256 B)**: validación total, colas sin
  fragmentación, copia acotada. Lo grande va por `Memory` transferida.
* **Capacidades que se mueven**: evitan memoria compartida y hacen natural la
  actualización en vivo (los extremos de canal cambian de dueño).
