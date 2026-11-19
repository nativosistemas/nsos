//! Cell `contador-v1`: servidor con estado (un contador), versión 1.
//!
//! Capacidades de arranque:
//! 1. Canal de control con el supervisor.
//! 2. Canal de servicio con el cliente.
//!
//! Protocolo de servicio: `INC` → incrementa y responde `COUNT` con
//! `[contador: u64, versión: u32]`.
//!
//! Actualización en vivo (modelo: `verification/lean/Nsos/Upgrade.lean`): al
//! recibir `UPGRADE_PREPARE` deja de atender, escribe su estado en un objeto
//! `Memory` (formato `CNT1`) y responde `UPGRADE_STATE` adjuntando ese
//! objeto **y** el extremo del cliente. Después espera la decisión:
//! * `UPGRADE_COMMIT`: la nueva versión aceptó; termina con 0.
//! * `UPGRADE_ROLLBACK [mem, ep]`: la nueva rechazó; recupera las
//!   capacidades, vuelve a mapear `mem` (al enviarlo perdió el mapeo),
//!   **relee el contador** y vuelve a atender.
//!
//! Para que la prueba sea determinista, solo mira el canal de control
//! después de atender `MIN_BEFORE_UPGRADE` pedidos desde que arrancó o
//! retomó.
//!
//! Sin `wait` multi-handle todavía: sondea ambos canales cediendo la CPU.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::labels::{UPGRADE_COMMIT, UPGRADE_PREPARE, UPGRADE_ROLLBACK, UPGRADE_STATE};
use nsos_rt::abi::syscall::flags::NONBLOCK;
use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    bootstrap, channel_recv, channel_send, mem_create, mem_map, println, yield_now, Mapped,
};

const INC: u64 = 1;
const COUNT: u64 = 2;
const VERSION: u32 = 1;
const MIN_BEFORE_UPGRADE: u64 = 5;
const STATE_VADDR: u64 = 0x3000_0000;
/// Formato del estado v1: `"CNT1"`, contador (u64).
const STATE_MAGIC: u32 = u32::from_le_bytes(*b"CNT1");

nsos_rt::entry!(main);

fn reply(client: CapHandle, count: u64) -> Result<(), Error> {
    let mut d = [0u8; 12];
    d[..8].copy_from_slice(&count.to_le_bytes());
    d[8..].copy_from_slice(&VERSION.to_le_bytes());
    channel_send(client, &Message::new(COUNT, &d).unwrap(), 0)
}

/// Entrega estado y cliente y espera la decisión del supervisor. Devuelve
/// `Some((mem, client, count))` si hubo rollback, `None` si hubo commit.
fn upgrade(
    ctrl: CapHandle,
    mem: CapHandle,
    state: Mapped,
    client: CapHandle,
    count: u64,
) -> Result<Option<(CapHandle, CapHandle, u64)>, Error> {
    state.write_u32(0, STATE_MAGIC);
    state.write_u64(8, count);
    let mut m = Message::new(UPGRADE_STATE, b"").unwrap();
    m.push_cap(mem)?;
    m.push_cap(client)?;
    channel_send(ctrl, &m, 0)?;
    channel_recv(ctrl, &mut m, 0)?;
    match (m.header.label, m.cap_slice()) {
        (UPGRADE_COMMIT, []) => Ok(None),
        (UPGRADE_ROLLBACK, &[mem, client]) => {
            // Al enviar `mem` el kernel nos quitó el mapeo (revocación):
            // hay que volver a mapearlo, en la misma dirección, y releer.
            let state = mem_map(mem, STATE_VADDR, Rights::READ | Rights::WRITE)?;
            if state.read_u32(0) != STATE_MAGIC {
                return Err(Error::BadMessage);
            }
            Ok(Some((mem, client, state.read_u64(8))))
        }
        _ => Err(Error::BadMessage),
    }
}

fn main() -> i32 {
    let ctrl = bootstrap(1);
    let mut client = bootstrap(2);
    let Ok(mut mem) = mem_create(4096) else {
        return 5;
    };
    let Ok(state) = mem_map(mem, STATE_VADDR, Rights::READ | Rights::WRITE) else {
        return 6;
    };
    let mut count = 0u64;
    let mut since_start = 0u64;
    let mut m = Message::default();
    println!("contador-v1: atendiendo");
    loop {
        if since_start >= MIN_BEFORE_UPGRADE {
            match channel_recv(ctrl, &mut m, NONBLOCK) {
                Ok(()) if m.header.label == UPGRADE_PREPARE => {
                    println!("contador-v1: entrego el estado (contador = {})", count);
                    match upgrade(ctrl, mem, state, client, count) {
                        Ok(None) => {
                            println!("contador-v1: la versión nueva aceptó; termino");
                            return 0;
                        }
                        Ok(Some((m2, c2, n))) => {
                            println!("contador-v1: rollback; retomo con contador = {}", n);
                            (mem, client, count, since_start) = (m2, c2, n, 0);
                        }
                        Err(e) => {
                            println!("contador-v1: actualización falló: {:?}", e);
                            return 1;
                        }
                    }
                    continue;
                }
                Ok(()) => println!("contador-v1: control desconocido {}", m.header.label),
                Err(Error::WouldBlock) => {}
                Err(_) => return 2,
            }
        }
        match channel_recv(client, &mut m, NONBLOCK) {
            Ok(()) if m.header.label == INC => {
                count += 1;
                since_start += 1;
                if reply(client, count).is_err() {
                    return 3;
                }
            }
            Ok(()) => println!("contador-v1: pedido desconocido {}", m.header.label),
            Err(Error::WouldBlock) => yield_now(),
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 4,
        }
    }
}
