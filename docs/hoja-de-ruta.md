# Hoja de ruta

Cada hito se entrega como un PR con tests y documentación.

| # | Hito | Estado |
|---|---|---|
| 1 | Especificar ABI de IPC y modelo de capacidades | ✅ `docs/abi-ipc-capacidades.md`, `crates/abi` |
| 2 | Imagen booteable mínima: Limine + kernel que imprime por serial (BIOS y UEFI) | ✅ |
| 3 | Allocador físico/virtual, paginación, GDT/IDT, scheduler básico | ✅ 9 tests en QEMU + 7 en host |
| 4 | IPC seguro (canales + transferencia de capacidades) y creación de cells | ✅ ping/pong/intruso en anillo 3 |
| 5 | "hello cell" y driver virtio de consola como cell | ✅ `virtcon` (driver) + `hola` (cliente) |
| 6 | Fault containment: matar/reiniciar una cell sin tumbar el kernel | ✅ supervisor `init` que reinicia `fragil` y mata `dormilona` |
| 7 | Actualización en vivo de una cell | ✅ `contador-v1` → `contador-v2` sin cortar al `cliente`, con rollback |
| 8 | Tests de aislamiento y benchmark de IPC | ✅ `intruso` (27 abusos), fuzzer `caos`, `bench` con resumen en la CI |
| 9 | Documentar límites de `unsafe` y modelo de amenazas | ✅ `docs/amenazas.md`, auditoría `tools/audit-unsafe.sh`, SMEP/SMAP |
| 10 | Fase 2: migración de cells entre dos instancias QEMU | ✅ `contador` se muda de A a B sin cortar al `cliente`, con rollback (ahora por un enlace autenticado sobre virtio-net) |
| 11 | Almacenamiento propio: driver virtio-blk y almacén de objetos con copy-on-write y capacidades | ✅ `virtblk`, `almacen`, `crates/store`; ver [almacenamiento.md](almacenamiento.md) |
| 12 | `wait` multi-handle e interrupciones para drivers | ✅ `wait` (modelo `Wait.lean`, `espera`), `Irq`/`irq_ack` (modelo `Irq.lean`); `virtcon`, `virtblk` y `virtnet` duermen en `IRQ_PENDING` |
| 13 | `channel_call` y `mem_unmap`: el ABI v1 completo | ✅ `Sys.call_inv`, `Mem.unmap_removes`; `cliente` y `bench` usan `channel_call`; `desmapeo` |

## Criterios de aceptación del MVP

- [x] Arranca en QEMU x86_64.
- [x] Kernel mínimo sin GC.
- [x] Al menos 3 cells aisladas comunicándose por IPC (`init` con `fragil`, `ping`/`pong`, `hola`/`virtcon`).
- [x] Un driver en user-space (`virtcon`).
- [x] Un fallo de cell no afecta al kernel (la cell se termina, se libera su memoria y un supervisor puede reiniciarla).
- [x] Actualización en vivo de una cell sin reinicio (protocolo de supervisor con rollback, modelado en Lean).
- [x] `unsafe` confinado y documentado (lint obligatorio, registro en `docs/unsafe.md` y auditoría automática en la CI).
- [x] Benchmarks básicos de IPC y arranque (cell `bench` en una corrida aparte; la CI publica la tabla en el resumen de cada PR).

## Próximos hitos

En orden de prioridad según el [modelo de amenazas](amenazas.md). El detalle
de lo ya hecho está en el [historial](historial.md).

1. **Probar PCID** con KVM o en hardware (QEMU sin KVM no lo emula) y
   prenderlo por defecto: hoy KPTI vacía la TLB en cada cambio de tablas y
   el IPC cuesta casi el doble. El job `kvm` de la CI corre `make smoke` con
   KVM y falla si hay PCID y el kernel no lo usó.
2. **Fuzzing en el host** con `cargo-fuzz` (ELF, FDT, `Message::from_bytes`,
   layout del almacén, enlace) y `caos` con semilla al azar en la CI.
3. Después: handoff, SMP, KPTI, IOMMU y Raspberry Pi 4 y 5 (ver
   [`arm64.md`](arm64.md)).

## Riesgos

* **Spectre**: no confiar solo en el lenguaje; MMU por cell siempre.
* **`unsafe`**: lint `undocumented_unsafe_blocks` en deny, registro en
  `docs/unsafe.md`, auditoría por PR.
* **Hardware**: empezar solo con virtio en QEMU.
* **Live update**: primero por cell (protocolo de supervisor), no kernel.
