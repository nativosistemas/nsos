# Contratos de canal tipados

Estado: **etapa 5** (todos los protocolos entre cells del sistema
migrados, salvo los ad hoc de las pruebas del supervisor, y el contrato de
cada canal guardado en el kernel). La fuente de verdad ejecutable es
[`crates/contract`](../crates/contract), y los modelos,
[`Contract.lean`](../verification/lean/Nsos/Contract.lean),
[`UpgradeContract.lean`](../verification/lean/Nsos/UpgradeContract.lean),
[`StoreContract.lean`](../verification/lean/Nsos/StoreContract.lean),
[`MigrationContract.lean`](../verification/lean/Nsos/MigrationContract.lean)
y [`ContractId.lean`](../verification/lean/Nsos/ContractId.lean).

## Qué son

En Singularity cada canal tenía un **contrato**: una máquina de estados que
decía qué mensajes podían ir en cada momento y en qué dirección, y el
compilador comprobaba que cada punta la respetara. En nsos el kernel solo
conoce mensajes de 256 bytes con una etiqueta (`header.label`); hasta ahora
cada protocolo era un puñado de constantes `u64` y cada cell comparaba
etiquetas y leía bytes a mano.

Un contrato de nsos se escribe una vez con la macro `contract!`:

```rust
contract! {
    pub contract echo {
        messages {
            Echo(1) { data: Bytes<224> },
            EchoReply(2) { data: Bytes<224> },
            NewChannel(100) { side: Endpoint<note::server::Start> },
            Adopted(101) {},
            Bye(999) {},
        }
        start Ready;
        client Ready { Echo => Waiting, NewChannel => Adopting, Bye => End }
        server Waiting { EchoReply => Ready }
        server Adopting { Adopted => Ready }
        end End;
    }
}
```

y genera los dos lados (`echo::client` y `echo::server`) desde la misma
tabla, así que el dual nunca se escribe a mano:

* Un tipo por estado. `Chan<T, echo::client::Ready>` solo deja mandar
  `Echo`, `NewChannel` o `Bye`. `send` **consume** el canal y devuelve el
  del estado siguiente, así que no se puede usar un estado viejo
  (*typestate*).
* Para cada estado en que se recibe, un `enum` (`echo::server::recv::Ready`)
  con una variante por mensaje posible, cada una con el canal en el estado
  siguiente. Un `match` sin todos los casos no compila.
* Los mensajes son structs con sus campos; los extremos de canal viajan
  tipados (`Endpoint<S>`), como en Singularity.

`T` es el transporte (las syscalls). En una cell es `nsos_rt::Sys`; en los
tests del host, un kernel simulado.

```rust
let ch = Chan::<Sys, echo::server::Ready>::open(Sys, bootstrap(1))?;
match ch.recv() {
    Ok(Ready::Echo(c, m)) => { c.send(echo::EchoReply { data: m.data })?; }
    Ok(Ready::NewChannel(c, m)) => { /* m.side: Endpoint<note::server::Start> */ }
    Ok(Ready::Bye(end, _)) => end.close(),
    Err(RecvError::Violation { chan, label }) => { /* descartado; `chan` sigue en Ready */ }
    Err(RecvError::WouldBlock(c)) => { /* solo con try_recv */ }
    Err(RecvError::Failed(e)) => { /* PeerClosed, etc.; el canal se cerró */ }
}
```

## Qué se comprueba al compilar

La macro corre `nsos_contract::check`, una `const fn`, sobre la tabla del
contrato. Si falla, el programa no compila:

