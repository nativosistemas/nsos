//! Cell `contador-v2`: versión 2 del servidor `contador`.
//!
//! Capacidad de arranque 1: canal de control con el supervisor. Arranca
//! *sin* capacidades de servicio y espera `UPGRADE_RESTORE` con:
//! 1. el objeto `Memory` con el estado de la versión anterior;
//! 2. el extremo de canal del cliente (con los pedidos que hayan quedado
//!    encolados).
//!
//! Migra el estado (`CNT1` → `CNT2`, que además cuenta los pedidos que
//! atendió esta versión) y sigue atendiendo al mismo cliente. Termina con 0
//! cuando el cliente cierra el canal.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::labels::UPGRADE_RESTORE;
use nsos_rt::abi::{Message, Rights};
use nsos_rt::{bootstrap, channel_recv, channel_send, mem_map, println};

const INC: u64 = 1;
const COUNT: u64 = 2;
const VERSION: u32 = 2;
const STATE_VADDR: u64 = 0x3000_0000;
const MAGIC_V1: u32 = u32::from_le_bytes(*b"CNT1");
const MAGIC_V2: u32 = u32::from_le_bytes(*b"CNT2");

nsos_rt::entry!(main);

fn main() -> i32 {
    let ctrl = bootstrap(1);
    let mut m = Message::default();
    if channel_recv(ctrl, &mut m, 0).is_err() || m.header.label != UPGRADE_RESTORE {
        println!("contador-v2: esperaba UPGRADE_RESTORE");
        return 1;
    }
    let [mem, client] = *m.cap_slice() else {
        println!("contador-v2: UPGRADE_RESTORE sin estado y canal");
        return 2;
    };
    let Ok(state) = mem_map(mem, STATE_VADDR, Rights::READ | Rights::WRITE) else {
        return 3;
    };
    // Migración del formato del estado.
    let (mut count, mut served) = match state.read_u32(0) {
        MAGIC_V1 => (state.read_u64(8), 0u64),
        MAGIC_V2 => (state.read_u64(8), state.read_u64(16)),
        other => {
            println!("contador-v2: estado desconocido {:#x}", other);
            return 4;
        }
    };
    state.write_u32(0, MAGIC_V2);
    println!("contador-v2: estado restaurado (contador = {})", count);
    loop {
        match channel_recv(client, &mut m, 0) {
            Ok(()) if m.header.label == INC => {
                count += 1;
                served += 1;
                state.write_u64(8, count);
                state.write_u64(16, served);
                let mut d = [0u8; 12];
                d[..8].copy_from_slice(&count.to_le_bytes());
                d[8..].copy_from_slice(&VERSION.to_le_bytes());
                if channel_send(client, &Message::new(COUNT, &d).unwrap(), 0).is_err() {
                    return 5;
                }
            }
            Ok(()) => println!("contador-v2: pedido desconocido {}", m.header.label),
            Err(_) => {
                println!("contador-v2: el cliente cerró; atendí {} pedidos", served);
                return 0;
            }
        }
    }
}
