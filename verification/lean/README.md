# Verificación formal en Lean 4

Modelo del sistema de capacidades y del IPC de nsos, con demostraciones de
sus propiedades de seguridad. No verifica el código Rust directamente: lo
conecta con el modelo mediante **tests derivados del modelo** (ver abajo).

## Qué hay

| Archivo | Contenido |
|---|---|
| `Nsos/Rights.lean` | Derechos y atenuación (`nsos_abi::cap::Rights`). |
| `Nsos/Handle.lean` | Handles `[gen:12 \| slot:20]` y errores del ABI. |
| `Nsos/CSpace.lean` | Modelo ejecutable de `nsos_kcore::cspace::CSpace`, paso a paso. |
| `Nsos/System.lean` | Cells, canales y syscalls `cap_dup`, `cap_drop`, `channel_create`, `channel_send`, `channel_recv`, bloqueo (`waitRecv`, `waitSend`) y recolección de extremos inalcanzables. |
| `Nsos/Security.lean` | Teoremas de seguridad. |
| `Nsos/Upgrade.lean` | Protocolo de actualización en vivo con rollback (supervisor, versión vieja y nueva). |
| `Nsos/Migration.lean` | Migración de un servicio a otra máquina: el mismo protocolo con la versión nueva del otro lado de un cable (hito 10). |
| `Nsos/Link.lean` | Enlace autenticado entre máquinas: un adversario que controla la red no logra que se acepte nada falso, repetido ni fuera de orden. |
| `Nsos/Ipc.lean` | Teoremas del IPC completo: colas, bloqueo y recolector. |
| `Nsos/Memory.lean` | Aislamiento de memoria: marcos, objetos `Memory`, mapeos y revocación. |
| `Nsos/Examples.lean` | Escenarios evaluados al compilar (ciclos de canales, dos cells esperando, handles viejos). |
| `TraceGen.lean` | Genera `crates/kcore/tests/data/cspace_model.trace`. |

## Teoremas demostrados

Todos sin `sorry` ni axiomas extra.

* `Rights.attenuate_subset`: derivar una capacidad nunca agrega derechos.
  `Rights.attenuate_needs_duplicate`: sin `DUPLICATE` no se deriva nada.
* `CSpace.get_after_remove`: un handle quitado deja de valer de inmediato.
* `CSpace.removeAll_rights`: `remove_all` solo entrega capacidades que estaban
  en la CSpace y tenían el derecho pedido.
* `Sys.step_other_cells`: una syscall de una cell no toca la CSpace de otra.
* `Sys.step_no_amplification` y `Sys.run_no_amplification`: en cualquier
  ejecución, toda capacidad que existe al final (en una CSpace o en una cola)
  viene de una del estado inicial con derechos iguales o mayores, o apunta a un
  objeto creado durante la ejecución.
* `Sys.send_requires_grant`: un `send` exitoso mueve capacidades del emisor que
  tenían `GRANT`, y viajan con los mismos derechos.
* `Sys.no_forging`: una cell que nunca hace `recv` termina solo con capacidades
  a objetos que ya tenía (con derechos iguales o menores) o a objetos nuevos.
  Nadie puede fabricar una capacidad ni recibirla sin un `recv` propio.
* `CSpace.stale_handle_never_revives`: después de quitar un handle, ninguna
  secuencia de operaciones lo vuelve a hacer válido.
* `Sys.step_no_leak`: después de cada syscall, todo extremo que ya nadie puede
  alcanzar está cerrado y con la cola vacía.
* `Upgrade.client_sees_consecutive`: con cualquier intercalado de pedidos,
  actualizaciones aceptadas y rechazos con rollback, el cliente ve el
  contador `1, 2, …, n` sin saltos ni repeticiones.
* `Upgrade.server_holds_everything`: la versión que atiende tiene el estado y
  el extremo del cliente; nunca atienden dos versiones a la vez.
* `Upgrade.rollback_resumes` y `Upgrade.accept_continues`: la versión que
  retoma (vieja tras un rollback, nueva tras aceptar) sigue la cuenta desde
  la última respuesta del cliente. Si la nueva escribiera el estado antes de
  decidir, o la vieja no lo releyera al retomar, la demostración falla (se
  probó mutando el modelo).
* `Migration.client_sees_consecutive`: lo mismo cuando el servicio se muda
  a otra máquina y el enlace reenvía los pedidos por el cable, con
  migraciones rechazadas (rollback) y aceptadas en el medio.
* `Migration.one_server`: nunca atienden las dos máquinas a la vez; mientras
  atiende la remota, el estado y el extremo del cliente están en el enlace.
* `Migration.rollback_resumes` y `Migration.remote_steps_local_untouched`:
  la otra máquina solo recibe una *copia* del estado, así que tras un
  rechazo la versión vieja retoma con exactamente la cuenta del cliente.
  `Migration.remote_accept_continues`: la versión remota arranca desde el
  último valor que vio el cliente. Tres mutaciones del modelo (copiar mal
  el estado, arrancar la remota corrida en uno, reenviar sin tener el
  extremo) hacen fallar las demostraciones.
