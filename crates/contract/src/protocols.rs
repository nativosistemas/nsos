//! Contratos de los protocolos del sistema.
//!
//! Cada uno está también en el modelo (`verification/lean/Nsos/Contract.lean`)
//! con la misma tabla; el test `contract_model` lo exige.

use crate::{contract, Bytes, Endpoint};

contract! {
    /// `ping` ↔ `pong` (y `bench` ↔ `pong`): eco, transferencia de un canal
    /// y despedida.
    ///
    /// `NewChannel` espera la respuesta `Adopted`. La versión anterior del
    /// protocolo volvía directo a `Ready` y el control la rechaza: el
    /// cliente podía mandar `NewChannel` sin límite y llenar la cola de
    /// `pong` (`Contract.echoOld_unbounded`).
    pub contract echo {
        messages {
            /// Pedido de eco.
            Echo(1) {
                /// Texto a devolver.
                data: Bytes<224>,
            },
            /// Respuesta a `Echo`.
            EchoReply(2) {
                /// El mismo texto.
                data: Bytes<224>,
            },
            /// El cliente le pasa al servidor un extremo de un canal `note`.
            NewChannel(100) {
                /// El extremo del servidor (se mueve).
                side: Endpoint<note::server::Start>,
            },
            /// El servidor terminó la conversación por el canal recibido.
            Adopted(101) {},
            /// Fin: el servidor termina.
            Bye(999) {},
        }
        start Ready;
        client Ready { Echo => Waiting, NewChannel => Adopting, Bye => End }
        server Waiting { EchoReply => Ready }
        server Adopting { Adopted => Ready }
        end End;
    }
}

contract! {
    /// El canal que `ping` le pasa a `pong` por `echo::NewChannel`: una
    /// nota y su acuse.
    pub contract note {
        messages {
            /// Texto del cliente.
            Note(0) {
                /// El texto.
                text: Bytes<224>,
            },
            /// Respuesta del servidor.
            Ack(2) {
                /// El texto de la respuesta.
                text: Bytes<224>,
            },
        }
        start Start;
        client Start { Note => Waiting }
        server Waiting { Ack => End }
        end End;
    }
}