| Regla | Por qué | Contraejemplo en el modelo |
|---|---|---|
| En cada estado manda una sola punta | Si las dos pueden mandar a la vez, cada una recibe algo que no espera | `Contract.mixed_breaks` |
| Una etiqueta lleva a un solo estado | Si no, las puntas quedan desfasadas | `Contract.nondet_breaks` |
| Hay una cota `k` de mensajes seguidos de una misma punta, y `k ≤ 64` | Sin cota, la cola del kernel (64 mensajes) se llena | `Contract.loop_unbounded` |
| En un mismo estado, dos mensajes no comparten etiqueta | El decodificador elige el mensaje por la etiqueta en el estado actual. En estados distintos sí pueden (el error del almacén va con o sin búfer según el estado) | — |
| Cada mensaje entra en un `Message` (224 bytes, 4 capacidades) | — | — |

La sintaxis ya impide algunos errores: un estado en `client` y en `server` a
la vez se define dos veces, y un mensaje repetido en un estado es una
variante repetida.

El control encontró un problema en el protocolo viejo de `ping`/`pong`:
`NEW_CHANNEL` volvía directo a `Ready`, así que un cliente podía mandarlo
sin límite y llenar la cola de `pong` (`Contract.echoOld_unbounded`). Ahora
`pong` contesta `Adopted`.

## Qué se garantiza (y qué no)

Si `check` acepta un contrato con cota `k`, para **dos puntas que lo
siguen** (`Contract.contract_safe`):

* **ningún mensaje inesperado**: el primero de cada cola siempre tiene una
  transición de recepción en el estado de su destinatario;
* **sin bloqueo de protocolo**: alguna punta puede avanzar, salvo que las
  dos estén en el mismo estado final sin nada en vuelo;
* **nada pendiente al terminar**: una punta en un estado final no tiene
  mensajes sin leer;
* **colas acotadas**: nunca hay más de `k` mensajes en vuelo, así que mandar
  nunca encuentra la cola llena.

Una punta tipada no puede violar el contrato: el compilador no la deja.
Pero **la otra punta puede ser cualquier ELF** (`docs/amenazas.md`): el
kernel no sabe de contratos. Por eso todo lo que llega se decodifica según
el estado, y lo que no corresponde (una etiqueta sin transición, datos o
capacidades de más o de menos, una capacidad que no es un canal donde se
espera uno) se descarta: se sueltan sus capacidades y el canal vuelve **en
el mismo estado** (`RecvError::Violation`). Kani lo demuestra para el
servidor de `echo` con cualquier mensaje (`decode_any`).

El contrato no es una barrera de aislamiento: lo hace cumplir la cell
tipada sobre sí misma. Frente a una punta que no cumple, los teoremas de
arriba no valen (por ejemplo, una punta que no lee puede hacer que la otra
se bloquee en `send`), pero la cell tipada nunca entra en un estado que el
contrato no prevé.

## Cómo se conecta con el modelo

* `Nsos/Contract.lean` define el autómata, la semántica de dos puntas con
  colas FIFO, el control `check` y los teoremas. Sin `sorry` ni axiomas
  nuevos.
* `lake exe tracegen` evalúa el `check` del modelo sobre más de 400
  contratos (los contraejemplos y otros al azar) y exporta las tablas de
  `echo` y `note` a `crates/contract/tests/data/contract_model.trace`.
* `crates/contract/tests/contract_model.rs` exige que la `const fn` de Rust
  dé el mismo veredicto y la misma cota en todos, y que las tablas que
  genera la macro sean las del modelo. La CI regenera la traza y falla si
  cambió.
* Kani (`crates/contract/src/proofs.rs`): `decode_any`, `restore_decode_any`,
  `store_reading_decode_any`, `try_send_any`, `roundtrip_echo`,
  `roundtrip_endpoint`, `check_any`.

La macro numera los estados como el modelo: primero los estados `client`,
después los `server` y al final los `end`, cada grupo en el orden en que se
declaró.

## La actualización en vivo (etapa 2)

El protocolo de la sección 8 de [`abi-ipc-capacidades.md`](abi-ipc-capacidades.md)
son tres contratos (`nsos_contract::protocols`):

