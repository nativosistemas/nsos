//! Contratos de los protocolos del sistema.
//!
//! Cada uno está también en el modelo (`verification/lean/Nsos/Contract.lean`)
//! con la misma tabla; el test `contract_model` lo exige.

use crate::abi::msg::labels;
use crate::{contract, Bytes, Endpoint, Mem, RO, RW};

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

contract! {
    /// Servicio `contador` (`cliente` ↔ `contador-v1`/`v2`): cada `Inc`
    /// incrementa y se contesta con el valor y la versión que atendió.
    pub contract counter {
        messages {
            /// Pedido: incrementar.
            Inc(1) {},
            /// Respuesta.
            Count(2) {
                /// Valor del contador después de incrementar.
                count: u64,
                /// Versión del servidor que atendió.
                version: u32,
            },
        }
        start Serving;
        client Serving { Inc => Waiting }
        server Waiting { Count => Serving }
    }
}

contract! {
    /// Actualización en vivo, supervisor (cliente) ↔ versión vieja
    /// (servidor). Modelo: `Upgrade.lean` y `UpgradeContract.lean`.
    ///
    /// La vieja entrega estado y extremo del cliente y **espera** la
    /// decisión: `Commit` (termina) o `Rollback` (retoma con lo que vuelve).
    pub contract handover {
        messages {
            /// Dejá de atender y entregá el estado.
            Prepare(labels::UPGRADE_PREPARE) {},
            /// El estado serializado y el extremo del cliente.
            State(labels::UPGRADE_STATE) {
                /// Objeto con el estado (escribible: es del servicio).
                state: Mem<RW>,
                /// Extremo del cliente, con los pedidos encolados.
                service: Endpoint<counter::server::Serving>,
            },
            /// La nueva aceptó: terminá.
            Commit(labels::UPGRADE_COMMIT) {},
            /// La nueva rechazó o se cayó: retomá con esto.
            Rollback(labels::UPGRADE_ROLLBACK) {
                /// El mismo objeto de estado.
                state: Mem<RW>,
                /// El extremo del cliente.
                service: Endpoint<counter::server::Serving>,
            },
        }
        start Serving;
        client Serving { Prepare => Handing }
        client Deciding { Commit => End, Rollback => Serving }
        server Handing { State => Deciding }
        end End;
    }
}

contract! {
    /// Actualización en vivo, supervisor (cliente) ↔ versión nueva
    /// (servidor). También lo usan `origen` → `enlace` y `enlace` →
    /// `destino` en la migración, varias veces por el mismo canal: por eso
    /// un rechazo vuelve a `Start`.
    ///
    /// El estado viaja **de solo lectura** (`Mem<RO>`): la versión nueva
    /// puede validarlo pero no dañarlo, así que un rechazo o una caída dejan
    /// el original intacto.
    pub contract restore {
        messages {
            /// El estado (solo lectura), el extremo del cliente y un modo
            /// (el nombre de la versión en la migración; `caer` para
            /// `contador-roto`).
            Restore(labels::UPGRADE_RESTORE) {
                /// Copia de solo lectura del estado.
                state: Mem<RO>,
                /// Extremo del cliente.
                service: Endpoint<counter::server::Serving>,
                /// Datos libres.
                mode: Bytes<64>,
            },
            /// Aceptó: se queda con el servicio.
            Accept(labels::UPGRADE_ACCEPT) {},
            /// Rechazó: devuelve lo que recibió sin usarlo.
            Reject(labels::UPGRADE_REJECT) {
                /// La copia de solo lectura del estado.
                state: Mem<RO>,
                /// Extremo del cliente.
                service: Endpoint<counter::server::Serving>,
            },
        }
        start Start;
        client Start { Restore => Deciding }
        server Deciding { Accept => End, Reject => Start }
        end End;
    }
}

