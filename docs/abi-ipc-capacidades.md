# Especificación: ABI de IPC y modelo de capacidades (v1)

Estado: **borrador aceptado para el hito 1**. La fuente de verdad ejecutable es
la crate [`nsos-abi`](../crates/abi); este documento explica el *por qué*.
Si ambos divergen, es un bug.

## 1. Objetivos

1. **Sin autoridad ambiental.** Una cell solo puede actuar sobre lo que tiene
   una capacidad. No hay rutas globales, PIDs que se puedan señalar, ni
   syscalls "privilegiadas" por identidad.
2. **Sin memoria compartida implícita.** Las cells no comparten punteros. Los
   datos grandes cambian de dueño (modelo *exchange heap* de Singularity),
   no se comparten.
3. **Doble barrera.** El kernel valida todo lo que viene de una cell como si
   fuera hostil, aunque esté escrita en Rust seguro: la seguridad del
   lenguaje no protege contra Spectre ni contra `unsafe` mal usado, así que
   cada cell además vive en su propio espacio de direcciones (MMU).
4. **Asíncrono primero.** Las primitivas encajan con un executor `async` en la
   cell (estilo Midori): enviar no bloquea mientras haya cola, y `wait`
   multiplexa muchos handles.
5. **Validable en el host.** Todo el formato se puede probar y fuzzear con
   `cargo test` sin arrancar QEMU.

## 2. Objetos del kernel

| Tipo (`ObjectType`) | Qué representa | Derechos válidos |
|---|---|---|
| `Channel` (1) | Un extremo de un canal bidireccional | `SEND RECV` |
| `Memory` (2) | Conjunto de marcos físicos mapeable | `READ WRITE CONTROL` (`CONTROL`: conocer la dirección física, para DMA; nunca `EXEC`) |
| `Cell` (3) | Control de una cell hija | `CONTROL` |
| `Irq` (4) | Línea de interrupción para un driver | `CONTROL` |
| `IoPorts` (5) | Rango de puertos x86 para un driver | `READ WRITE` |
| `Image` (6) | Ejecutable ELF ya validado | `EXEC` |

Todos admiten además `GRANT`, `DUPLICATE` y `WAIT`.

Los objetos se cuentan por referencias: viven mientras exista al menos una
capacidad que apunte a ellos.

## 3. Capacidades y handles

Cada cell tiene un **CSpace**: una tabla de slots, cada uno con
`(objeto, derechos)`. Desde la cell solo se ve un `CapHandle` de 32 bits:

```
 31            20 19                    0
+----------------+-----------------------+
|  generación:12 |       slot:20         |
+----------------+-----------------------+
```

* El slot 0 no se usa nunca: `CapHandle(0)` es el handle nulo.
* Al liberar un slot su generación aumenta. Un handle con generación vieja
  devuelve `BadHandle` en vez de tocar el objeto nuevo que ocupe el slot
  (evita el "use-after-free" de capacidades).
* La generación nunca da la vuelta: un slot que usó sus 4096 generaciones se
  retira. Una CSpace tiene como mucho `4 * límite + 1` slots, así que una cell
  puede emitir unos 16 millones de handles en su vida; después, `NoMemory`.
  Demostrado en `CSpace.stale_handle_never_revives` (verification/lean).
* Los handles son locales: el número no significa nada fuera de su cell.

### 3.1 Derechos

| Bit | Derecho | Significado |
|---|---|---|
| 0 | `SEND` | enviar por el canal |
| 1 | `RECV` | recibir del canal |
| 2 | `READ` | mapear lectura / leer puertos |
| 3 | `WRITE` | mapear escritura / escribir puertos |
| 4 | `EXEC` | instanciar imagen (no vale para memoria) |
| 5 | `GRANT` | transferir la capacidad por IPC |
| 6 | `DUPLICATE` | derivar una copia atenuada |
| 7 | `CONTROL` | matar/actualizar cell, ack de IRQ |
| 8 | `WAIT` | observar señales con `wait` |