| Contrato | Cliente ↔ servidor | Tabla | Cota |
|---|---|---|---|
| `counter` | `cliente` ↔ contador | `Serving: Inc → Waiting`; `Waiting: Count → Serving` | 1 |
| `handover` | supervisor ↔ versión vieja | `Serving: Prepare → Handing`; `Handing: State → Deciding`; `Deciding: Commit → End`, `Rollback → Serving` | 2 |
| `restore` | supervisor ↔ versión nueva | `Start: Restore → Deciding`; `Deciding: Accept → End`, `Reject → Start` | 1 |

`restore` vuelve a `Start` tras un rechazo porque en la migración
`origen` → `enlace` y `enlace` → `destino` usan el mismo canal para varios
intentos. Las etiquetas y el formato son los de siempre
(`abi::msg::labels`), así que `enlace`, que todavía no usa contratos,
habla con los supervisores tipados.

**Lo que garantiza el tipo.**
* `State` y `Rollback` llevan `Endpoint<counter::server::Serving>`: la
  versión vieja no puede entregar el extremo del cliente a mitad de un
  pedido (con una respuesta debida).
* `Restore` y `Reject` llevan `Mem<RO>`: una capacidad `Memory` con `READ`
  y **sin** `WRITE`. Un `Mem<RO>` solo se obtiene con `Mem::read_only`
  (`cap_dup` sin `WRITE`) o recibiéndolo, y al recibirlo se comprueba. Así
  ningún supervisor tipado le puede pasar a la versión nueva el estado
  escribible por error, y ninguna versión nueva tipada acepta uno: lo
  descarta como violación (Kani: `restore_decode_any`; test
  `restore_rejects_a_writable_state`). Es la condición de
  `Upgrade.restoring_mem_at_sup`, que antes dependía de que `init` hiciera
  bien el `cap_dup`.

**Lo que demuestra el modelo** (`Nsos/UpgradeContract.lean`). A cada fase
del modelo de `Upgrade.lean` le corresponde una configuración de cada
canal de control, y cada paso del modelo es cero, uno o dos pasos de cada
contrato (`handover_follows`, `restore_follows`). La única excepción es la
caída de la versión nueva: su canal muere y el próximo intento usa uno
nuevo. Por eso en todo estado alcanzable de la actualización los dos
canales están en configuraciones alcanzables de sus contratos
(`handover_reachable`, `restore_reachable`) y valen los teoremas de
`Contract.lean` (`upgrade_contract_safe`). Si el modelo hiciera algo que el
contrato no permite, la demostración falla: se probó con tres mutaciones
del modelo (mandar `Commit` antes del `Accept`, reenviar sin haber recibido
el estado, retomar sin `Rollback`) y cinco de los contratos.

`init`, `contador-v1`, `contador-v2`, `contador-roto`, `cliente`, `origen`
y `destino` usan los contratos; `enlace` sigue sin tipos (etapa 4), pero
ahora le pasa a `destino` el estado de solo lectura, como exige `restore`.

## El almacén (etapa 3)

El protocolo de [`almacenamiento.md`](almacenamiento.md) son dos contratos,
con las etiquetas de `nsos_store::proto` (un test lo comprueba):

| Contrato | Cliente ↔ servidor | Tabla | Cota |
|---|---|---|---|
| `store_root` | dueño ↔ `almacen` | `Ready: Open → Opening`; `Opening: Opened → Ready`, `Fail → Ready` | 1 |
| `store_obj` | quien tiene el handle ↔ `almacen` | `Idle: Stat → Asking`, `Read → Reading`, `Write → Writing`, `DupRo → Duping`; `Asking: Info`, `Fail`; `Reading: Data`, `FailMem`; `Writing: Written`, `FailMem`; `Duping: Opened`, `Fail` (todas a `Idle`) | 1 |

Más `store_share`, el canal de prueba por el que `escritor` le pasa a
`curioso` un handle de solo lectura.

