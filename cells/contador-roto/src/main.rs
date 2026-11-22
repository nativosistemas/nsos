//! Cell `contador-roto`: una versión del contador que no entiende el estado
//! de la anterior (solo conoce un formato `CNT9` que nadie produce).
//!
//! Capacidad de arranque 1: canal de control con el supervisor (servidor
//! del contrato `restore`). Al recibir `Restore` mira el estado **sin
//! escribirlo**, lo rechaza y devuelve las dos capacidades con `Reject` sin
//! haber tocado el canal del cliente. Así el supervisor puede hacer
//! rollback.
//!
//! Si el modo de `Restore` es `caer`, en cambio se porta como una versión
//! con un bug: intenta escribir el estado. Como el contrato exige que el
//! estado llegue de solo lectura (`Mem<RO>`), el kernel la termina por la
//! excepción, y el supervisor hace rollback igual
//! (`Upgrade.crash_rolls_back`).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::Rights;
use nsos_rt::contract::protocols::restore;
use nsos_rt::contract::Chan;
use nsos_rt::{bootstrap, mem_map, println, Sys};

const STATE_VADDR: u64 = 0x3000_0000;
const MAGIC_V9: u32 = u32::from_le_bytes(*b"CNT9");

nsos_rt::entry!(main);

fn main() -> i32 {
    let Ok(ctrl) = Chan::<Sys, restore::server::Start>::open(Sys, bootstrap(1)) else {
        return 1;
    };
    let Ok(restore::server::recv::Start::Restore(ctrl, r)) = ctrl.recv() else {
        return 2;
    };
    // Solo lectura: si rechaza, el estado vuelve intacto a la versión vieja.
    let state = mem_map(r.state.handle(), STATE_VADDR, Rights::READ);
    if r.mode.as_slice() == b"caer" {
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
    let back = restore::Reject {
        state: r.state,
        service: r.service,
    };
    match ctrl.send(back) {
        Ok(start) => start.close(),
        Err(_) => return 5,
    }
    0
}
