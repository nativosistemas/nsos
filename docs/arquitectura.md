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

## Arranque

1. El firmware (BIOS o UEFI) carga **Limine** desde la ISO.
2. Limine carga el ELF del kernel en la mitad alta
   (`0xffffffff80000000`), arma un mapeo directo de la memoria física (HHDM)
   y rellena las *requests* del kernel (`kernel/src/boot/limine.rs`).
3. `kmain` inicializa COM1 y valida la revisión del protocolo.
4. **CPU**: GDT, TSS (pila IST para doble fallo) e IDT.
5. **Memoria**: bitmap de marcos desde el mapa de memoria; tablas de páginas
   propias con W^X; heap del kernel.
6. **Interrupciones**: PIC deshabilitado, APIC local con timer periódico
   calibrado con el PIT.
7. **Scheduler**: el contexto de arranque pasa a ser el hilo idle; se
   habilitan interrupciones e imprime `nsos: arranque OK` con el tiempo de
   arranque medido con el TSC.
8. Con `test-exit` corre `ktest` y sale de QEMU por `isa-debug-exit`.

## Mapa de memoria virtual

| Rango | Uso | Permisos |
|---|---|---|
| `0x0000_0000_0000_0000 – 0x0000_7fff_ffff_ffff` | espacio de la cell (hito 4) | por cell |
| `0xffff_8000_0000_0000 + física` | HHDM: toda la RAM y MMIO mapeado | RW-, sin ejecución |
| `0xffff_a000_0000_0000 …` | reservado para mapeos de prueba / futuros | — |
| `0xffff_ffff_8000_0000 …` | imagen del kernel | text R-X, rodata R--, data RW- |

## Interrupciones y cambio de contexto

Todos los vectores pasan por el mismo stub: guarda los registros en un
`TrapFrame` en la pila actual y llama a `trap::dispatch`, que devuelve el
frame desde el cual continuar. Si el timer decide cambiar de hilo, devuelve
el frame guardado del otro hilo y el stub lo restaura con `iretq`. No hay
otra rutina de *context switch*: un hilo nuevo es una pila con un frame
fabricado que apunta a su función.

## Cells en anillo 3

* Cada cell tiene su PML4. La mitad alta (kernel, HHDM, pilas de kernel) es
  la misma en todos los espacios y no tiene bit USER; la baja es de la cell.
* Al cambiar a un hilo de cell el scheduler carga su CR3 y fija la pila de
  kernel en el TSS y para `syscall`.
* Una syscall construye el mismo `TrapFrame` que una interrupción: bloquear
  en `channel_recv` es marcar el hilo `Blocked` y ceder la CPU.
* El kernel nunca desreferencia punteros de cells: traduce cada página con
  las tablas de la cell, exige USER/WRITABLE y copia por el HHDM.
* Una excepción en anillo 3 termina la cell (capacidades cerradas, memoria
  y tablas liberadas) y el kernel elige otro hilo.

```
+--------+  canal (capacidades)  +--------+
|  ping  | <-------------------> |  pong  |      intruso: sin capacidades,
+--------+                       +--------+      todo intento falla y su
     |  canal nuevo, creado por ping y movido a pong |  lectura del kernel
     +-----------------------------------------------+  termina en #PF
```

## Supervisión

El kernel no tiene políticas de reinicio: solo termina una cell que falla y
avisa a quien la espera. La política vive en una cell supervisora (`init`)
con capacidades `Image` (para crear cells) y `Cell` (para esperarlas o
matarlas):

```
init --cell_spawn(fragil, [canal])--> fragil   #PF       -> Faulted
     <--cell_status (espera)--------           reiniciar
init --cell_spawn(fragil, [canal'])-> fragil   panic     -> Exited(101)
init --cell_spawn(fragil, [canal''])> fragil   ok        -> Exited(0)
init --cell_kill--------------------> dormilona          -> Killed
```

`cell_kill` solo marca el pedido y despierta a la cell; la cell muere desde
su propio hilo en la ruta de vuelta a anillo 3. Así nunca se destruye un
espacio de direcciones que otro hilo está usando ni una cell a mitad de una
syscall.

## Actualización en vivo

Tampoco está en el kernel: es un protocolo entre el supervisor y el servicio.
Lo que lo hace posible es que un extremo de canal es una capacidad que se
puede mover, con su cola de mensajes, sin que el otro extremo lo note.