**Lo que garantiza el tipo.**
* `Open` lleva `flags` y `name`, **no** un dueño: el almacén lo toma de la
  raíz por la que llegó (`Store.rw_handle_from_owner` supone eso).
* El handle viaja como `Endpoint<store_obj::client::Idle>`, en `Opened`
  y en `Share`.
* `Read` y `Write` llevan el búfer del cliente (`Mem<RW>`), y sus
  respuestas también, la de error incluida (`FailMem`). El almacén tipado
  no puede contestar sin devolverlo, y el cliente tipado descarta una
  respuesta sin él (Kani: `store_reading_decode_any`; test
  `store_buffer_always_comes_back`).
* El error tiene la misma etiqueta (99) con y sin búfer. Por eso la regla
  de etiquetas distintas pasó a ser **por estado**, que es lo que necesita
  el decodificador.

**Lo que demuestra el modelo** (`Nsos/StoreContract.lean`): cada pedido
del modelo del almacén es un intercambio completo de su contrato, de
reposo a reposo (`exec_follows`), y los que el modelo rechaza tienen su
respuesta de error (`rejected_follows`). Es una correspondencia por tipo
de pedido: lo que decide si se atiende (quién puede escribir) lo sigue
demostrando `Store.lean`. `buffer_states_reply_only` es la propiedad de la
tabla en que se apoya la devolución del búfer. Cinco mutaciones de las
tablas lo rompen.

Un cambio de comportamiento: un nombre de más de 31 bytes ya no recibe
`ERR`, porque no es del contrato (`Bytes<31>`): el almacén lo descarta
como violación. Ningún cliente del sistema lo mandaba.

## Drivers, flujos y el enlace (etapa 4)

| Contrato | Cliente ↔ servidor | Tabla | Cota |
|---|---|---|---|
| `console` | `hola` ↔ `virtcon` | `Ready: Write → Writing`; `Writing: Written → Ready` | 1 |
| `block` | `almacen` ↔ `virtblk` | `Ready: Info → Asking`, `Read`/`Write`/`Flush → Busy`; `Asking: Geometry → Ready`; `Busy: Done → Ready` | 1 |
| `net` (flujo) | `enlace` ↔ `virtnet` | `Open: Send → Open` (cliente), `Recv → Open` (servidor) | sin cota |

**Flujos.** La red no es pedido y respuesta: `enlace` manda paquetes cuando
quiere y `virtnet` le entrega cada trama que llega. Eso es un estado en el
que mandan las dos puntas, que `check` rechaza. Para eso está la otra forma
de la macro, `stream`: un solo estado, `Open`, del que nunca se sale. Se
comprueba al compilar con `stream_check` (`Contract.streamB` en el modelo;
el test `contract_model` compara las dos en 412 contratos). Lo que vale:

* ningún mensaje inesperado: como nadie cambia de estado, los mensajes
  cruzados se reciben bien (`Contract.stream_head_receivable_a/b`);
* pero **no hay cota**: la cola puede crecer tanto como se quiera
  (`Contract.stream_unbounded`).

Con colas finitas, lo segundo es un riesgo concreto. Antes de esta etapa,
`virtnet` entregaba cada trama con un `send` bloqueante y `enlace` le
mandaba paquetes también bloqueando. Si alguien inunda la red mientras
`enlace` atiende al cliente (y no lee la red), se llena la cola de
`enlace`, `virtnet` queda bloqueado, deja de transmitir, se llena su propia
cola y `enlace` también se bloquea. No hace falta la clave del enlace:
alcanza con poder poner tramas en la red. Ahora `virtnet` entrega con
`Chan::try_send` y, si la cola de `enlace` está llena, descarta la trama y
la cuenta, como una placa de red con el anillo lleno. La regla para todo
flujo: al menos una punta manda sin bloquear.