**Regla de monotonía:** los derechos solo se reducen. `cap_dup(h, mask)`
produce `derechos(h) ∩ mask` y exige `DUPLICATE`. No existe operación que
agregue bits.

**Cells selladas:** una cell solo ejecuta el código de su imagen. `EXEC` vale
para un objeto `Image` (`cell_spawn`), no para un objeto `Memory`: ninguna
capacidad de memoria lo tiene y `mem_map` rechaza pedirlo (`InvalidArgs`),
así que todo objeto de memoria se mapea sin permiso de ejecución. Como el
mismo objeto puede estar mapeado escribible en otra dirección o en otra
cell, W^X por mapeo no alcanzaría: hace falta que la memoria compartible no
se ejecute nunca. Modelo: `Mem.step_sealed` (`verification/lean/Nsos/Memory.lean`).

### 3.2 Revocación

En v1 la revocación es por **destrucción**: cerrar un extremo de canal o
matar una cell invalida todo lo que depende de ello (el peer ve
`PEER_CLOSED`). La revocación selectiva (árbol de derivación tipo seL4) queda
para una versión posterior; el formato del handle ya la permite.

## 4. Convención de syscalls (x86_64)

* Instrucción `syscall`. Número en `rax`.
* Argumentos: `rdi, rsi, rdx, r10, r8, r9`.
* Retorno en `rax` como `i64`: `>= 0` éxito, `< 0` es `-código_error`.
* Se preservan todos los registros salvo `rax`, `rcx`, `r11`.
* Todo puntero de usuario se valida (rango del espacio de usuario, alineación,
  páginas mapeadas con el permiso adecuado) y se **copia** antes de usarse,
  para evitar TOCTOU con otros hilos de la misma cell.

Tabla completa en [`syscall.rs`](../crates/abi/src/syscall.rs). Estado de
implementación (hito 7): `debug_log`, `yield`, `exit`, `clock_monotonic`,
`cap_dup`, `cap_drop`, `cap_info`, `channel_create`, `channel_send`,
`channel_recv`, `mem_create`, `mem_map`, `mem_info`, `ioport_read`, `ioport_write`,
`cell_spawn`, `cell_kill`, `cell_status`, (hito 12) `wait` e `irq_ack`, y
(hito 13) `channel_call` y `mem_unmap`: todas las del ABI v1. Después
(contratos, etapa 5) `channel_contract`, y `channel_create` toma el contrato
como segundo argumento; los dos cambios son compatibles con lo anterior.
Resumen:

| Rango | Subsistema | Llamadas |
|---|---|---|
| 0x | básicas | `debug_log`, `yield`, `exit`, `clock_monotonic` |
| 1x | capacidades | `cap_dup`, `cap_drop`, `cap_info` |
| 2x | canales | `channel_create`, `channel_send`, `channel_recv`, `channel_call`, `channel_contract` |
| 3x | espera | `wait` |
| 4x | memoria | `mem_create`, `mem_map`, `mem_unmap`, `mem_info` |
| 5x | cells | `cell_spawn`, `cell_kill`, `cell_status` |
| 6x | hardware | `irq_ack`, `ioport_read`, `ioport_write` |

Errores: `NoSys, BadHandle, WrongType, AccessDenied, InvalidArgs, WouldBlock,
PeerClosed, NoMemory, TimedOut, BadMessage, Unsupported`
([`error.rs`](../crates/abi/src/error.rs)).

## 5. Canales IPC

`channel_create` devuelve dos handles conectados, cada uno con
`SEND|RECV|GRANT|DUPLICATE|WAIT`. Cada extremo tiene una cola acotada (64
mensajes en v1) de mensajes que **le llegan**.

### 5.1 Formato de mensaje (256 bytes fijos)

```
offset  tamaño  campo
0       8       label        (u64, lo define el protocolo)
8       2       data_len     (<= 224)
10      1       cap_count    (<= 4)
11      1       flags        (0)
12      4       reserved     (0)
16      16      caps[4]      (CapHandle; los no usados en 0)
32      224     data
```

