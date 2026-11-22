//! Contratos de canal tipados (al estilo de los *channel contracts* de
//! Singularity).
//!
//! Un contrato describe un protocolo como un autómata: en cada estado manda
//! una sola punta (cliente o servidor) y cada mensaje lleva a un estado. La
//! macro [`contract!`] genera desde una sola tabla:
//!
//! * un struct por mensaje, con su codificación sobre [`Message`];
//! * los dos lados (`client` y `server`), con un tipo por estado: un
//!   [`Chan`] en el estado `S` solo deja mandar lo que `S` permite, y `send`
//!   consume el canal y devuelve el del estado siguiente (*typestate*);
//! * para cada estado en que se recibe, un `enum` con una variante por
//!   mensaje posible, así que un `match` exhaustivo trata todos los casos;
//! * la tabla [`Transition`] del contrato, que se comprueba **al compilar**
//!   con [`check`] (la misma función que el modelo `Nsos/Contract.lean`; si
//!   la acepta valen sus teoremas).
//!
//! El kernel no sabe nada de contratos: los mensajes siguen siendo los de
//! la ABI, con la etiqueta en `header.label`. Lo que se gana es que la cell
//! tipada no puede mandar fuera de protocolo, y que todo lo que llega se
//! decodifica según su estado: un mensaje que no tiene transición, o con
//! datos o capacidades de otra forma, se descarta (soltando sus
//! capacidades) y el canal queda en el mismo estado
//! ([`RecvError::Violation`]). No es una barrera de aislamiento: la otra
//! punta puede ser cualquier ELF (ver `docs/amenazas.md`).
//!
//! Ver `docs/contratos.md`.
//!
//! # Lo que no compila
//!
//! Mandar algo que el estado no permite (aquí, el cliente manda la
//! respuesta del servidor):
//!
//! ```compile_fail,E0277
//! use nsos_contract::{protocols::echo, Bytes, Chan, Transport};
//! fn f<T: Transport>(c: Chan<T, echo::client::Ready>) {
//!     let _ = c.send(echo::EchoReply { data: Bytes::new(b"x").unwrap() });
//! }
//! ```
//!
//! Usar un estado viejo: `send` consume el canal.
//!
//! ```compile_fail,E0382
//! use nsos_contract::{protocols::echo, Bytes, Chan, Transport};
//! fn f<T: Transport>(c: Chan<T, echo::client::Ready>) {
//!     let _w = c.send(echo::Echo { data: Bytes::new(b"x").unwrap() });
//!     let _ = c.send(echo::Bye {});
//! }
//! ```
//!
//! Olvidarse de un caso al recibir:
//!
//! ```compile_fail,E0004
//! use nsos_contract::{protocols::echo, Chan, Transport};
//! fn f<T: Transport>(s: Chan<T, echo::server::Ready>) {
//!     match s.recv() {
//!         Ok(echo::server::recv::Ready::Echo(..)) => {}
//!         Ok(echo::server::recv::Ready::Bye(..)) => {}
//!         Err(_) => {}
//!     }
//! }
//! ```
//!
//! Un contrato sin cota (el cliente puede mandar sin que el servidor
//! conteste):
//!
//! ```compile_fail,E0080
//! nsos_contract::contract! {
//!     pub contract flood {
//!         messages { Push(1) {} }
//!         start A;
//!         client A { Push => A }
//!     }
//! }
//! ```
//!
//! Un estado en el que mandan las dos puntas:
//!
//! ```compile_fail,E0428
//! nsos_contract::contract! {
//!     pub contract mixed {
//!         messages { Ping(1) {}, Pong(2) {} }
//!         start A;
//!         client A { Ping => B }
//!         server A { Pong => B }
//!         end B;
//!     }
//! }
//! ```
//!
//! Dos mensajes con la misma etiqueta que pueden llegar en el mismo
//! estado (el decodificador no sabría cuál es):
//!
//! ```compile_fail,E0080
//! nsos_contract::contract! {
//!     pub contract dup {
//!         messages { Ping(1) {}, Pong(1) {}, Ack(2) {} }
//!         start A;
//!         client A { Ping => B, Pong => B }
//!         server B { Ack => A }
//!     }
//! }
//! ```
//!
//! En estados distintos sí pueden compartirla (como el error del almacén,
//! que según el estado devuelve o no el búfer):
//!
//! ```
//! nsos_contract::contract! {
//!     pub contract shared {
//!         messages { Ping(1) {}, Pong(1) {}, Ack(2) {} }
//!         start A;
//!         client A { Ping => B }
//!         server B { Pong => A }
//!     }
//! }
//! # fn main() {}
//! ```

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod chan;
mod macros;
mod mem;
pub mod protocols;
mod wire;

