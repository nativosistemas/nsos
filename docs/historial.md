# Historial de hitos

Lo que se hizo en cada hito, en orden. La lista corta de lo que sigue está
en la [hoja de ruta](hoja-de-ruta.md).

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

Pendiente, para hitos siguientes: `wait` multi-handle (hito 12), `channel_call` (hito 13),
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
  * El rollback cubre el rechazo explícito y, desde que el supervisor
    guarda un respaldo (estado escribible y copia del extremo), también la
    caída de la versión nueva antes de decidir. Una caída después de
    aceptar sigue cerrando el extremo del cliente.
  * Mover una capacidad `Memory` ya revoca los mapeos de quien la tenía
    (`Mem.step_iso`); la v1 vuelve a mapear el estado al retomar.
    (`mem_unmap` llegó en el hito 13.)
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

## Hito 10 (hecho)

* Dos QEMU unidas por el segundo puerto serie (COM2) de cada una, con un
  socket Unix (`tools/migrate-test.sh`, parte de `make smoke`). El kernel
  arranca en modo `migrar-origen` o `migrar-destino` y solo entrega a la
  cell `enlace` el rango de puertos de COM2.
* El protocolo es el de la actualización en vivo: para `origen` el enlace
  es la versión nueva. Primero pide `contador-roto` en B, que rechaza:
  rollback en A y la v1 retoma. Después pide `contador-v2`, que acepta: la
  v1 termina y el enlace de A reenvía los pedidos del `cliente` por el
  cable. El `cliente` es el mismo del hito 7 y verifica que el contador no
  salta.
* Verificación formal: `verification/lean/Nsos/Migration.lean`, escrito
  antes que el código. Demuestra que el cliente ve el contador sin saltos,
  que nunca atienden las dos máquinas a la vez y que un rechazo remoto no
  puede dañar el estado local (B solo recibe una copia).
* Hallazgo al probarlo: limpiar la FIFO del UART al iniciarlo perdía el
  primer byte si la otra máquina ya había mandado algo. El enlace deja la
  FIFO apagada y QEMU retiene lo que llega.
* Solo con BIOS: OVMF usa los puertos serie como consola y con UEFI los
  firmwares se escriben entre sí por COM2.
* Límites (ver `docs/amenazas.md`): el cable no está autenticado, las
  capacidades no cruzan de máquina y se migra un servicio cuyo estado cabe
  en 32 bytes (lo que define el formato del contador).

## Después del backlog

* Hecho: revocar mapeos al mover un `Memory` (PR #15, modelo
  `verification/lean/Nsos/Memory.lean` y Kani sobre la paginación).
* Hecho: enlace autenticado entre máquinas sobre virtio-net (ver abajo).
* En curso: rollback cuando la versión nueva se cae (no solo cuando
  rechaza), extendiendo `Upgrade.lean`.
* Regla para todo trabajo nuevo: evaluar la verificación formal desde el
  diseño (ver [`CONTRIBUTING.md`](../CONTRIBUTING.md)).

## Enlace autenticado (hecho)

* Modelo primero: `verification/lean/Nsos/Link.lean`. Contra un adversario
  que controla la red (descarta, reordena, repite, refleja e inyecta), lo
  que el receptor entrega es un prefijo de lo que mandó la otra punta en la
  sesión actual: nada falsificado, nada repetido, todo en orden. Cuatro
  mutaciones (sin MAC, sin sesión, sin dirección, secuencia `≤`) rompen la
  demostración.
* `crates/link` (sin `unsafe`, en el host): SHA-256 y HMAC-SHA256 propios
  con vectores FIPS 180-4 y RFC 4231; sesión con un nonce aleatorio
  (`rdrand`) de cada punta; tramas con dirección, secuencia y etiqueta de
  128 bits; tests de ataques (cada bit alterado, repetición, reorden,
  reflejo, otra sesión, otra clave) y Kani sobre el parser.
* Cell `virtnet`: driver virtio-net legacy en user-space (difusión con un
  EtherType propio); no participa de la autenticación.
* `enlace` usa la red en lugar de COM2; el kernel le da la clave
  (`enlace.key` en la ISO) y el rol en un `Memory` de solo lectura.
* Prueba negativa en QEMU (`tools/migrate-test.sh --clave-distinta`): con
  otra clave, B descarta todo, no arranca ninguna versión y A hace rollback.
* Ahora también corre con UEFI (OVMF ya no se mete en el enlace).

## Hito 11: almacenamiento propio (hecho)

* Cell `virtblk`: driver virtio-blk legacy en user-space con un protocolo
  genérico de bloques (`INFO`, `READ`, `WRITE`, `FLUSH`) y un búfer
  compartido para DMA. Sirve igual para el almacén y para una futura cell
  lectora de FAT32.
