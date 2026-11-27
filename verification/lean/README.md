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
| `Nsos/Memory.lean` | Aislamiento de memoria: marcos, objetos `Memory`, mapeos y revocación; cells selladas (solo ejecutan su imagen). |
| `Nsos/Store.lean` | Almacén de objetos: consistencia ante caídas (copy-on-write) y aislamiento entre dueños (handles). |
| `Nsos/Contract.lean` | Contratos de canal tipados (`crates/contract`): autómata, dos puntas con colas FIFO, el control `check` y su corrección. |
| `Nsos/UpgradeContract.lean` | El protocolo de `Upgrade.lean` sigue los contratos `handover` y `restore`. |
| `Nsos/StoreContract.lean` | El almacén de `Store.lean` sigue los contratos `store_root` y `store_obj`. |
| `Nsos/MigrationContract.lean` | La migración de `Migration.lean` sigue los contratos `handover` y `restore`. |
| `Nsos/ContractId.lean` | El contrato de cada canal (`Sys.contract`) no cambia. |
| `Nsos/Quota.lean` | Cuotas de memoria por supervisor: cada cell gasta solo su cuota, `cell_spawn` le pasa parte de la suya a la hija y al morir vuelve al supervisor (modelo antes del código). |
| `Nsos/Examples.lean` | Escenarios evaluados al compilar (ciclos de canales, dos cells esperando, handles viejos). |
| `TraceGen.lean` | Genera `crates/kcore/tests/data/cspace_model.trace` y `crates/contract/tests/data/contract_model.trace`. |

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
* `Upgrade.crash_rolls_back` y `Upgrade.restoring_mem_at_sup`: si la
  versión nueva se cae antes de decidir, todo vuelve a la vieja con el estado
  intacto. Mientras valida, la capacidad escribible al estado la tiene el
  supervisor, así que ninguna escritura de la nueva (paso `scribble`) puede
  tocarlo.
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

### Almacén de objetos (`Nsos/Store.lean`)

El disco es una función de bloques; un superbloque válido promete la tabla
y los datos exactos (un hash ideal). Una caída deja cada escritura hecha, no
hecha o rota, en cualquier orden.

* `Store.crash_safe`: después de una caída en cualquier punto de una
  escritura, se recupera el catálogo viejo o el nuevo, nunca una mezcla.
* `Store.commit_good`, `Store.mount_good`, `Store.crash_then_mount`,
  `Store.format_good`: el invariante vale al formatear, al escribir, al
  montar y después de una caída, así que se puede seguir escribiendo.
* `Store.rw_handle_from_owner`, `Store.change_needs_owner`,
  `Store.untouched_without_owner`: un objeto del dueño `o` solo cambia por
  un handle escribible que abrió la raíz de `o`; las copias de solo lectura
  nunca escriben.
* Contraejemplos evaluados con `decide`: `overwrite_breaks` (datos sobre un
  bloque confirmado), `same_slot_breaks` (superbloque en la ranura actual),
  `same_seq_loses` (sin subir la secuencia) y `stale_resurrects` (montar sin
  limpiar la otra ranura resucita un estado nunca confirmado). Este último
  caso lo encontró el modelo antes de escribir el código.
* Mutaciones que rompen las demostraciones: además de las cuatro de arriba,
  que `DUP_RO` conserve la escritura y que `OPEN` tome el dueño del mensaje.

### Contratos de canal (`Nsos/Contract.lean`)

Un contrato es una tabla de transiciones `(origen, quién manda, etiqueta,
destino)`; el cliente la sigue tal cual y el servidor con los papeles
cambiados. `check` es el control que corre la macro al compilar.

* `Contract.contract_safe`: si `check` acepta con cota `k`, en toda
  ejecución de dos puntas que cumplen: el primero de cada cola lo puede
  recibir su destinatario (`head_receivable_a/b`), alguien puede avanzar
  salvo que las dos estén en el mismo estado final sin nada en vuelo
  (`progress`), una punta terminada no tiene mensajes sin leer
  (`finished_no_pending_a/b`) y nunca hay más de `k` mensajes en vuelo
  (`queue_bound`). La clave es el invariante `Inv`: una punta va adelantada
  por un camino de una sola dirección, los mensajes en vuelo son ese camino
  y la cola de la que va adelante está vacía.
* `check_sound`: lo que acepta `check` no mezcla direcciones, es
  determinista y está acotado.
* Contraejemplos: `mixed_breaks` (un estado en que mandan las dos puntas),
  `nondet_breaks` (una etiqueta a dos estados) y `loop_unbounded` (la cola
  crece sin límite) violan alguna propiedad, y `check` los rechaza.
* `echo_check`, `note_check`: los contratos de `ping`/`pong` tienen cota 1.
  `echoOld_unbounded`: el protocolo anterior (`NEW_CHANNEL` sin respuesta)
  no tenía cota.