Un tamaño fijo hace que la copia sea una operación acotada y predecible, que
la cola sea un arreglo sin fragmentación y que la validación sea total.

Reglas de validación (`Message::validate`), aplicadas a todo mensaje que
entra desde una cell:

* `data_len <= 224`, `cap_count <= 4`, `flags == 0`, `reserved == 0`.
* Los `cap_count` primeros handles no son nulos ni se repiten.
* Los slots de capacidad no usados valen 0 (no se filtra basura).

### 5.2 Semántica

* **send** copia el mensaje a la cola del peer. Si está llena: bloquea, o
  `WouldBlock` con `NONBLOCK`. Si el peer cerró: `PeerClosed`.
* **Las capacidades adjuntas se mueven.** Al enviar, el kernel las quita del
  CSpace del emisor y, al recibir, las inserta en el del receptor con handles
  nuevos. Cada una debe tener `GRANT`; si alguna falla, el envío entero falla
  y el emisor no pierde nada (operación atómica).
* **recv** saca el mensaje más antiguo. Cola vacía: bloquea o `WouldBlock`.
* **call** (`channel_call(h, msg, timeout_ns)`) = send + recv de la
  respuesta en el mismo extremo, en una syscall; la respuesta se escribe
  sobre `msg`. Requiere `SEND` y `RECV`, comprobados antes de enviar, y `msg`
  no puede adjuntar el propio `h` (`InvalidArgs`: la cell lo perdería y
  esperaría en un extremo que ya no tiene). El plazo cubre las dos mitades,
  como en `wait` (`0` no espera, `WAIT_FOREVER` no vence). Si el envío falla
  no se mandó nada; si falla la espera (`TimedOut`, `PeerClosed`), el pedido
  **ya** se mandó. La respuesta es el próximo mensaje que llegue a `h`:
  emparejar pedidos y respuestas es cosa del protocolo. No agrega pasos al
  modelo (`Sys.call_inv`). Todavía sin *handoff* directo al servidor.
* Orden FIFO por canal. Sin garantías de orden entre canales distintos.
* **Varios hilos pueden esperar en el mismo extremo.** Un `send` despierta a
  todos los que esperaban recibir; un `recv` despierta a todos los que
  esperaban lugar; al cerrarse un extremo se despierta a todos los que
  esperaban en él o en su peer. Los despertados reintentan. Demostrado: ningún
  mensaje se pierde ni se duplica (`Sys.step_msgInv`) y ninguna cell queda
  bloqueada si ya podría avanzar (`Sys.step_wakeInv`).
* **Ciclos.** Un extremo puede viajar en la cola de otro, incluso en la suya.
  Si un grupo de extremos queda referenciado solo desde colas que nadie puede
  leer, el kernel lo detecta (recolector de extremos en vuelo, como el de
  sockets Unix en Linux) y lo cierra: sus colas se descartan y los peers ven
  `PeerClosed`. Corre tras `send` con capacidades, `cap_drop` y al morir una
  cell. Demostrado en `Sys.step_no_leak` (verification/lean), y que nunca
  cierra un extremo alcanzable en `Sys.gc_keeps_reachable`.

### 5.3 Zero-copy seguro

Para datos mayores a 224 bytes:

1. El emisor crea un objeto `Memory` (`mem_create`), lo mapea y escribe.
2. Lo adjunta al mensaje. Como las capacidades se *mueven*, el kernel
   **desmapea** el objeto del emisor antes de entregarlo.
3. El receptor lo mapea en su espacio.

En ningún momento dos cells tienen el mismo marco mapeado como escribible: es
transferencia de propiedad, no memoria compartida. Esto es el equivalente
hardware del *exchange heap* de Singularity y no depende de que la cell
emisora sea "buena". Compartir memoria de solo lectura (p. ej. código) se
logra con `cap_dup` sin `WRITE`.

### 5.4 Contratos