* `crates/store`: objetos con dueño, nombre y versión; escritura
  copy-on-write con dos ranuras de superbloque y SHA-256 de todo el árbol;
  versión esperada (compare-and-swap); nunca formatea un disco ajeno.
* Cell `almacen`: monta el disco y da acceso por capacidades (una raíz por
  dueño, un extremo de canal por objeto abierto, copias de solo lectura).
* Verificación primero: `verification/lean/Nsos/Store.lean` (consistencia
  ante caídas y aislamiento entre dueños), tests en el host con cada
  combinación de escrituras rotas, Kani sobre el decodificador y el mapa de
  bloques libres.
* Prueba en QEMU (`make smoke-almacen`): caída simulada, `kill -9` a QEMU
  en momentos al azar y un disco FAT que no se toca.

## Hito 12: `wait` multi-handle e interrupciones (hecho)

Primera parte (hecha): la syscall `wait`.

* Modelo primero: `verification/lean/Nsos/Wait.lean`. Modela el recorrido
  del kernel paso a paso (marcar bloqueado, registrar y mirar cada ítem bajo
  su lock, ceder la CPU), intercalado con cualquier activación y apagado de
  señales y con despertares arbitrarios (`cell_kill`, plazos). Demuestra
  `no_lost_wakeup`: un hilo dormido con un ítem listo ya está despierto.
  Cuatro mutaciones rompen la demostración: marcar bloqueado al final del
  recorrido en vez de al principio, que activar una señal despierte solo al
  primero de la lista, que el recorrido no se despierte a sí mismo si un
  ítem ya está listo y que registre solo la primera señal pedida.
* `nsos_kcore::wait` (sin `unsafe`): qué señales emite cada tipo, validación
  de ítems, señales de un canal y de una cell, y el plazo. Kani:
  `check_item_any`, `channel_signals_any`, `deadline_any`.
* Kernel: `Endpoint::watch` y `Cell::watch_exit` (paso `scan` del modelo),
  plazos en el scheduler (`sched::wake_at`), y `unrecv` ahora despierta a
  quienes esperan `READABLE` (el modelo exige que toda activación despierte).
* Cells: `espera` prueba la semántica (plazo 0, plazo que vence a tiempo,
  `READABLE`, `WRITABLE` con la cola del peer llena y vaciándose,
  `PEER_CLOSED`, varios ítems); `contador-v1` duerme en `wait` sobre sus dos
  canales en lugar de sondearlos; `init` espera `CELL_EXITED` con `wait` y
  mata a una `dormilona` dormida en `wait`; `intruso` suma 11 abusos (38 en
  total); `caos` ahora llama a `wait` con plazo 0.

Segunda parte (hecha): capacidad `Irq` e `irq_ack`.

* Modelo primero: `verification/lean/Nsos/Irq.lean`. Una línea por nivel con
  un dispositivo arbitrario, el kernel (enmascarar al entregar, `irq_ack` del
  dueño, reclamar y soltar). Demuestra `no_storm` (sin `irq_ack` no vuelve a
  llegar), `no_lost_irq` (con la línea activa, o hay `IRQ_PENDING` o puede
  llegar), `ack_needs_owner`, `unowned_silent` y `claim_exclusive`. Cuatro
  mutaciones rompen la demostración: entregar sin enmascarar, que `irq_ack`
  desenmascare sin borrar `IRQ_PENDING`, que lo borre sin desenmascarar y
  soltar la línea sin enmascararla.
* `nsos_kcore::irq` (sin `unsafe`): registro de líneas reclamadas, entrada
  del IOAPIC y lectura del `interrupt-map` de PCI. Kani: `claims_exclusive`
  e `intx_any`; dos mutaciones las rompen (reclamar sin mirar si ya tiene
  dueño, aceptar SPI fuera de rango).
* Kernel: `irq.rs` (objeto `Irq`, `on_irq`, `irq_ack`), IOAPIC en x86_64
  (`arch/x86_64/ioapic.rs`, vectores 48 en adelante), SPI por nivel en el
  GIC; `Device::irq_cap` entrega la línea al driver en el slot 5.
* Drivers: `virtcon`, `virtblk` y `virtnet` duermen en `IRQ_PENDING` (el de
  red, en `wait` sobre su canal y la línea a la vez); sin línea, sondean como
  antes y le piden al dispositivo que no interrumpa. `virtcon` comprueba
  que su `Irq` no se copia ni se manda a otra cell.
