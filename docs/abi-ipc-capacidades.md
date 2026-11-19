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
| `Memory` (2) | Conjunto de marcos físicos mapeable | `READ WRITE EXEC CONTROL` (`CONTROL`: conocer la dirección física, para DMA) |
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
| 4 | `EXEC` | mapear ejecutable / instanciar imagen |
| 5 | `GRANT` | transferir la capacidad por IPC |
| 6 | `DUPLICATE` | derivar una copia atenuada |
| 7 | `CONTROL` | matar/actualizar cell, ack de IRQ |
| 8 | `WAIT` | observar señales con `wait` |

**Regla de monotonía:** los derechos solo se reducen. `cap_dup(h, mask)`
produce `derechos(h) ∩ mask` y exige `DUPLICATE`. No existe operación que
agregue bits. Además `mem_map` nunca mapea una página como `WRITE` y `EXEC`
a la vez (W^X).

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
`cell_spawn`, `cell_kill` y `cell_status`. El resto devuelve `Unsupported`. Resumen:

| Rango | Subsistema | Llamadas |
|---|---|---|
| 0x | básicas | `debug_log`, `yield`, `exit`, `clock_monotonic` |
| 1x | capacidades | `cap_dup`, `cap_drop`, `cap_info` |
| 2x | canales | `channel_create`, `channel_send`, `channel_recv`, `channel_call` |
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
* **call** = send + recv de la respuesta en el mismo canal. Permite al
  scheduler hacer *handoff* directo al servidor (camino rápido de RPC).
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

## 6. Espera y async

`wait(items, n, timeout)` recibe un arreglo de `WaitItem { handle, wanted }`
y vuelve cuando alguna señal pedida está activa, rellenando `observed`.

Señales: `READABLE`, `WRITABLE`, `PEER_CLOSED`, `CELL_EXITED`, `IRQ_PENDING`.

El runtime de la cell construye encima un executor: cada `Future` que espera
un canal registra su handle; el executor llama a `wait` con todos y despierta
las tareas correspondientes. Sin hilos del kernel por petición y sin GC.

## 7. Cells

* `cell_spawn(image, caps, n)` exige `EXEC` sobre la imagen y `GRANT` sobre
  cada capacidad (hasta 16). Valida el ELF *antes* de quitarle nada a quien
  llama, crea un espacio de direcciones nuevo, carga la imagen y **mueve**
  las `n` capacidades al CSpace de la hija (slots 1..n en ese orden: es su
  "vector de arranque"). Devuelve una capacidad `Cell` con
  `CONTROL | WAIT | DUPLICATE | GRANT`.
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

Se implementa encima de las primitivas anteriores, por cell:

1. El supervisor arranca la versión nueva **sin** capacidades de servicio.
2. Envía `UPGRADE_PREPARE` a la cell vieja por su canal de control.
3. La vieja deja de aceptar trabajo nuevo, serializa su estado en un objeto
   `Memory` y responde `UPGRADE_STATE` adjuntando ese objeto **y** los
   extremos de canal que estaba sirviendo. No termina: espera la decisión.
4. El supervisor reenvía todo a la nueva con `UPGRADE_RESTORE`. La nueva
   valida el estado **sin escribirlo** y responde `UPGRADE_ACCEPT` (se queda
   con todo) o `UPGRADE_REJECT` (devuelve las capacidades sin usarlas).
   Tras aceptar, el supervisor manda `UPGRADE_COMMIT` a la vieja, que
   termina.
5. Los clientes no notan el cambio: sus extremos de canal siguen siendo los
   mismos; solo cambió quién tiene el otro extremo. Los mensajes encolados
   viajan con el extremo.
6. Si la nueva rechaza, el supervisor devuelve estado y canales a la vieja
   con `UPGRADE_ROLLBACK` y la vieja retoma: vuelve a mapear el objeto (al
   enviarlo perdió el mapeo) y relee el estado (rollback).
   Si la nueva *falla* con las capacidades, el rollback no es posible: el
   extremo se cierra y los clientes ven `PEER_CLOSED`.

Modelo y demostraciones: [`Nsos/Upgrade.lean`](../verification/lean/Nsos/Upgrade.lean).

Implementado en el hito 7 con `init` (supervisor), `contador-v1`,
`contador-v2`, `contador-roto` (rechaza) y `cliente`. El formato del estado lo define cada servicio
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

* `mem_map(h, vaddr, rights)`: mapea el objeto entero en `vaddr` (alineada a
  página, en el rango de usuario, sin pisar otro mapeo). Exige `READ` en la
  capacidad y los derechos pedidos; W^X se aplica también aquí. El mapeo
  dura mientras la cell tenga una capacidad al objeto con esos derechos: si
  la envía, la pasa a una cell hija o la suelta, el kernel lo quita
  (revocación, demostrada en `Mem.step_iso`).
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
