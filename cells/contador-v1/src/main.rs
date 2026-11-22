//! Cell `contador-v1`: servidor con estado (un contador), versión 1.
//!
//! Capacidades de arranque:
//! 1. Canal de control con el supervisor (servidor del contrato `handover`).
//! 2. Canal de servicio con el cliente (servidor del contrato `counter`).
//!
//! Servicio: `Inc` → incrementa y responde `Count` con el contador y la
//! versión.
//!
//! Actualización en vivo (modelos: `verification/lean/Nsos/Upgrade.lean` y
//! `UpgradeContract.lean`): al recibir `Prepare` deja de atender, escribe su
//! estado en un objeto `Memory` (formato `CNT1`) y responde `State` con ese
//! objeto **y** el extremo del cliente. El tipo exige que el extremo esté en
//! `counter::server::Serving`: no se puede entregar a mitad de un pedido.
//! Después espera la decisión:
//! * `Commit`: la nueva versión aceptó; termina con 0.
//! * `Rollback`: la nueva rechazó o se cayó; recupera las capacidades,
//!   vuelve a mapear el estado (al enviarlo perdió el mapeo), **relee el
//!   contador** y vuelve a atender.
//!
//! Para que la prueba sea determinista, solo mira el canal de control
//! después de atender `MIN_BEFORE_UPGRADE` pedidos desde que arrancó o
//! retomó.
//!
//! Cuando no hay nada que hacer duerme en `wait` sobre los dos canales (el
//! de control solo cuando ya lo puede mirar), en lugar de sondearlos.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::cap::signals::{PEER_CLOSED, READABLE};
use nsos_rt::abi::syscall::WAIT_FOREVER;
use nsos_rt::abi::{Error, Rights};
use nsos_rt::contract::protocols::{counter, handover};
use nsos_rt::contract::{Chan, Mem, RecvError, RW};
use nsos_rt::{bootstrap, mem_create, mem_map, println, wait, wait_item, Mapped, Sys};

const VERSION: u32 = 1;
const MIN_BEFORE_UPGRADE: u64 = 5;
const STATE_VADDR: u64 = 0x3000_0000;
/// Formato del estado v1: `"CNT1"`, contador (u64).
const STATE_MAGIC: u32 = u32::from_le_bytes(*b"CNT1");

nsos_rt::entry!(main);

type Ctrl = Chan<Sys, handover::server::Serving>;
type Client = Chan<Sys, counter::server::Serving>;

/// Lo que queda después de una actualización que no se completó.
struct Resumed {
    ctrl: Ctrl,
    mem: Mem<RW>,
    state: Mapped,
    client: Client,
    count: u64,
}

/// Entrega estado y cliente y espera la decisión del supervisor. Devuelve
/// `Some` si hubo rollback, `None` si hubo commit.
fn upgrade(
    ctrl: Chan<Sys, handover::server::Handing>,
    mem: Mem<RW>,
    state: Mapped,
    client: Client,
    count: u64,
) -> Result<Option<Resumed>, Error> {
    state.write_u32(0, STATE_MAGIC);
    state.write_u64(8, count);
    let ctrl = ctrl.send(handover::State {
        state: mem,
        service: client.into_endpoint(),
    })?;
    match ctrl.recv() {
        Ok(handover::server::recv::Deciding::Commit(end, _)) => {
            end.close();
            Ok(None)
        }
        Ok(handover::server::recv::Deciding::Rollback(ctrl, rb)) => {
            // Al enviar el estado el kernel nos quitó el mapeo
            // (revocación): hay que volver a mapearlo y releer.
            let state = mem_map(rb.state.handle(), STATE_VADDR, Rights::READ | Rights::WRITE)?;
            if state.read_u32(0) != STATE_MAGIC {
                return Err(Error::BadMessage);
            }
            Ok(Some(Resumed {
                ctrl,
                mem: rb.state,
                state,
                client: Chan::attach(Sys, rb.service),
                count: state.read_u64(8),
            }))
        }
        Err(_) => Err(Error::BadMessage),
    }
}

fn main() -> i32 {
    let (Ok(mut ctrl), Ok(mut client)) = (
        Ctrl::open(Sys, bootstrap(1)),
        Client::open(Sys, bootstrap(2)),
    ) else {
        return 1;
    };
    let Ok(mut mem) = mem_create(4096).and_then(|h| Mem::<RW>::open(Sys, h)) else {
        return 5;
    };
    let Ok(mut state) = mem_map(mem.handle(), STATE_VADDR, Rights::READ | Rights::WRITE) else {
        return 6;
    };
    let mut count = 0u64;
    let mut since_start = 0u64;
    println!("contador-v1: atendiendo");
    loop {
        if since_start >= MIN_BEFORE_UPGRADE {
            ctrl = match ctrl.try_recv() {
                Ok(handover::server::recv::Serving::Prepare(handing, _)) => {
                    println!("contador-v1: entrego el estado (contador = {})", count);
                    match upgrade(handing, mem, state, client, count) {
                        Ok(None) => {
                            println!("contador-v1: la versión nueva aceptó; termino");
                            return 0;
                        }
                        Ok(Some(r)) => {
                            println!("contador-v1: rollback; retomo con contador = {}", r.count);
                            (mem, state, client, count, since_start) =
                                (r.mem, r.state, r.client, r.count, 0);
                            r.ctrl
                        }
                        Err(e) => {
                            println!("contador-v1: actualización falló: {:?}", e);
                            return 1;
                        }
                    }
                }
                Err(RecvError::Violation { chan, label }) => {
                    println!("contador-v1: control fuera de protocolo {}", label);
                    chan
                }
                Err(RecvError::WouldBlock(c)) => c,
                Err(RecvError::Failed(_)) => return 2,
            };
            if since_start == 0 {
                continue;
            }
        }
        client = match client.try_recv() {
            Ok(counter::server::recv::Serving::Inc(c, _)) => {
                count += 1;
                since_start += 1;
                match c.send(counter::Count {
                    count,
                    version: VERSION,
                }) {
                    Ok(c) => c,
                    Err(_) => return 3,
                }
            }
            Err(RecvError::Violation { chan, label }) => {
                println!("contador-v1: pedido fuera de protocolo {}", label);
                chan
            }
            Err(RecvError::WouldBlock(c)) => {
                // Dormir hasta que haya un pedido o, si ya se puede, algo
                // del supervisor.
                let mut items = [
                    wait_item(c.handle(), READABLE | PEER_CLOSED),
                    wait_item(ctrl.handle(), READABLE | PEER_CLOSED),
                ];
                let n = if since_start >= MIN_BEFORE_UPGRADE {
                    2
                } else {
                    1
                };
                if wait(&mut items[..n], WAIT_FOREVER).is_err() {
                    return 7;
                }
                c
            }
            Err(RecvError::Failed(Error::PeerClosed)) => return 0,
            Err(RecvError::Failed(_)) => return 4,
        };
    }
}