**El enlace** usa `net` hacia `virtnet`, `restore` hacia los supervisores
(servidor del lado de origen, donde el enlace hace de versión nueva;
cliente del lado de destino) y `counter` para el servicio que reenvía. Lo
que viaja por el cable no cambió (`T_REQ`/`T_RESP` con etiqueta y datos),
así que el modelo del enlace autenticado (`Link.lean`) sigue valiendo.

**Lo que demuestra el modelo** (`Nsos/MigrationContract.lean`): cada paso
de la migración de `Migration.lean` es cero, uno o dos pasos de `handover`
y `restore`, ahora sin excepciones (el canal con el enlace es el mismo en
todos los intentos y por eso `restore` vuelve a `Start`), y en todo estado
alcanzable valen los teoremas de `Contract.lean`
(`migration_contract_safe`). Al escribirlo apareció un error en el mapeo
(el paso en que `origen` recibe el estado de la vieja); la demostración no
cerraba hasta corregirlo. Tres mutaciones del modelo y tres de las tablas y
de la teoría de flujos lo rompen.

## El contrato de cada canal, en el kernel (etapa 5)

Hasta la etapa 4, al abrir un canal o recibir un extremo en un mensaje solo
se comprobaba que fuera un canal con `SEND` y `RECV`. Un emisor podía pasar
un canal de otro protocolo (por ejemplo un handle del almacén donde se
espera un canal `note`) y del otro lado todo llegaba como violación.

Ahora cada canal guarda el contrato que declaró quien lo creó:

* `channel_create(out, contrato)`: el segundo argumento, que antes se
  ignoraba, es el identificador del contrato (0 = sin contrato; menor que
  2^63, porque `channel_contract` lo devuelve como resultado de syscall).
  Las llamadas viejas siguen valiendo y crean canales sin contrato.
* `channel_contract(h)`: lo devuelve. No pide derechos (como `cap_info`).
* Los dos extremos lo comparten y **ninguna syscall lo cambia**
  (`Sys.step_contract_fixed`, `run_contract_fixed`; `cap_dup` o `create`
  que reetiquetaran rompen la demostración).

La macro le da a cada contrato un `ID` (FNV-1a de 63 bits sobre el nombre y
la tabla; un test comprueba que los del sistema son distintos), `pair` crea
el canal con ese `ID`, y `Chan::open` y el decodificador de `Endpoint<S>`
exigen que el canal tenga el `ID` del contrato de `S`. El kernel etiqueta
también los canales que reparte al arrancar (`echo`, `console`, `net`,
`restore`, `block`, `store_root`, `store_share`).

El número **identifica, no autentica**: quien crea un canal puede declarar
el contrato que quiera y después hablarlo mal. Lo que cierra es el
reetiquetado: un canal creado para un protocolo no se puede hacer pasar
por otro. En QEMU, `ping` le manda a `pong` un `NewChannel` con un canal
creado para `store_obj`, y `pong` lo descarta y lo suelta.

## Etapas

1. **(hecha)** Crate, modelo, `ping`/`pong`.
2. **(hecha)** Protocolo de actualización en vivo (`init`, `contador-*`,
   `cliente`, `origen`, `destino`), con `Mem<RW>`/`Mem<RO>` y `call`. El
   modelo `Upgrade.lean` sigue los contratos (`UpgradeContract.lean`).
3. **(hecha)** Protocolo del almacén (`almacen`, `escritor`, `curioso`):
   `store_root`, `store_obj` y `store_share` (`StoreContract.lean`).
4. **(hecha)** Drivers (`virtcon`, `virtblk`, `virtnet`) y el enlace de
   migración: `console`, `block`, el flujo `net` y `restore`/`counter` en
   `enlace` (`MigrationContract.lean`).
5. **(hecha)** El contrato de cada canal lo guarda el kernel
   (`channel_create(out, contrato)`, `channel_contract(h)`) y no cambia
   nunca (`ContractId.lean`).