* Mutaciones que rompen las demostraciones: quitar el determinismo o la
  regla de una sola dirección de `check`, acotar una sola dirección o
  correr la cota en uno, generar mal el dual (el servidor recibe en
  transiciones del servidor) y recibir sin mirar la etiqueta.

### La actualización sigue sus contratos (`Nsos/UpgradeContract.lean`)

* `hcfg`, `rcfg`: a cada fase de `Upgrade.lean`, la configuración del canal
  supervisor ↔ vieja (`handover`) y supervisor ↔ nueva (`restore`).
* `handover_follows`, `restore_follows`: cada paso del modelo es cero, uno o
  dos pasos de cada contrato (dos cuando el modelo junta un `recv` y un
  `send` de la misma cell). Excepción: la caída de la versión nueva, tras la
  cual el intento siguiente usa un canal nuevo.
* `handover_reachable`, `restore_reachable`, `upgrade_contract_safe`: en todo
  estado alcanzable, los dos canales están en configuraciones alcanzables
  de sus contratos y valen los teoremas de `Contract.lean` (cotas 2 y 1).
  `done_clean`: al terminar, los dos canales terminaron sin nada en vuelo.
* `counter_check`, `handover_check`, `restore_check`.
* Mutaciones que rompen la simulación: en los contratos, que `Reject`
  termine el canal, que `handover` no tenga `Rollback`, que la vieja
  termine al entregar o que `Restore` lleve a otro estado; en el modelo
  (compilado sin sus demostraciones propias, para aislar la simulación),
  `commit` antes de `accept`, `forward` sin haber recibido el estado y
  `resume` sin `rollback`.
  Para compilar así se escribe el `.olean` a mano (`lake env lean -o`).
  Lake decide si recompilar por el hash del fuente, no del `.olean`, así
  que después hay que borrar `.lake/build`: si no, queda el modelo mutado
  y recortado aunque el fuente esté bien. Por lo mismo, antes de usar
  `lake env lean` hay que hacer `lake build` de lo que se restauró de una
  mutación anterior: `lake env lean` usa el `.olean` que haya.

### El almacén sigue sus contratos (`Nsos/StoreContract.lean`)

* `answered_steps`: un pedido del cliente y una respuesta permitida son
  cuatro pasos del contrato, de reposo a reposo.
* `exec_follows`: cada pedido que el modelo de `Store.lean` atiende
  (`openObj`, `dupRo`, `write`) es un intercambio de su contrato con la
  respuesta de éxito; `rejected_follows`: los rechazados tienen la de
  error. `read_answered`, `stat_answered`: lo mismo para `Read` y `Stat`.
* `buffer_states_reply_only`: en los estados que esperan respuesta a un
  pedido con búfer solo se contesta con `Data`, `Written` o el error (en
  la crate, los tres llevan el búfer).
* `storeRoot_check`, `storeObj_check`, `storeShare_check`: cota 1.
* Mutaciones: quitar `Written` o el error de `Write`, contestar `Read` con
  `Written`, y que una respuesta no vuelva al reposo (estas dos las atrapa
  también la cota de `check`).

### Flujos (`Nsos/Contract.lean`)

* `streamB`: todas las transiciones salen del estado inicial y vuelven a
  él (las dos puntas mandan sin cambiar de estado).
* `stream_inv`, `stream_head_receivable_a/b`: en un flujo, todo lo que
  llega es esperado aunque los mensajes se crucen.
* `stream_unbounded`: pero la cola crece sin límite; por eso al menos una
  punta tiene que mandar sin bloquear (`try_send`).
* `net_is_stream`: `net` es un flujo y `check` lo rechaza.
* `console_check`, `block_check`: cota 1.
* Mutaciones: que `streamB` no exija volver al estado (rompe
  `stream_inv`) y que `Send` cambie de estado (rompe `net_is_stream`).

### La migración sigue sus contratos (`Nsos/MigrationContract.lean`)

* `handover_follows`, `restore_follows`: cada paso de la migración es cero,
  uno o dos pasos de cada contrato, sin excepciones (el canal con el
  enlace es el mismo en todos los intentos).
* `handover_reachable`, `restore_reachable`, `migration_contract_safe`.
* Mutaciones: que `Reject` termine el canal, y en el modelo (recortado,
  como en `UpgradeContract`) que el enlace tome los recursos sin
  `MIGRATE`, que `origen` haga commit sin que el enlace le pase el
  `ACCEPT` y que haga rollback sin que el enlace le devuelva los recursos.

### El contrato de cada canal (`Nsos/ContractId.lean`)

`Sys.contract` guarda, por par, el contrato que declaró `channel_create`
(`Op.create cid`).

* `step_contract_fixed`, `run_contract_fixed`: ninguna syscall cambia el
  contrato de un canal que ya existe (tampoco el recolector).