El kernel no interpreta `label` ni `data`: qué mensajes valen en cada
momento lo dice el **contrato** del protocolo, que se escribe con la macro
`nsos_contract::contract!` y se comprueba al compilar (estados de una sola
dirección, determinismo, cota de mensajes en vuelo menor o igual que los 64
de la cola). Una cell tipada no puede mandar fuera de protocolo, y lo que
recibe fuera de protocolo lo descarta soltando sus capacidades. El formato
en el cable es este mismo, así que una cell sin contrato puede hablar con
una tipada. Ver [`contratos.md`](contratos.md).

Cada canal guarda el **contrato** que declaró quien lo creó
(`channel_create(out, contrato)`; 0 = sin contrato, menor que 2^63) y
`channel_contract(h)` lo devuelve. Lo comparten los dos extremos y ninguna
syscall lo cambia (`Sys.step_contract_fixed` en
`verification/lean/Nsos/ContractId.lean`). Una cell tipada exige que un
canal que abre o recibe tenga el contrato que espera: nadie puede hacer
pasar un canal de un protocolo por otro. El número identifica, no
autentica: quien crea el canal declara lo que quiere.

## 6. Espera y async

`wait(items, n, timeout_ns) -> n_listos` recibe un arreglo de `n` (1 a 16)
`WaitItem { handle, wanted, observed, reserved }` (16 bytes cada uno) y vuelve
cuando algún ítem tiene activa una señal pedida. Devuelve cuántos ítems están
listos y escribe en `observed` de **cada** ítem todas las señales activas de su
objeto (pedidas o no).

| Tipo | Señales | Cuándo está activa |
|---|---|---|
| `Channel` | `READABLE` | hay un mensaje en la cola de este extremo |
| | `WRITABLE` | el peer está abierto y su cola tiene lugar (`send` no bloquearía) |
| | `PEER_CLOSED` | el otro extremo se cerró |
| `Cell` | `CELL_EXITED` | la cell terminó (`cell_status` ya no bloquearía) |
| `Irq` | `IRQ_PENDING` | llegó una interrupción y todavía no hubo `irq_ack` |

* Cada handle necesita `WAIT`; `wanted` no puede ser 0 ni pedir señales que
  su tipo no emite (`Memory`, `IoPorts` e `Image` no emiten ninguna);
  `reserved` va en 0. Un ítem malo invalida la llamada entera, antes de
  esperar (`BadHandle`, `AccessDenied` o `InvalidArgs`).
* `timeout_ns` es relativo: `0` solo mira (`TimedOut` si nada está listo) y
  `WAIT_FOREVER` (`u64::MAX`) no vence. Si vence: `TimedOut`, con `observed`
  igual escrito. La granularidad es la del timer (10 ms).
* Si se pide matar a la cell (`cell_kill`), la espera se abandona.
* **Sin despertares perdidos.** El kernel marca al hilo bloqueado, después
  recorre los ítems registrándolo y mirando cada uno bajo el lock de su
  objeto (si alguno ya está listo se despierta a sí mismo) y recién entonces
  cede la CPU; quien activa una señal despierta a todos los registrados.
  Demostrado en [`Nsos/Wait.lean`](../verification/lean/Nsos/Wait.lean)
  (`no_lost_wakeup`): un hilo dormido con un ítem listo ya está despierto.
  Pueden quedar registros viejos (un despertar de más), inofensivos porque
  toda espera del kernel vuelve a mirar.

El runtime de la cell construye encima un executor: cada `Future` que espera
un canal registra su handle; el executor llama a `wait` con todos y despierta
las tareas correspondientes. Sin hilos del kernel por petición y sin GC.

## 7. Cells

