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
| 7 | Actualización en vivo de una cell | ✅ `contador-v1` → `contador-v2` sin cortar al `cliente` |
| 8 | Tests de aislamiento y benchmark de IPC | ⏳ primeros tests (`intruso`) y benchmark de ida y vuelta |
| 9 | Documentar límites de `unsafe` y modelo de amenazas | ⏳ registro inicial en `docs/unsafe.md` |
| 10 | Fase 2: migración de cells entre dos instancias QEMU | ○ |

## Criterios de aceptación del MVP

- [x] Arranca en QEMU x86_64.
- [x] Kernel mínimo sin GC.
- [x] Al menos 3 cells aisladas comunicándose por IPC (`init` con `fragil`, `ping`/`pong`, `hola`/`virtcon`).
- [x] Un driver en user-space (`virtcon`).
- [x] Un fallo de cell no afecta al kernel (la cell se termina, se libera su memoria y un supervisor puede reiniciarla).
- [x] Actualización en vivo de una cell sin reinicio (protocolo de supervisor; falta el rollback).
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
`mem_create` (zero-copy entre cells; hecho en el hito 7), `cell_spawn` desde una cell
(supervisor), `Irq` para drivers.

## Hito 5 (hecho)

* Objetos `Memory` (marcos contiguos, a cero) e `IoPorts` (rango de
  puertos). Syscalls `mem_map`, `mem_info` (la dirección física solo con
  `CONTROL`, para DMA), `ioport_read` y `ioport_write` (desplazamiento
  relativo al rango concedido; nunca un puerto fuera de él).
* Enumeración PCI mínima en el kernel (`kernel/src/pci.rs`): encuentra la
  consola virtio (`1af4:1003`), habilita E/S y bus master y entrega BAR0
  como `IoPorts`.
* Cell `virtcon`: driver virtio-console (PCI legacy) en anillo 3. Configura
  la cola de transmisión, atiende `WRITE` por un canal y responde `WRITTEN`.
  Sin interrupciones por ahora: sondea el anillo "used" cediendo la CPU.
* Cell `hola`: escribe tres líneas a través del driver. El test de humo
  exige que el texto aparezca en la consola virtio de QEMU (un archivo) y
  que ambas cells terminen con 0 (al cerrarse `hola`, `virtcon` ve
  `PeerClosed` y termina).

## Hito 6 (hecho)

* Objetos `Image` (imagen ELF, derecho `EXEC`) y `Cell` (derecho
  `CONTROL` para matarla, `WAIT` para esperarla).
* Syscalls `cell_spawn` (mueve las capacidades a la hija, que las recibe en
  los slots `1..=n`), `cell_kill` y `cell_status` (bloqueante o
  `NONBLOCK`).
* `cell_kill` no destruye la cell desde otro hilo: marca el pedido y
  despierta a la cell. Sus esperas en el kernel abandonan y, al volver a
  anillo 3, `trap::dispatch` la desvía a un trampolín en anillo 0 que la
  termina desde su propio hilo (igual que `exit`). Funciona tanto si está
  bloqueada en una syscall como si gira en anillo 3.
* Cell `init` (supervisor): reinicia `fragil` (falla con #PF, luego con
  pánico, y al tercer intento sale bien) con un canal nuevo por intento, y
  mata a `dormilona` bloqueada y girando. Mientras tanto `ping`/`pong` y
  el driver siguen corriendo. Los tests en QEMU verifican la secuencia de
  terminaciones y que no se pierde memoria.

## Hito 7 (hecho)

* Syscall `mem_create(size, 0)`: memoria nueva, a cero, con
  `READ | WRITE | DUPLICATE | GRANT` (sin `CONTROL`: no revela direcciones
  físicas). Tope de 1 MiB por objeto.
* Protocolo de la sección 8 del ABI, en user-space:
  1. `init` arranca `contador-v2` **sin** capacidades de servicio.
  2. Envía `UPGRADE_PREPARE` a `contador-v1`, que deja de atender, escribe
     su estado (formato `CNT1`) en un objeto `Memory` y responde
     `UPGRADE_STATE` adjuntando el objeto y el extremo de su cliente.
  3. `init` reenvía ambas capacidades a la v2 con `UPGRADE_RESTORE`. La v2
     migra el estado a `CNT2` y sigue atendiendo al mismo extremo.
* El `cliente` nunca se reconecta: su extremo es el mismo y el pedido que
  quedó encolado durante el cambio viaja con el extremo del servidor. El
  test exige que el contador avance de a uno, sin saltos ni repeticiones,
  con al menos 5 respuestas de cada versión.
* Limitaciones: falta el rollback (si la v2 rechaza el estado, devolverlo a
  la v1), y la v1 sondea dos canales porque todavía no hay `wait`
  multi-handle.

## Hito 8 en detalle (próximo)

1. Tests de aislamiento sistemáticos: cada syscall con handles, punteros y
   derechos inválidos (ampliar `intruso`), y fuzzing del decodificador de
   mensajes y del validador ELF.
2. Benchmarks reproducibles: ida y vuelta de IPC, `cell_spawn` y arranque,
   con resultados en CI para comparar entre PRs.

## Riesgos

* **Spectre**: no confiar solo en el lenguaje; MMU por cell siempre.
* **`unsafe`**: lint `undocumented_unsafe_blocks` en deny, registro en
  `docs/unsafe.md`, auditoría por PR.
* **Hardware**: empezar solo con virtio en QEMU.
* **Live update**: primero por cell (protocolo de supervisor), no kernel.