pub use chan::{
    Chan, Endpoint, Receives, RecvError, RecvResult, Sends, State, Transport, TrySendError,
};
pub use mem::{Access, Mem, RO, RW};
pub use wire::{Bytes, Field, Msg, Reader, Writer};

#[doc(hidden)]
pub use wire::fits as __fits;

#[doc(hidden)]
pub use nsos_abi as abi;
use nsos_abi::Message;

/// Capacidad de la cola de un extremo en el kernel (`QUEUE_LIMIT` en
/// `kernel/src/cell.rs`). Todo contrato tiene que tener una cota menor o
/// igual: así, mientras las dos puntas cumplan, mandar nunca encuentra la
/// cola llena (`Contract.queue_bound`).
pub const QUEUE_CAP: u32 = 64;

/// Máximo de estados de un contrato (el control usa conjuntos de 64 bits).
pub const MAX_STATES: u16 = 64;

/// Quién manda en una transición.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// El cliente le manda al servidor.
    ClientToServer,
    /// El servidor le manda al cliente.
    ServerToClient,
}

/// Una fila de la tabla de un contrato.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    /// Estado de origen.
    pub src: u16,
    /// Quién manda.
    pub dir: Dir,
    /// Etiqueta del mensaje (`header.label`).
    pub label: u64,
    /// Estado de destino.
    pub dst: u16,
}

/// Por qué [`check`] rechaza un contrato.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckError {
    /// Un estado tiene transiciones de las dos direcciones: las dos puntas
    /// podrían mandar a la vez y cada una recibiría algo que no espera
    /// (`Contract.mixed_breaks`).
    Mixed,
    /// Una etiqueta lleva a dos estados distintos desde el mismo estado
    /// (`Contract.nondet_breaks`).
    Nondeterministic,
    /// Una punta puede mandar mensajes sin límite sin que la otra conteste:
    /// la cola se llenaría (`Contract.loop_unbounded`).
    Unbounded,
    /// Un estado con número `>= MAX_STATES`.
    TooManyStates,
}

/// El control de un contrato: la cota `k` de mensajes en vuelo si está bien
/// formado, o por qué no.
///
/// Es la función `Contract.check` del modelo en Lean, escrita con conjuntos
/// de bits en vez de listas: en cada estado manda una sola punta, cada
/// etiqueta lleva a un solo estado, y `k` es la menor cota de los caminos
/// de una sola dirección (se prueba hasta la cantidad de transiciones; un
/// camino más largo repite una y no termina). El test `contract_model`
/// compara las dos sobre contratos al azar.
pub const fn check(table: &[Transition]) -> Result<u32, CheckError> {
    let n = table.len();
    let mut i = 0;
    while i < n {
        if table[i].src >= MAX_STATES || table[i].dst >= MAX_STATES {
            return Err(CheckError::TooManyStates);
        }
        i += 1;
    }
    let mut i = 0;
    while i < n {
        let mut j = 0;
        while j < n {
            let (a, b) = (&table[i], &table[j]);
            if a.src == b.src {
                if !dir_eq(a.dir, b.dir) {
                    return Err(CheckError::Mixed);
                }
                if a.label == b.label && a.dst != b.dst {
                    return Err(CheckError::Nondeterministic);
                }
            }
            j += 1;
        }
        i += 1;
    }
    let mut srcs = 0u64;
    let mut i = 0;
    while i < n {
        srcs |= 1 << table[i].src;
        i += 1;
    }
    let mut k = 0;
    while k <= n {
        if reach_n(table, Dir::ClientToServer, k + 1, srcs) == 0
            && reach_n(table, Dir::ServerToClient, k + 1, srcs) == 0
        {
            return Ok(k as u32);
        }
        k += 1;
    }
    Err(CheckError::Unbounded)
}

