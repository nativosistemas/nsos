//! Cell `contador-v2`: versión 2 del servidor `contador`.
//!
//! Capacidad de arranque 1: canal de control con el supervisor (servidor
//! del contrato `restore`). Arranca *sin* capacidades de servicio y espera
//! `Restore` con:
//! 1. el estado de la versión anterior, **de solo lectura** (`Mem<RO>`: el
//!    decodificador rechaza uno escribible);
//! 2. el extremo del cliente (`counter`), con los pedidos que hayan quedado
//!    encolados.
//!
//! Valida el estado *sin escribirlo*, responde `Accept` y recién entonces lo migra a un
//! objeto `Memory` propio (`CNT1` → `CNT2`, que además cuenta los pedidos
//! que atendió esta versión) y sigue atendiendo al mismo cliente. Termina
//! con 0 cuando el cliente cierra el canal.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::Rights;
use nsos_rt::contract::protocols::{counter, restore};
use nsos_rt::contract::{Chan, RecvError};
use nsos_rt::{bootstrap, mem_create, mem_map, println, Sys};

const VERSION: u32 = 2;
const STATE_VADDR: u64 = 0x3000_0000;
/// Dónde mapea el estado propio (formato `CNT2`).
const OWN_VADDR: u64 = 0x3100_0000;
const MAGIC_V1: u32 = u32::from_le_bytes(*b"CNT1");
const MAGIC_V2: u32 = u32::from_le_bytes(*b"CNT2");

nsos_rt::entry!(main);

fn main() -> i32 {
    let Ok(ctrl) = Chan::<Sys, restore::server::Start>::open(Sys, bootstrap(1)) else {
        return 1;
    };
    let (ctrl, r) = match ctrl.recv() {
        Ok(restore::server::recv::Start::Restore(c, r)) => (c, r),
        Err(e) => {
            println!("contador-v2: esperaba Restore: {:?}", e);
            return 2;
        }
    };
    let Ok(old) = mem_map(r.state.handle(), STATE_VADDR, Rights::READ) else {
        return 3;
    };
    // Migración del formato del estado.
    let (mut count, mut served) = match old.read_u32(0) {
        MAGIC_V1 => (old.read_u64(8), 0u64),
        MAGIC_V2 => (old.read_u64(8), old.read_u64(16)),
        other => {
            println!("contador-v2: estado desconocido {:#x}", other);
            return 4;
        }
    };
    match ctrl.send(restore::Accept {}) {
        Ok(end) => end.close(),
        Err(_) => return 6,
    }
    // Ya aceptó: el estado nuevo va en un objeto propio, y suelta el viejo
    // (el kernel le quita también el mapeo).
    let Ok(own) = mem_create(4096) else {
        return 7;
    };
    let Ok(state) = mem_map(own, OWN_VADDR, Rights::READ | Rights::WRITE) else {
        return 8;
    };
    r.state.release(Sys);
    state.write_u32(0, MAGIC_V2);
    state.write_u64(8, count);
    state.write_u64(16, served);
    println!("contador-v2: estado restaurado (contador = {})", count);
    let mut client = Chan::attach(Sys, r.service);
    loop {
        client = match client.recv() {
            Ok(counter::server::recv::Serving::Inc(c, _)) => {
                count += 1;
                served += 1;
                state.write_u64(8, count);
                state.write_u64(16, served);
                match c.send(counter::Count {
                    count,
                    version: VERSION,
                }) {
                    Ok(c) => c,
                    Err(_) => return 5,
                }
            }
            Err(RecvError::Violation { chan, label }) => {
                println!("contador-v2: pedido fuera de protocolo {}", label);
                chan
            }
            Err(RecvError::WouldBlock(c)) => c,
            Err(RecvError::Failed(_)) => {
                println!("contador-v2: el cliente cerró; atendí {} pedidos", served);
                return 0;
            }
        }
    }
}