* `step_create_contract`: el canal nuevo tiene el contrato que se pidió.
* `peer_contract`: los dos extremos lo comparten.
* `held_contract`: un extremo que una cell tiene al final y ya existía al
  principio conserva su contrato.
* Mutaciones: que `cap_dup` reetiquete el canal y que `create` le cambie el
  contrato a todos.

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

Cells selladas (cada mapeo dice además si es ejecutable):

* `Mem.step_sealed` (y `Mem.step_iso_sealed`, junto con el aislamiento): en
  cada paso, todo mapeo ejecutable es de un marco propio de la cell (su
  imagen) y ningún mapeo de ninguna cell a ese marco es escribible.
* `Mem.exec_is_own_code`: lo que una cell ejecuta es un marco suyo; nunca un
  objeto `Memory`, ni un marco del kernel, libre o de otra cell.
* `Mem.code_never_writable`: nadie, ni la propia cell, escribe en su código.
* `Mem.obj_never_exec`: un objeto `Memory` nunca se ejecuta.
* `Mem.exec_obj_breaks`: con el `mem_map` de antes (que aceptaba `EXEC`), la
  cell mapea el mismo objeto RW y RX y el invariante se rompe.

Mutaciones que rompen `step_sealed`: que `mem_map` acepte `EXEC` aunque
nunca junto con `WRITE` (W^X solo por mapeo); que el cargador mapee dos veces
un mismo marco propio; que el cargador acepte segmentos W+X.

Cuotas de memoria (`Nsos/Quota.lean`, antes del código del kernel):

* `QuotaModel.step_inv`: en todo paso, los marcos libres son la suma de las
  cuotas disponibles de las cells vivas, y libres más usados es el total.
* `QuotaModel.avail_le_free` y `QuotaModel.alloc_ok`: la cuota disponible
  siempre está respaldada por marcos libres, y pedir hasta ella funciona.
* `QuotaModel.step_others`: un paso que no hace `x` ni mata a `x` no le baja
  la cuota disponible ni la saca de las vivas.
* `QuotaModel.no_starvation`: con cualquier secuencia de pasos ajenos, `x`
  sigue viva, con al menos la cuota que tenía, y la puede pedir entera.
* `QuotaModel.run_conserves`: desde el arranque, ningún marco se pierde.

Mutaciones que rompen `step_inv`: aceptar un pedido que se pasa por un
marco de la cuota, crear una hija con una cuota que se pasa por un marco de
la del supervisor, y no devolver los marcos usados de la hija al morir.

## Tests derivados del modelo

`lake exe tracegen` ejecuta el modelo de la CSpace con unas 4800 operaciones
pseudoaleatorias (deterministas) y anota el resultado esperado de cada una.
`crates/kcore/tests/model_traces.rs` repite esas operaciones sobre la `CSpace`
real y exige resultados idénticos: handles, errores, objetos, derechos y
cantidad de capacidades vivas. La CI regenera la traza y falla si el archivo
del repo no coincide con lo que produce el modelo.

Para los contratos, `tracegen` evalúa `Contract.check` sobre 412 contratos
(los contraejemplos y otros al azar) y exporta las tablas de `echo`,
`note`, `counter`, `handover`, `restore`, `store_root`, `store_obj`,
`store_share`, `console`, `block` y `net`, y el veredicto de `streamB`
(test `stream_check_agrees_with_model`). `crates/contract/tests/contract_model.rs` exige que la `const fn`
`nsos_contract::check` dé el mismo veredicto y la misma cota, y que las
tablas que genera la macro `contract!` sean las del modelo.

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
5. **Estados resucitados en el almacén.** Una escritura que se cae con la
   tabla y el superbloque escritos pero sin los datos deja una ranura que no
   valida. Si una escritura posterior reusaba esos bloques con los mismos
   datos y se caía antes de su superbloque, la ranura vieja quedaba válida
   y ganaba: un estado que nunca se confirmó. Ahora montar la borra
   (`Store.mount_good`, `Store.stale_resurrects`).
6. **W^X por mapeo, no por objeto.** `mem_map` aceptaba `EXEC` si la
   capacidad lo tenía y solo rechazaba un mapeo W+X. El mismo objeto podía
   quedar mapeado RW en una dirección (o en otra cell) y RX en otra: código
   escrito y ejecutado. No se alcanzaba porque ninguna capacidad de memoria
   tenía `EXEC`, pero nada lo garantizaba. Ahora `EXEC` no es un derecho de
   la memoria y `mem_map` lo rechaza (`Mem.step_sealed`,
   `Mem.exec_obj_breaks`).

## Uso

Requiere [elan](https://github.com/leanprover/elan); la versión de Lean está
fijada en `lean-toolchain`.

```sh
make verify                     # desde la raíz: demostraciones + trazas
# o, desde verification/lean:
lake build                      # compila y verifica todas las demostraciones
lake exe tracegen ../../crates/kcore/tests/data/cspace_model.trace \
    ../../crates/contract/tests/data/contract_model.trace
```