const fn dir_eq(a: Dir, b: Dir) -> bool {
    matches!(
        (a, b),
        (Dir::ClientToServer, Dir::ClientToServer) | (Dir::ServerToClient, Dir::ServerToClient)
    )
}

/// Estados a exactamente `steps` pasos en dirección `d` desde `from`
/// (`Contract.reachN`).
const fn reach_n(table: &[Transition], d: Dir, steps: usize, from: u64) -> u64 {
    let mut cur = from;
    let mut s = 0;
    while s < steps {
        let mut next = 0u64;
        let mut i = 0;
        while i < table.len() {
            let t = &table[i];
            if dir_eq(t.dir, d) && cur & (1 << t.src) != 0 {
                next |= 1 << t.dst;
            }
            i += 1;
        }
        cur = next;
        s += 1;
    }
    cur
}

/// `true` si en cada estado las etiquetas de sus transiciones no se
/// repiten. El decodificador elige el mensaje por la etiqueta **en el
/// estado actual**, así que dos mensajes que pueden llegar en el mismo
/// estado no pueden compartirla; en estados distintos sí (el modelo
/// identifica cada mensaje por estado y etiqueta). La macro genera una fila
/// por cada par (estado, mensaje), así que dos filas con el mismo origen y
/// la misma etiqueta son dos mensajes distintos.
pub const fn labels_distinct_per_state(table: &[Transition]) -> bool {
    let mut i = 0;
    while i < table.len() {
        let mut j = i + 1;
        while j < table.len() {
            if table[i].src == table[j].src && table[i].label == table[j].label {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// `true` si la tabla es un **flujo**: todas sus transiciones salen de
/// `start` y vuelven a `start` (`Contract.streamB`). En un flujo mandan las
/// dos puntas sin cambiar de estado, así que todo lo que llega es esperado
/// (`Contract.stream_head_receivable_a/b`), pero la cola no tiene cota
/// (`Contract.stream_unbounded`): al menos una punta tiene que mandar con
/// [`Chan::try_send`] y descartar lo que no entra.
pub const fn stream_check(table: &[Transition], start: u16) -> bool {
    let mut i = 0;
    while i < table.len() {
        if table[i].src != start || table[i].dst != start {
            return false;
        }
        i += 1;
    }
    true
}

/// El control de un flujo que corre la macro al compilar.
#[doc(hidden)]
pub const fn __assert_stream(table: &[Transition]) {
    if !labels_distinct_per_state(table) {
        panic!("flujo: dos mensajes con la misma etiqueta");
    }
    if !stream_check(table, 0) {
        panic!("flujo: una transición cambia de estado");
    }
}

/// El control que corre la macro al compilar: falla la compilación con un
/// mensaje si el contrato no está bien formado o su cota no entra en la
/// cola del kernel.
#[doc(hidden)]
pub const fn __assert_contract(table: &[Transition]) -> u32 {
    if !labels_distinct_per_state(table) {
        panic!("contrato: dos mensajes con la misma etiqueta en un mismo estado");
    }
    match check(table) {
        Ok(k) if k <= QUEUE_CAP => k,
        Ok(_) => panic!("contrato: la cota de mensajes en vuelo supera la cola del kernel"),
        Err(CheckError::Mixed) => panic!("contrato: un estado deja mandar a las dos puntas"),
        Err(CheckError::Nondeterministic) => {
            panic!("contrato: una etiqueta lleva a dos estados")
        }
        Err(CheckError::Unbounded) => {
            panic!("contrato: una punta puede mandar sin límite sin que la otra conteste")
        }
        Err(CheckError::TooManyStates) => panic!("contrato: demasiados estados"),
    }
}

/// Vacía un mensaje recibido que no se va a usar: suelta sus capacidades
/// para que no queden ocupando slots del CSpace.
pub(crate) fn discard<T: Transport>(t: T, m: &Message) {
    for &h in m.cap_slice() {
        t.cap_drop(h);
    }
}

#[cfg(test)]
mod tests;

#[cfg(kani)]
mod proofs;