contract! {
    /// Almacén, por la **raíz** de un dueño (`escritor`, `curioso` ↔
    /// `almacen`). Etiquetas de `nsos_store::proto`. Modelo:
    /// `Store.lean` y `StoreContract.lean`.
    ///
    /// `Open` no tiene campo de dueño: el almacén lo toma de la raíz por la
    /// que llega el pedido, nunca del mensaje (`Store.rw_handle_from_owner`).
    pub contract store_root {
        messages {
            /// Abrir el objeto `name` del dueño de esta raíz.
            Open(10) {
                /// `nsos_store::proto::OPEN_WRITE` pide escritura.
                flags: u8,
                /// Nombre (1 a 31 bytes, sin ceros).
                name: Bytes<31>,
            },
            /// Abierto: datos del objeto y su handle.
            Opened(11) {
                /// Versión actual (0: todavía no existe).
                version: u64,
                /// Largo en bytes.
                len: u32,
                /// Si el handle permite escribir.
                write: bool,
                /// El handle: un canal que da acceso a este objeto y a nada más.
                handle: Endpoint<store_obj::client::Idle>,
            },
            /// No se pudo.
            Fail(99) {
                /// Código (`nsos_store::proto::E_*`).
                code: u32,
                /// Versión actual, si viene al caso.
                version: u64,
            },
        }
        start Ready;
        client Ready { Open => Opening }
        server Opening { Opened => Ready, Fail => Ready }
    }
}

contract! {
    /// Almacén, por un **handle** (quien lo tenga ↔ `almacen`).
    ///
    /// `Read` y `Write` llevan el búfer del cliente (`Mem<RW>`) y **toda**
    /// respuesta a ellos lo devuelve, también la de error (`FailMem`): el
    /// cliente tipado siempre lo recupera. Por eso hay dos errores con la
    /// misma etiqueta, cada uno en sus estados.
    pub contract store_obj {
        messages {
            /// Datos del objeto.
            Stat(12) {},
            /// Respuesta a `Stat`.
            Info(13) {
                /// Versión actual.
                version: u64,
                /// Largo en bytes.
                len: u32,
                /// Si este handle permite escribir.
                write: bool,
            },
            /// Leer el objeto en el búfer.
            Read(14) {
                /// Búfer (el almacén escribe en él).
                buf: Mem<RW>,
            },
            /// Respuesta a `Read`: los datos están al principio del búfer.
            Data(15) {
                /// Versión leída.
                version: u64,
                /// Bytes leídos.
                len: u32,
                /// El búfer, de vuelta.
                buf: Mem<RW>,
            },
            /// Escribir los primeros `len` bytes del búfer.
            Write(16) {
                /// Versión que se espera encontrar (`ANY_VERSION`: cualquiera;
                /// 0: que no exista).
                expected: u64,
                /// Bytes a escribir.
                len: u32,
                /// Búfer con los datos.
                buf: Mem<RW>,
            },
            /// Respuesta a `Write`.
            Written(17) {
                /// Versión nueva.
                version: u64,
                /// El búfer, de vuelta.
                buf: Mem<RW>,
            },
            /// Un handle nuevo de **solo lectura** al mismo objeto.
            DupRo(18) {},
            /// Respuesta a `DupRo`.
            Opened(11) {
                /// Versión actual.
                version: u64,
                /// Largo en bytes.
                len: u32,
                /// Siempre `false`.
                write: bool,
                /// El handle nuevo.
                handle: Endpoint<client::Idle>,
            },
            /// Error de un pedido sin búfer.
            Fail(99) {
                /// Código.
                code: u32,
                /// Versión actual, si viene al caso.
                version: u64,
            },
            /// Error de un pedido con búfer: lo devuelve.
            FailMem(99) {
                /// Código.
                code: u32,
                /// Versión actual, si viene al caso.
                version: u64,
                /// El búfer, de vuelta.
                buf: Mem<RW>,
            },
        }
        start Idle;
        client Idle { Stat => Asking, Read => Reading, Write => Writing, DupRo => Duping }
        server Asking { Info => Idle, Fail => Idle }
        server Reading { Data => Idle, FailMem => Idle }
        server Writing { Written => Idle, FailMem => Idle }
        server Duping { Opened => Idle, Fail => Idle }
    }
}

contract! {
    /// Prueba del almacén: `escritor` (cliente) le pasa a `curioso`
    /// (servidor) un handle de solo lectura a su objeto, y `curioso`
    /// contesta si todo salió como se esperaba.
    pub contract store_share {
        messages {
            /// El handle de solo lectura.
            Share(1) {
                /// Handle (se mueve).
                handle: Endpoint<store_obj::client::Idle>,
            },
            /// Resultado de las pruebas de `curioso`.
            Verdict(2) {
                /// `true` si todo fue como se esperaba.
                ok: bool,
            },
        }
        start Start;
        client Start { Share => Waiting }
        server Waiting { Verdict => End }
        end End;
    }
}