* Pruebas: ktest exige que `virtcon` reciba interrupciones;
  `migrate-test.sh` y `store-test.sh`, que `virtnet` y `virtblk` también
  (en x86_64 y en ARM64); `intruso` suma 4 abusos de `irq_ack` y `wait`
  (42 en total).

## Hito 13: `channel_call` y `mem_unmap` (hecho)

Con esto no queda ninguna syscall del ABI v1 que devuelva `Unsupported`.

* Verificación: `mem_unmap` es una sucesión de pasos `unmap` que el modelo
  de memoria ya tenía (y cubre `Mem.step_iso`); se agregan
  `Mem.unmap_removes` (no queda ningún mapeo en esa página) y
  `Mem.unmap_only_shrinks` (no agrega mapeos ni toca capacidades, otras
  cells ni marcos). Dos mutaciones las rompen: invertir el filtro (quitar
  todo menos esa página) y que `unmap` además vacíe las capacidades.
  `channel_call` no agrega pasos: es un `send` seguido de un `recv` en el
  mismo extremo (`Sys.call_inv`, a partir de `run_inv`); no hay paso nuevo
  que mutar. En el código, `nsos_kcore::paging::unmap_target` elige el
  mapeo (Kani: `unmap_target_exact`; aceptar un objeto más grande que el
  rango la rompe).
* Kernel: `Cell::unmap_memory` (primero las tablas, después suelta el
  objeto); `channel_send` y `channel_recv` ahora comparten `send_msg` y
  `recv_msg` con plazo, y `channel_call` los compone. Al implementarla
  apareció un caso: adjuntar el propio extremo al pedido lo sacaba del
  CSpace mientras el kernel seguía esperando la respuesta en él; se rechaza.
* Cells: `cliente` hace cada pedido con `channel_call`; `bench` mide
  `ipc_call_ns` (en este contenedor, 50 µs contra 53 µs de `send` + `recv`)
  y desmapea en vez de usar una dirección nueva por vuelta; `espera` prueba
  la semántica de `channel_call`; la cell nueva `desmapeo` prueba
  `mem_unmap` y termina con #PF al leer la página desmapeada; `intruso` suma
  3 abusos de `channel_call` (45 en total).

Después: *handoff* directo al servidor en `channel_call`, cuotas por cell.

## Contratos de canal tipados (en curso)

Ver [`contratos.md`](contratos.md). Los protocolos dejan de ser constantes
sueltas: un contrato es un autómata que la macro `contract!` convierte en
un tipo por estado, y se comprueba al compilar.

* Etapa 1 (hecha): modelo primero (`Nsos/Contract.lean`: sin mensajes
  inesperados, sin bloqueo de protocolo, nada pendiente al terminar, colas
  acotadas; 3 contraejemplos y 6 mutaciones), crate `nsos-contract` con el
  mismo control como `const fn` (comparado con el modelo en 406
  contratos), Kani sobre el decodificador, y `ping`/`pong` migrados con
  una prueba negativa en QEMU. El control encontró que el protocolo viejo
  de `ping`/`pong` no tenía cota.
* Etapa 2 (hecha): actualización en vivo y migración con los contratos
  `counter`, `handover` y `restore`; el estado viaja a la versión nueva
  como `Mem<RO>`. Modelo: `UpgradeContract.lean` demuestra que cada paso
  de `Upgrade.lean` es un paso de los contratos (8 mutaciones lo rompen).
* Etapa 3 (hecha): almacén con `store_root`, `store_obj` y
  `store_share`; el búfer vuelve en toda respuesta por tipo. Modelo:
  `StoreContract.lean`. Las etiquetas pasaron a exigirse distintas por
  estado.
* Etapa 4 (hecha): drivers y enlace con `console`, `block` y el flujo
  `net` (forma `stream` de la macro, `try_send`); el modelo de flujos
  mostró un bloqueo mutuo posible entre `virtnet` y `enlace`, que se
  corrigió. Modelo: `MigrationContract.lean`.
* Etapa 5 (hecha): el kernel guarda el contrato de cada canal
  (`channel_create(out, contrato)`, `channel_contract`) y no lo deja
  cambiar (`ContractId.lean`, 2 mutaciones); `open` y `Endpoint` lo
  exigen. `intruso` suma 3 abusos y `ping` le manda a `pong` un canal de
  otro contrato.

## Cells selladas (hecho)

Como los SIPs de Singularity, una cell solo ejecuta el código de su imagen
ELF: no genera código ni toca el de otra cell. Antes se cumplía de hecho
(ninguna capacidad de memoria tenía `EXEC`), pero nada lo garantizaba y
`mem_map` controlaba W^X por mapeo, no por objeto: con un solo cambio de
derechos, el mismo objeto podía quedar RW y RX a la vez.

