//! Cell `contador-v1`: servidor con estado (un contador), versión 1.
//!
//! Capacidades de arranque:
//! 1. Canal de control con el supervisor.
//! 2. Canal de servicio con el cliente.
//!
//! Protocolo de servicio: `INC` → incrementa y responde `COUNT` con
//! `[contador: u64, versión: u32]`.
//!
//! Actualización en vivo: al recibir `UPGRADE_PREPARE` deja de atender,
//! escribe su estado en un objeto `Memory` nuevo (formato `CNT1`) y responde
//! `UPGRADE_STATE` adjuntando ese objeto **y** el extremo del cliente.
//! Después termina con 0. Para que la prueba sea determinista, solo mira el
//! canal de control después de atender `MIN_BEFORE_UPGRADE` pedidos.
//!
//! Sin `wait` multi-handle todavía: sondea ambos canales cediendo la CPU.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::labels::{UPGRADE_PREPARE, UPGRADE_STATE};
use nsos_rt::abi::syscall::flags::NONBLOCK;
use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{bootstrap, channel_recv, channel_send, mem_create, mem_map, println, yield_now};

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

fn hand_over(ctrl: CapHandle, client: CapHandle, count: u64) -> Result<(), Error> {
    let mem = mem_create(4096)?;
    let state = mem_map(mem, STATE_VADDR, Rights::READ | Rights::WRITE)?;
    state.write_u32(0, STATE_MAGIC);
    state.write_u64(8, count);
    let mut m = Message::new(UPGRADE_STATE, b"").unwrap();
    m.push_cap(mem)?;
    m.push_cap(client)?;
    channel_send(ctrl, &m, 0)
}

fn main() -> i32 {
    let ctrl = bootstrap(1);
    let client = bootstrap(2);
    let mut count = 0u64;
    let mut m = Message::default();
    println!("contador-v1: atendiendo");
    loop {
        if count >= MIN_BEFORE_UPGRADE {
            match channel_recv(ctrl, &mut m, NONBLOCK) {
                Ok(()) if m.header.label == UPGRADE_PREPARE => {
                    println!("contador-v1: entrego el estado (contador = {})", count);
                    return match hand_over(ctrl, client, count) {
                        Ok(()) => 0,
                        Err(e) => {
                            println!("contador-v1: no pude entregar el estado: {:?}", e);
                            1
                        }
                    };
                }
                Ok(()) => println!("contador-v1: control desconocido {}", m.header.label),
                Err(Error::WouldBlock) => {}
                Err(_) => return 2,
            }
        }
        match channel_recv(client, &mut m, NONBLOCK) {
            Ok(()) if m.header.label == INC => {
                count += 1;
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
