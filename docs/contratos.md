# Contratos de canal tipados

Estado: **etapa 1** (crate `nsos-contract`, modelo en Lean, `ping`/`pong`
migrados). La fuente de verdad ejecutable es
[`crates/contract`](../crates/contract), y el modelo,
[`verification/lean/Nsos/Contract.lean`](../verification/lean/Nsos/Contract.lean).

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
| Las etiquetas no se repiten | El decodificador no sabría qué mensaje es | — |
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
* Kani (`crates/contract/src/proofs.rs`): `decode_any`, `roundtrip_echo`,
  `roundtrip_endpoint`, `check_any`.

La macro numera los estados como el modelo: primero los estados `client`,
después los `server` y al final los `end`, cada grupo en el orden en que se
declaró.

## Etapas

1. **(hecha)** Crate, modelo, `ping`/`pong`.
2. Protocolo de actualización en vivo (`init`, `contador-*`), con un campo
   `Memory` tipado. Su modelo (`Upgrade.lean`) pasa a usar el autómata como
   especificación de los pasos.
3. Protocolo del almacén (`nsos_store::proto`).
4. Drivers (`virtcon`, `virtblk`, `virtnet`) y el enlace de migración.
5. Opcional: un `contract_id` fijo en cada canal, que el kernel informe en
   `cap_info`, para que la identidad del contrato de un extremo recibido la
   garantice el kernel (hoy se comprueba solo que sea un canal con `SEND` y
   `RECV`). Cambia la ABI: modelo primero.