* `cell_spawn(image, caps, n, quota)` exige `EXEC` sobre la imagen y `GRANT` sobre
  cada capacidad (hasta 16). Valida el ELF *antes* de quitarle nada a quien
  llama, crea un espacio de direcciones nuevo, carga la imagen y **mueve**
  las `n` capacidades al CSpace de la hija (slots 1..n en ese orden: es su
  "vector de arranque"). Devuelve una capacidad `Cell` con
  `CONTROL | WAIT | DUPLICATE | GRANT`. El lugar de esa capacidad se reserva
  en el CSpace de quien llama *antes* de crear la hija (`CSpace::reserve`):
  si no hay lugar, falla con `NoMemory` sin crear nada, y nunca queda una
  hija corriendo sin una capacidad que la controle.
* La hija recibe `quota` marcos de cuota de memoria, que salen de la cuota
  disponible de quien llama (sin tantos, `NoMemory`). Con ella paga todo lo
  suyo: tablas de páginas, imagen, pila y los objetos de `mem_create` (que
  se le cobran aunque viajen a otra cell). Al terminar, lo que le queda
  disponible vuelve a quien la creó, y lo que sigue en uso pasa a contarse
  como suyo hasta que se libere. Cada marco de cuota trae además 512 B de
  memoria del kernel, con la que la hija paga lo que el kernel gasta por
  ella: unos 210 KiB fijos (pila de kernel y CSpace), cada mensaje que
  encola hasta que se recibe (432 B), cada canal que crea (1,5 KiB) y cada
  mapeo. `nsos_rt::cell_spawn` da 512 marcos (2 MiB y 256 KiB de memoria
  del kernel); `cell_spawn_with_quota` elige la cuota.
* Una cell no hereda nada que no se le pase explícitamente.
* Si la cell falla (excepción de CPU, `panic`, violación de ABI), el kernel la
  termina y libera sus capacidades (los peers ven `PEER_CLOSED`). El kernel
  sigue funcionando: así se contienen fallos (hito 6).
* `cell_status(h, out, flags)` espera a que la cell termine (requiere `WAIT`)
  o, con `NONBLOCK`, responde enseguida. Escribe
  `CellStatus { state, code, vector, addr }` (24 bytes) con `state` en
  `RUNNING`, `EXITED` (`code`), `FAULTED` (`vector`, `addr`) o `KILLED`.
* `cell_kill(h)` (requiere `CONTROL`) pide terminarla y vuelve enseguida. La
  cell muere desde su propio hilo antes de volver a anillo 3, nunca a mitad
  de una syscall.
* Un **supervisor** (cell normal con capacidades `Cell` de sus hijas) decide
  si reinicia. Al reiniciar crea canales nuevos: los extremos de la cell
  muerta ya se cerraron.

## 8. Actualización en vivo (protocolo, no syscall)

Se implementa encima de las primitivas anteriores, por cell. Los mensajes
son los de los contratos `handover` (supervisor ↔ versión vieja) y
`restore` (supervisor ↔ versión nueva) de `nsos_contract::protocols`; ver
[`contratos.md`](contratos.md). En `restore` el estado es un `Mem<RO>`: el
tipo no admite la capacidad escribible.

1. El supervisor arranca la versión nueva **sin** capacidades de servicio.
2. Envía `UPGRADE_PREPARE` a la cell vieja por su canal de control.
3. La vieja deja de aceptar trabajo nuevo, serializa su estado en un objeto
   `Memory` y responde `UPGRADE_STATE` adjuntando ese objeto **y** los
   extremos de canal que estaba sirviendo. No termina: espera la decisión.
4. El supervisor reenvía a la nueva, con `UPGRADE_RESTORE`, los extremos y
   una copia **de solo lectura** del objeto de estado (`cap_dup` sin
   `WRITE`). Se queda con la capacidad escribible y con una copia de cada
   extremo hasta que la nueva decida. La nueva valida el estado (el kernel
   no la deja escribirlo) y responde `UPGRADE_ACCEPT` (se queda con los
   extremos y copia el estado a un objeto propio) o `UPGRADE_REJECT`
   (devuelve las capacidades sin usarlas).
   Tras aceptar, el supervisor manda `UPGRADE_COMMIT` a la vieja, que
   termina.