```
cliente ====canal==== contador-v1           cliente ====canal==== contador-v2
                         |  UPGRADE_STATE        ^                     ^
                         |  [Memory, extremo]    |   UPGRADE_RESTORE   |
                         +-------> init ---------+---------------------+
                                    ^   UPGRADE_ACCEPT / UPGRADE_REJECT [Memory, extremo]
                                    |   -> UPGRADE_COMMIT a la v1, o UPGRADE_ROLLBACK [Memory, extremo]
```

El protocolo está modelado en Lean (`verification/lean/Nsos/Upgrade.lean`):
con cualquier intercalado de pedidos, rechazos y aceptaciones, el cliente ve
el contador sin saltos y solo atiende quien tiene estado y canal.

### Migración entre máquinas (hito 10)

```
máquina A                                           máquina B
cliente ==canal== enlace ~~ virtnet ~~ red (HMAC) ~~ virtnet ~~ enlace ==canal== contador-v2
                    ^                            |
   origen: PREPARE  |  UPGRADE_RESTORE           |  UPGRADE_RESTORE
   a contador-v1 ---+  [Memory, extremo]         +-> destino -> contador-v2
```

Para `origen` (supervisor de A), el `enlace` es la versión nueva: el mismo
protocolo de actualización, con rechazo y rollback incluidos. Por la red
viaja una copia del estado; las capacidades se quedan en su máquina. Cuando
B acepta, el enlace de A reenvía cada pedido del cliente y el cliente no
nota la mudanza. Modelo: `verification/lean/Nsos/Migration.lean`.

El enlace va sobre virtio-net (driver `virtnet`, una cell aparte) y está
autenticado (`crates/link`): las dos máquinas comparten una clave, negocian
una sesión con un nonce aleatorio de cada lado y cada trama lleva
dirección, secuencia y un HMAC-SHA256. Un mensaje falso, repetido, reflejado
o de una sesión vieja se descarta. Modelo: `verification/lean/Nsos/Link.lean`.
El driver no participa de la autenticación: aunque estuviera comprometido,
no podría hacer que el enlace acepte una trama falsa.

## Drivers en user-space

El kernel no maneja dispositivos: los *descubre* y entrega sus recursos como
capacidades. Para la consola virtio:

```
kernel: pci::find(1af4:1003) -> BAR0 (puertos) + 2 objetos Memory (DMA)
          |
          v  capacidades de arranque
+---------+   canal (WRITE/WRITTEN)   +--------+
| virtcon | <-----------------------> |  hola  |
+---------+                           +--------+
     | ioport_read/write (relativo a BAR0), anillos virtio en Memory
     v
dispositivo virtio-console (QEMU) -> archivo build/virtcon.log
```

Sin IOMMU, un driver con DMA puede escribir cualquier memoria física: el
driver sigue siendo de confianza respecto al DMA (documentado en el modelo
de amenazas). Lo que sí garantiza el kernel es que solo toca *sus* puertos.

## Organización del repositorio

```
crates/abi/        ABI kernel↔cells (no_std, sin unsafe, testeable en host)
crates/kcore/      lógica del kernel sin hardware (marcos, CSpace, validador ELF)
cells/rt/          runtime de las cells (entrada, syscalls, println!)
cells/ping|pong/   cells de ejemplo que hablan por IPC
cells/intruso/     test de aislamiento
cells/init/        supervisor: reinicia cells que fallan, mata cells
cells/fragil|dormilona/  cells que fallan o no terminan, para el supervisor
cells/contador-v1|v2/    servidor con estado en dos versiones (actualización en vivo)
cells/contador-roto/  versión que rechaza el estado (prueba el rollback)
cells/cliente/     cliente del contador
cells/caos/        fuzzer de syscalls (50 000 llamadas al azar)
cells/bench/       benchmarks (bandera de arranque `bench`)
cells/virtcon/     driver virtio-console en user-space
cells/hola/        cliente del driver: escribe por la consola virtio
kernel/            microkernel (x86_64-unknown-none)
  src/arch/        código dependiente de arquitectura
  src/boot/        protocolo Limine
  src/mm/          marcos físicos, tablas de páginas del kernel, heap
  src/sched.rs     scheduler (hilos de kernel y de cells, bloqueo)
  src/cell.rs      cells, objetos del kernel y canales
  src/syscall.rs   implementación de las syscalls
  src/usercopy.rs  copias seguras kernel↔cell
  src/pci.rs       descubrimiento PCI (solo encuentra y entrega recursos)
  src/trap.rs      despacho de interrupciones y excepciones
  src/ktest.rs     tests dentro del kernel
docs/              especificaciones y documentación
tools/             construcción de imagen, QEMU, tests de humo
```

Más adelante: `tests/` (aislamiento) y `bench/` (IPC y arranque).

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