* Modelo primero: `Mem.step_sealed`, `Mem.exec_is_own_code`,
  `Mem.code_never_writable` y el contraejemplo `Mem.exec_obj_breaks`
  (`Nsos/Memory.lean`; 3 mutaciones lo rompen).
* `EXEC` deja de ser un derecho de `Memory` y `mem_map` lo rechaza
  (`kcore::paging::map_request`, Kani `map_request_never_exec`); todo
  objeto se mapea NX.
* Prueba negativa en QEMU (x86_64 y ARM64): la cell `sellada` escribe un
  `ret` en su memoria y salta ahí; el kernel la termina por #PF o
  instruction abort en esa dirección. Con la mutación de mapear la memoria
  sin NX, la cell ejecuta el `ret`, vuelve y ktest falla.
* Si algún día hace falta un JIT, tiene que ser un derecho nuevo y explícito,
  con W^X por objeto (nunca mapeado escribible en ningún lado mientras sea
  ejecutable) y su propio modelo.

## ARM64 (en curso)

* Paso 1 (hecho): QEMU `virt` aarch64. Kernel en EL1 y cells en EL0 con la
  misma lógica genérica (capa `arch` con `TrapFrame`, paginación, timer).
  Limine sobre UEFI, GICv2, generic timer, PL011, `svc` para syscalls.
* Verificación primero: codec de descriptores ARM64 sobre el recorrido de
  tablas existente, con Kani contra un modelo de la MMU (lo que ve el
  kernel es lo que hace cumplir la MMU); 4 mutaciones lo rompen.
* Pruebas negativas en QEMU: W^X del kernel y PAN.
* Paso 2 (hecho): device tree y PCI por ECAM. El kernel lee la máquina del
  device tree (lector verificado con Kani: sin pánicos con cualquier blob)
  y descarta toda región de dispositivo que toque la RAM; asigna los BAR de
  E/S con un asignador verificado (rangos disjuntos). Consola virtio,
  almacén y migración corren en ARM64 igual que en x86_64.
* Paso 3: Raspberry Pi 4 y 5 (firmware UEFI, controlador PCIe propio de la
  Pi, FAT32 para la SD). Ver [`arm64.md`](arm64.md).

## Cuotas de memoria por supervisor (hecho)

Cerraba el hueco más grande frente al adversario definido: una cell podía
dejar sin memoria a todas las demás.

* Modelo primero: `Nsos/Quota.lean`. Los marcos libres son la suma de las
  cuotas disponibles de las cells vivas (`step_inv`), así que una cell con
  cuota siempre consigue memoria haga lo que haga el resto
  (`no_starvation`), y ningún marco se pierde (`run_conserves`). Tres
  mutaciones lo rompen.
* `kcore::quota` hace exactamente los pasos del modelo (Kani:
  `charge_any`, `refund_any`, `split_any`, `absorb_any`). El kernel tiene
  una cuenta por cell (`kernel/src/quota.rs`) y cobra imagen, pila, tablas
  y `mem_create`; `cell_spawn` recibe la cuota de la hija.
* Prueba negativa en QEMU: `glotona` pide objetos de 1 MiB hasta
  `NoMemory` y se frena en su cuota; sin cuotas se lleva 215 MiB. ktest
  comprueba además que todas las cuotas vuelven enteras al kernel.

## Memoria del kernel por cell (hecho)

Una cell podía agotar el heap del kernel (8 MiB fijos): llenando las colas
de sus canales (hasta 1024 extremos con 64 mensajes cada uno, unos 20 MiB)
o creando hijas, que cuestan 64 KiB de pila de kernel cada una. Y el
registro de terminaciones y los nombres de hilo crecían sin tope.

* La cuenta de cada cell tiene un segundo recurso con las mismas reglas: la
  memoria del kernel (512 B por marco de cuota). Se cobran la pila de
  kernel y la CSpace de cada cell (reservada entera, nunca crece), cada
  mensaje encolado (a quien lo manda), cada canal y cada mapeo. El heap pasa
  a ser una novena parte de la RAM libre.
* Es otra instancia del modelo de `Nsos/Quota.lean` y usa la misma
  aritmética verificada con Kani (`kcore::quota`).
* Prueba negativa en QEMU: `charlatana` se manda mensajes a sí misma hasta
  `NoMemory` y se frena en 678; sin cobrarlos encola 12 928. ktest comprueba
  que las dos cuotas vuelven enteras al kernel.
* `EXITS` guarda las últimas 1024 terminaciones y el hilo de una cell ya no
  filtra una copia de su nombre.