5. Los clientes no notan el cambio: sus extremos de canal siguen siendo los
   mismos; solo cambió quién tiene el otro extremo. Los mensajes encolados
   viajan con el extremo.
6. Si la nueva rechaza, el supervisor devuelve estado y canales a la vieja
   con `UPGRADE_ROLLBACK` y la vieja retoma: vuelve a mapear el objeto (al
   enviarlo perdió el mapeo) y relee el estado (rollback).
   Si la nueva **se cae** antes de decidir, su canal de control se cierra y
   el supervisor lo ve como `PEER_CLOSED`. Entonces hace el mismo rollback
   con lo que guardó: el estado, que nadie más pudo escribir, y la copia de
   los extremos, que los mantuvo abiertos. Los clientes no ven nada.
   Demostrado en `Upgrade.crash_rolls_back` y `Upgrade.restoring_mem_at_sup`.
   Si se cae *después* de aceptar, la vieja ya terminó: corresponde
   reiniciarla (hito 6).

Modelo y demostraciones: [`Nsos/Upgrade.lean`](../verification/lean/Nsos/Upgrade.lean).

Implementado en el hito 7 con `init` (supervisor), `contador-v1`,
`contador-v2`, `contador-roto` (rechaza, o con `caer` intenta escribir el
estado y el kernel la termina) y `cliente`. El formato del estado lo define cada servicio
(con un número mágico de versión, `CNT1`/`CNT2`) y la versión nueva es la
que sabe migrar desde la anterior.

### 8.1 Migración a otra máquina

La migración (hito 10) es el mismo protocolo con la versión nueva en otra
máquina. Un `enlace` (cell con un canal autenticado sobre la red) hace de
versión nueva para
el supervisor de origen: recibe `UPGRADE_RESTORE [Memory, extremo]` con el
*nombre* de la versión a arrancar del otro lado, manda por la red una
**copia** del estado y contesta `UPGRADE_ACCEPT` o `UPGRADE_REJECT [Memory,
extremo]` según lo que decida la otra máquina. Si aceptó, se queda con el
extremo del cliente y reenvía cada pedido por la red. Del otro lado, el
`enlace` crea un `Memory` y un canal locales y se los pasa a su supervisor
(`destino`) con `UPGRADE_RESTORE`. Por la red no viajan capacidades: un
mensaje del cliente con capacidades llega sin ellas.

Modelo y demostraciones: [`Nsos/Migration.lean`](../verification/lean/Nsos/Migration.lean).

El enlace (`crates/link`) autentica cada trama con HMAC-SHA256 y una clave
de sesión derivada de una clave compartida y de un nonce de cada punta;
acepta solo la secuencia siguiente, en la dirección hacia él. Modelo:
[`Nsos/Link.lean`](../verification/lean/Nsos/Link.lean).

Las etiquetas están en `nsos_abi::msg::labels` (rango
`0xFFFF_0000_0000_0000..` reservado al sistema).

## 9. Drivers en user-space

Un driver es una cell que recibe en su vector de arranque:

* `IoPorts` o `Memory` (MMIO) del dispositivo,
* `Irq` de su línea de interrupción,
* un canal hacia sus clientes.

El kernel solo enmascara la IRQ, activa `IRQ_PENDING` y espera `irq_ack`.

### 9.1 Interrupciones (hito 12)

* La línea es **por nivel**: el dispositivo la mantiene activa hasta que el
  driver lo atiende (en virtio legacy, leyendo el registro ISR). Cuando llega,
  el kernel la enmascara en el controlador, activa `IRQ_PENDING` en el objeto
  `Irq` (despierta a quien espera en `wait`) y no toca el dispositivo.
* `irq_ack(h)` (requiere `CONTROL`) borra `IRQ_PENDING` y desenmascara, en un
  solo paso. Si el dispositivo todavía tiene algo, la interrupción vuelve a
  llegar. Mientras no haya `irq_ack`, no vuelve (sin tormentas). Un
  `irq_ack` sin nada pendiente no hace nada.
