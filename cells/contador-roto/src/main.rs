//! Cell `contador-roto`: una versión del contador que no entiende el estado
//! de la anterior (solo conoce un formato `CNT9` que nadie produce).
//!
//! Capacidad de arranque 1: canal de control con el supervisor. Al recibir
//! `UPGRADE_RESTORE [mem, ep]` mira el estado **sin escribirlo**, lo rechaza
//! y devuelve las dos capacidades con `UPGRADE_REJECT` sin haber tocado el
//! canal del cliente. Así el supervisor puede hacer rollback.
//!
//! Si los datos de `UPGRADE_RESTORE` son `caer`, en cambio se porta como una
//! versión con un bug: intenta escribir el estado. Como el supervisor le dio
//! `mem` de solo lectura, el kernel la termina por la excepción, y el
//! supervisor hace rollback igual (`Upgrade.crash_rolls_back`).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::labels::{UPGRADE_REJECT, UPGRADE_RESTORE};
use nsos_rt::abi::{Message, Rights};
use nsos_rt::{bootstrap, channel_recv, channel_send, mem_map, println};

const STATE_VADDR: u64 = 0x3000_0000;
const MAGIC_V9: u32 = u32::from_le_bytes(*b"CNT9");

nsos_rt::entry!(main);

fn main() -> i32 {
    let ctrl = bootstrap(1);
    let mut m = Message::default();
    if channel_recv(ctrl, &mut m, 0).is_err() || m.header.label != UPGRADE_RESTORE {
        return 1;
    }
    let [mem, client] = *m.cap_slice() else {
        return 2;
    };
    // Solo lectura: si rechaza, el estado vuelve intacto a la versión vieja.
    let state = mem_map(mem, STATE_VADDR, Rights::READ);
    if m.payload() == b"caer" {
        if let Ok(state) = state {
            println!("contador-roto: escribo el estado (no debería poder)");
            state.write_u32(0, MAGIC_V9);
        }
        println!("contador-roto: ¡pude escribir un estado de solo lectura!");
        return 6;
    }
    let magic = match state {
        Ok(state) => state.read_u32(0),
        Err(_) => 0,
    };
    if magic == MAGIC_V9 {
        println!("contador-roto: ¿estado CNT9? no debería existir");
        return 3;
    }
    println!(
        "contador-roto: no entiendo el estado {:#x}; lo devuelvo",
        magic
    );
    let mut r = Message::new(UPGRADE_REJECT, b"").unwrap();
    if r.push_cap(mem).is_err() || r.push_cap(client).is_err() {
        return 4;
    }
    if channel_send(ctrl, &r, 0).is_err() {
        return 5;
    }
    0
}
