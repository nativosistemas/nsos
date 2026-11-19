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
| 10 | Fase 2: migración de cells entre dos instancias QEMU | ○ |

## Criterios de aceptación del MVP

- [x] Arranca en QEMU x86_64.
- [x] Kernel mínimo sin GC.
- [x] Al menos 3 cells aisladas comunicándose por IPC (`init` con `fragil`, `ping`/`pong`, `hola`/`virtcon`).
- [x] Un driver en user-space (`virtcon`).
- [x] Un fallo de cell no afecta al kernel (la cell se termina, se libera su memoria y un supervisor puede reiniciarla).
- [x] Actualización en vivo de una cell sin reinicio (protocolo de supervisor con rollback, modelado en Lean).
- [x] `unsafe` confinado y documentado (lint obligatorio, registro en `docs/unsafe.md` y auditoría automática en la CI).
- [x] Benchmarks básicos de IPC y arranque (cell `bench` en una corrida aparte; la CI publica la tabla en el resumen de cada PR).

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
  3. `init` reenvía ambas capacidades a la nueva con `UPGRADE_RESTORE`. La
     nueva valida el estado sin escribirlo y responde:
     * `UPGRADE_ACCEPT`: `init` manda `UPGRADE_COMMIT` a la v1, que termina;
       la nueva migra el estado a `CNT2` y sigue atendiendo al mismo extremo.
     * `UPGRADE_REJECT [mem, ep]`: `init` devuelve todo a la v1 con
       `UPGRADE_ROLLBACK`; la v1 relee su contador de `mem` y retoma.
* El test hace primero un intento con `contador-roto` (rechaza: rollback) y
  después con `contador-v2` (acepta).
* Verificación formal: `verification/lean/Nsos/Upgrade.lean` modela el
  protocolo y demuestra que el cliente ve `1, 2, …, n` sin saltos con
  cualquier intercalado de pedidos, rechazos y aceptaciones, y que solo
  atiende quien tiene estado y canal. El modelo se escribió antes que el
  código del rollback.
* El `cliente` nunca se reconecta: su extremo es el mismo y el pedido que
  quedó encolado durante el cambio viaja con el extremo del servidor. El
  test exige que el contador avance de a uno, sin saltos ni repeticiones,
  con al menos 5 respuestas de cada versión.
* Limitaciones:
  * El rollback cubre el rechazo explícito. Si la versión nueva *falla*
    (excepción) con las capacidades, el extremo del cliente se cierra y el
    cliente ve `PEER_CLOSED`. Cubrirlo exige que el kernel devuelva las
    capacidades de una cell muerta a su supervisor, o un extremo de respaldo.
  * Mover una capacidad `Memory` no desmapea a quien la tenía: la v1 sigue
    viendo `mem` después de entregarlo (lo usa para el rollback). Una versión
    vieja maliciosa podría escribir el estado de la nueva. Falta `mem_unmap`
    y decidir si mover una capacidad `Memory` revoca los mapeos.
  * La v1 sondea dos canales porque todavía no hay `wait` multi-handle.

## Hito 8 (hecho)

* `intruso` prueba 27 abusos. A los 8 de antes suma las syscalls de memoria,
  puertos y cells: tamaños y flags inválidos, mapear sobre el kernel o sobre
  su código, W+X, desalinear, tipos de objeto equivocados, ver direcciones
  físicas sin `CONTROL`, escribir con una capacidad de solo lectura, mover
  una capacidad sin `GRANT`, `cell_spawn`/`cell_kill`/`cell_status` sin
  derechos. Para probar derechos se fabrica sus propios objetos y los atenúa.
* `caos` hace 50 000 syscalls con números y argumentos pseudoaleatorios
  (semilla fija). El kernel tiene que devolver un resultado válido o un error
  del ABI en cada una, y seguir: ktest exige que `caos` termine con 0 y que
  no se pierda memoria. Corre a la vez que las demás cells.
* Fuzzing en host: el del validador ELF ahora hace 50 000 mutaciones de las
  cabeceras y verifica todas las reglas de carga sobre lo aceptado (W^X,
  rangos, sin solapes, entrada en un segmento ejecutable); se suma uno para
  `CellStatus`.
* Benchmarks: la bandera de arranque `bench` corre solo la cell `bench`
  (kernel en release) y mide ida y vuelta de IPC, ciclo de vida de una cell y
  `mem_create` + `mem_map`; el arranque lo mide el kernel. La CI agrega la
  tabla al resumen del job (`tools/bench-summary.sh`). Valores en este
  contenedor (QEMU TCG, sin KVM): arranque 15,7 ms, IPC 23,8 µs, ciclo de
  vida 447 µs, memoria 66 µs.

## Hito 9 (hecho)

* `docs/amenazas.md`: activos, adversario (una cell con código nativo
  cualquiera), qué se garantiza con qué mecanismo y con qué prueba, y 8
  huecos conocidos ordenados por gravedad (canales laterales sin KPTI, DMA
  sin IOMMU, mapeos que sobreviven a mover un `Memory`, DoS sin cuotas,
  consola falsificable, caída de la versión nueva en una actualización,
  arranque sin firmas, un solo núcleo).
* `tools/audit-unsafe.sh` (en `make lint` y en la CI): cada archivo con
  `unsafe` tiene que estar en `docs/unsafe.md` y al revés; `abi` y `kcore`
  con `forbid(unsafe_code)`, cada cell con `deny(unsafe_code)`.
* SMEP y SMAP activados si la CPU los tiene. La entrada de syscalls y de
  interrupciones limpia `AC`. QEMU corre con `-cpu max`. La prueba negativa
  `test-smap-fault` lee una página de usuario desde el kernel y espera #PF.
  El kernel no necesitó cambios: ya copiaba todo por el HHDM.

## Hito 10 en detalle (próximo)

Migración de una cell entre dos instancias QEMU (fase 2):

1. Serializar el estado de una cell con el mismo protocolo de la
   actualización en vivo (`UPGRADE_STATE`), sin congelar la memoria entera.
2. Un canal entre máquinas: driver virtio-net o puerto serie entre dos QEMU,
   como cell.
3. Proxy de capacidades: los canales de la cell migrada se reenvían por el
   enlace; el cliente no nota la mudanza (mismo invariante que
   `Upgrade.client_sees_consecutive`, a modelar en Lean antes de escribirlo).

## Riesgos

* **Spectre**: no confiar solo en el lenguaje; MMU por cell siempre.
* **`unsafe`**: lint `undocumented_unsafe_blocks` en deny, registro en
  `docs/unsafe.md`, auditoría por PR.
* **Hardware**: empezar solo con virtio en QEMU.
* **Live update**: primero por cell (protocolo de supervisor), no kernel.