* El driver: mira su anillo; si no está lo que espera, `wait` sobre
  `IRQ_PENDING` (y, si quiere, sus canales); al despertar lee el ISR y hace
  `irq_ack`. Si el dispositivo termina entre la mirada y la espera,
  `IRQ_PENDING` ya está activa y `wait` vuelve enseguida.
* **Una línea, un dueño.** El kernel reclama la línea al crear el objeto y la
  suelta (enmascarada) cuando muere la última capacidad. La capacidad tiene
  solo `CONTROL | WAIT`: sin `GRANT` ni `DUPLICATE`, no sale del driver.
  Si dos dispositivos comparten línea, solo el primer driver la recibe; el
  otro no recibe `Irq`, sondea y le pide a su dispositivo que no interrumpa
  (`VRING_AVAIL_F_NO_INTERRUPT`).
* De dónde sale la línea: en x86_64, el pin del IOAPIC (en `0xFEC00000`) que
  anotó el firmware en `Interrupt Line`; vector `48 + pin`. En ARM64, la SPI
  del GIC que da el `interrupt-map` del controlador PCI en el device tree
  (solo para el bus raíz).
* Los drivers virtio reciben la `Irq` en el slot 5, si se pudo entregar.

Modelo y demostraciones: [`Nsos/Irq.lean`](../verification/lean/Nsos/Irq.lean)
(`no_storm`, `no_lost_irq`, `ack_needs_owner`, `unowned_silent`,
`claim_exclusive`); la espera en sí, `Nsos/Wait.lean`.

* `mem_map(h, vaddr, rights)`: mapea el objeto entero en `vaddr` (alineada a
  página, en el rango de usuario, sin pisar otro mapeo). Exige `READ` en la
  capacidad y los derechos pedidos, que son `READ` o `READ | WRITE`: pedir
  `EXEC` (o cualquier otro bit) es `InvalidArgs`, y el mapeo es siempre sin
  permiso de ejecución (cells selladas, ver §3.1). El mapeo
  dura mientras la cell tenga una capacidad al objeto con esos derechos: si
  la envía, la pasa a una cell hija o la suelta, el kernel lo quita
  (revocación, demostrada en `Mem.step_iso`).
* `mem_unmap(vaddr, len)`: quita el mapeo de un objeto que empieza en
  `vaddr` y mide exactamente `len` (un objeto se desmapea entero, como se
  mapeó). Cualquier otra cosa es `InvalidArgs`: medio objeto, el código, la
  pila, el kernel, una dirección libre. La capacidad no cambia: se puede
  volver a mapear. Modelo: `Mem.unmap_removes`, `Mem.unmap_only_shrinks`;
  Kani: `unmap_target_exact`.
* `mem_info(h, out)`: escribe `MemInfo { size, phys }`; `phys` es 0 salvo
  que la capacidad tenga `CONTROL`.
* `ioport_read(h, off, width)` / `ioport_write(h, off, width, v)`: `width`
  es 1, 2 o 4 y `off` es relativo al rango concedido; `off + width` debe
  caber en él. Un driver nunca nombra puertos absolutos.
Para DMA (virtio) el driver recibe objetos `Memory` cuyas direcciones físicas
conoce el kernel; sin IOMMU el driver sigue siendo de confianza respecto al
DMA, y así se documenta en el [modelo de amenazas](amenazas.md).

## 10. Versionado

`ABI_VERSION = 1`. Cambios incompatibles incrementan la versión; las cells
declaran la versión con la que se compilaron en una nota ELF y el kernel
rechaza versiones desconocidas.

## 11. Pendiente / preguntas abiertas

* Revocación selectiva (árbol de derivación).
* Tamaño de cola configurable por canal.
* Canales entre nodos (fase 2): un *proxy* cell serializa mensajes y traduce
  capacidades `Memory` a copias; las capacidades no transferibles por red
  (`Irq`, `IoPorts`) se rechazan.