* `Link.delivered_prefix`: contra un adversario que descarta, reordena,
  repite, refleja e inyecta tramas (sin poder falsificar el MAC), lo que el
  receptor entrega en cada dirección es un prefijo de lo que mandó la otra
  punta en la sesión actual. Corolarios: `Link.no_forgery`,
  `Link.no_replay` y `Link.in_order`. Quitar cualquiera de las cuatro
  comprobaciones del receptor (etiqueta, sesión, dirección, secuencia
  exacta) hace fallar la demostración.

### IPC completo (`Nsos/Ipc.lean`)

El modelo sigue cada mensaje con un número propio y registra qué fue de él
(`unsent`, `queued o`, `delivered`, `dropped`).

* `Sys.step_msgInv`: **ningún mensaje se pierde ni se duplica**. Todo mensaje
  enviado está exactamente una vez en exactamente una cola, o se entregó, o se
  descartó porque su extremo quedó inalcanzable (nadie podía recibirlo).
* `Sys.step_fate_final`: entregado y descartado son finales. Un mensaje se
  entrega a lo sumo una vez.
* `Sys.mem_reachSet` y `Sys.gc_keeps_reachable`: el recolector calcula
  exactamente lo alcanzable y **nunca cierra un extremo vivo** ni le vacía la
  cola.
* `Sys.step_wakeInv`: **sin despertares perdidos**. Toda cell bloqueada está
  registrada en el extremo que la va a despertar, y su condición de espera
  sigue sin cumplirse (cola vacía y peer abierto, o cola de destino llena y
  abierta). Apenas la condición se cumple, la cell ya no está bloqueada.
* `Sys.run_inv`: los tres invariantes valen en toda ejecución que arranca de un
  estado inicial (`Sys.inv_init`).

### Aislamiento de memoria (`Nsos/Memory.lean`)

Cada marco físico tiene un dueño (kernel, libre, una cell o un objeto
`Memory`), y cada cell una lista de mapeos.

* `Mem.step_iso`: en cada paso (crear, mapear, desmapear, duplicar, soltar,
  enviar, liberar un objeto, crear o terminar una cell), todo mapeo de una
  cell apunta a un marco propio o a un marco de un objeto del que la cell
  tiene una capacidad con los derechos del mapeo.
* `Mem.never_maps_foreign`: una cell nunca mapea un marco del kernel, uno
  libre (tampoco memoria ya liberada) ni uno propio de otra cell.
* `Mem.write_needs_cap`: un mapeo escribible exige una capacidad con `WRITE`.
* `Mem.send_revokes`: quien envía su única capacidad a un objeto pierde todos
  sus mapeos de ese objeto.
* `Mem.no_revocation_breaks`: sin revocación (el kernel antes de este cambio)
  el invariante se rompe.

## Tests derivados del modelo

`lake exe tracegen` ejecuta el modelo de la CSpace con unas 4800 operaciones
pseudoaleatorias (deterministas) y anota el resultado esperado de cada una.
`crates/kcore/tests/model_traces.rs` repite esas operaciones sobre la `CSpace`
real y exige resultados idénticos: handles, errores, objetos, derechos y
cantidad de capacidades vivas. La CI regenera la traza y falla si el archivo
del repo no coincide con lo que produce el modelo.

El IPC del kernel (`kernel/src/cell.rs`) todavía no tiene este puente porque
depende del scheduler. Para tenerlo hay que mover la lógica de colas de canal
a `kcore`.

## Límites que encontró el modelo (corregidos)

1. **Ciclos de referencias.** Un extremo que viaja solo en su propia cola, o
   un ciclo de extremos en colas ajenas, quedaba vivo para siempre con el
   conteo de referencias. Ahora `Sys.gc` cierra todo lo inalcanzable
   (`Sys.step_no_leak`) y el kernel lo implementa con `ipc_gc`
   (`crates/kcore/src/ipc_gc.rs` + `kernel/src/cell.rs`). Ver
   `Examples.selfLoop` y `Examples.twoCycle`.
2. **La generación daba la vuelta.** Con 12 bits, tras 4096 reusos un handle
   viejo volvía a valer. Ahora el slot se retira
   (`CSpace.stale_handle_never_revives`) y la última traza comprueba que Rust
   hace lo mismo. Ver `Examples.noWrap`.
3. **Despertares perdidos.** Cada extremo guardaba un solo hilo en espera. Si
   dos cells con el mismo extremo se bloqueaban, la segunda pisaba a la
   primera y esa no se despertaba nunca. Ahora el kernel guarda una lista y
   despierta a todos (`Sys.step_wakeInv`, `Examples.wakeBoth`).
4. **Mapeos que sobrevivían a la capacidad.** Quien enviaba o soltaba una
   capacidad `Memory` seguía viendo el objeto mapeado. Ahora el kernel revoca
   esos mapeos (`Cell::revoke_mappings`; `Mem.step_iso`,
   `Mem.no_revocation_breaks`).

## Uso

Requiere [elan](https://github.com/leanprover/elan); la versión de Lean está
fijada en `lean-toolchain`.

```sh
make verify                     # desde la raíz: demostraciones + trazas
# o, desde verification/lean:
lake build                      # compila y verifica todas las demostraciones
lake exe tracegen ../../crates/kcore/tests/data/cspace_model.trace
```
