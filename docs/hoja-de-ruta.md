# Hoja de ruta

Cada hito se entrega como un PR con tests y documentación.

| # | Hito | Estado |
|---|---|---|
| 1 | Especificar ABI de IPC y modelo de capacidades | ✅ `docs/abi-ipc-capacidades.md`, `crates/abi` |
| 2 | Imagen booteable mínima: Limine + kernel que imprime por serial (BIOS y UEFI) | ✅ |
| 3 | Allocador físico/virtual, paginación, GDT/IDT, scheduler básico | ✅ 9 tests en QEMU + 7 en host |
| 4 | IPC seguro (canales + transferencia de capacidades) y creación de cells | ✅ ping/pong/intruso en anillo 3 |
| 5 | "hello cell" y driver virtio de consola como cell | ⏳ siguiente |
| 6 | Fault containment: matar/reiniciar una cell sin tumbar el kernel | ⏳ matar ya funciona; falta reiniciar (supervisor) |
| 7 | Actualización en vivo de una cell | ○ |
| 8 | Tests de aislamiento y benchmark de IPC | ⏳ primeros tests (`intruso`) y benchmark de ida y vuelta |
| 9 | Documentar límites de `unsafe` y modelo de amenazas | ⏳ registro inicial en `docs/unsafe.md` |
| 10 | Fase 2: migración de cells entre dos instancias QEMU | ○ |

## Criterios de aceptación del MVP

- [x] Arranca en QEMU x86_64.
- [x] Kernel mínimo sin GC.
- [ ] Al menos 3 cells aisladas comunicándose por IPC (hoy 3 cells aisladas, 2 de ellas por IPC).
- [ ] Un driver en user-space.
- [x] Un fallo de cell no afecta al kernel (la cell se termina y se libera su memoria).
- [ ] Actualización en vivo de una cell sin reinicio.
- [ ] `unsafe` confinado y documentado (en curso: lint obligatorio + registro).
- [ ] Benchmarks básicos de IPC y arranque (arranque medido con el TSC; IPC: ida y vuelta entre `ping` y `pong`; falta automatizar y comparar).

## Hito 3 (hecho)

* GDT + TSS con pila IST para el doble fallo; IDT con 256 stubs y un único
  punto de entrada que devuelve el frame a reanudar (el cambio de contexto
  es "devolver otro frame").
* Allocador de marcos con bitmap (`crates/kcore`, sin `unsafe`, probado
  contra un modelo de referencia en el host).
* Tablas de páginas propias: HHDM sin ejecución, kernel con W^X por sección
  (text R-X, rodata R--, data RW-), NX y CR0.WP activos.
* Heap del kernel de 8 MiB con allocador seguro frente a interrupciones.
* APIC local con timer periódico a 100 Hz calibrado con el PIT; PIC 8259
  deshabilitado.
* Scheduler round-robin apropiativo de hilos de kernel; `yield` por
  interrupción de software; liberación diferida de pilas de hilos muertos.
* `ktest`: tests dentro del kernel en QEMU, más un test negativo que
  comprueba que escribir en `.rodata` produce #PF.

## Hito 4 (hecho)

* Espacio de direcciones por cell: mitad baja propia, mitad alta del kernel
  compartida (tablas de nivel 3 preasignadas) y sin bit USER.
* Entrada a anillo 3 y syscalls con `syscall`; vuelta con `iretq`. Las
  syscalls corren sin interrupciones (kernel no apropiable).
* CSpace por cell (`crates/kcore::cspace`, con tests de handles viejos,
  falsificados, atenuación y operaciones atómicas).
* Canales con colas acotadas, bloqueo real (hilos `Blocked` + `wake`) y
  transferencia atómica de capacidades: se mueven solo si el envío ocurre.
* Validador ELF en `crates/kcore::elf` (W^X, rangos de usuario, sin
  solapes) y carga de cells desde módulos de Limine.
* Copias kernel↔cell siempre por traducción de páginas con bit USER
  (`usercopy`): una cell no puede hacer que el kernel toque memoria ajena.
* Excepciones en anillo 3: se termina la cell, se cierran sus capacidades
  (los peers ven `PeerClosed`) y se libera su memoria.
* Cells de ejemplo: `ping`, `pong` (eco, transferencia de canal, benchmark)
  e `intruso` (8 abusos bloqueados y lectura del kernel atrapada por la MMU).

Pendiente, para hitos siguientes: `wait` multi-handle, `channel_call`,
objetos `Memory` (zero-copy), `cell_spawn` desde una cell (supervisor),
`Irq`/`IoPorts` para drivers.

## Hito 5 en detalle (próximo)

1. Objetos `Memory`, `Irq` e `IoPorts` y sus syscalls.
2. `cell_spawn` desde una cell `init` que hace de supervisor.
3. Driver virtio-console como cell (PCI legacy de QEMU) y "hello cell" que
   escribe a través de él.

## Riesgos

* **Spectre**: no confiar solo en el lenguaje; MMU por cell siempre.
* **`unsafe`**: lint `undocumented_unsafe_blocks` en deny, registro en
  `docs/unsafe.md`, auditoría por PR.
* **Hardware**: empezar solo con virtio en QEMU.
* **Live update**: primero por cell (protocolo de supervisor), no kernel.
