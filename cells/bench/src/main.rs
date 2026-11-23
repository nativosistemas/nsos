//! Cell `bench`: benchmarks reproducibles.
//!
//! El kernel la arranca sola (bandera de arranque `bench`), así nada más
//! compite por la CPU. Capacidades de arranque (imágenes):
//! 1. `pong`, servidor de eco.
//! 2. `fragil`, que termina sola en cuanto ve su canal cerrado.
//!
//! Medidas, impresas como `bench: nombre_unidad = valor`:
//! * `ipc_ida_y_vuelta_ns`: `ECHO` a `pong` y su respuesta (`send` + `recv`).
//! * `ipc_call_ns`: lo mismo con una sola syscall, `channel_call`.
//! * `ciclo_de_vida_cell_us`: `cell_spawn`, correr, terminar y liberar todo.
//! * `mem_create_map_us`: crear, mapear, desmapear (`mem_unmap`) y soltar un
//!   objeto de memoria de 64 KiB, siempre en la misma dirección.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::Syscall;
use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    bootstrap, cap_drop, cell_spawn, cell_status, channel_call, channel_create, channel_create_for,
    channel_recv, channel_send, mem_create, mem_map, mem_unmap, println, raw_syscall,
};

const ECHO: u64 = 1;
const ECHO_REPLY: u64 = 2;
const BYE: u64 = 999;
const IPC_ROUNDS: u64 = 2000;
const LIFECYCLE_ROUNDS: u64 = 20;
const MEM_ROUNDS: u64 = 20;
const MEM_SIZE: usize = 64 * 1024;

nsos_rt::entry!(main);

fn now_ns() -> u64 {
    raw_syscall(Syscall::ClockMonotonic as u64, 0, 0, 0, 0).max(0) as u64
}

/// Ida y vuelta a `pong`: con `send` + `recv`, o con `channel_call`.
fn ipc(pong: CapHandle, call: bool) -> Result<u64, Error> {
    // `pong` exige que el canal se haya creado para el contrato `echo`
    // (`channel_contract`); `bench` le habla sin tipos, con el mismo formato.
    let (mine, theirs) = channel_create_for(nsos_rt::contract::protocols::echo::ID)?;
    let child = cell_spawn(pong, &[theirs])?;
    let msg = Message::new(ECHO, b"x").unwrap();
    let mut r = Message::default();
    // Calentar (la primera vuelta incluye el arranque de pong).
    channel_send(mine, &msg, 0)?;
    channel_recv(mine, &mut r, 0)?;
    let t0 = now_ns();
    for _ in 0..IPC_ROUNDS {
        if call {
            r = msg;
            channel_call(mine, &mut r, nsos_rt::abi::syscall::WAIT_FOREVER)?;
        } else {
            channel_send(mine, &msg, 0)?;
            channel_recv(mine, &mut r, 0)?;
        }
        if r.header.label != ECHO_REPLY {
            return Err(Error::BadMessage);
        }
    }
    let dt = now_ns() - t0;
    channel_send(mine, &Message::new(BYE, b"").unwrap(), 0)?;
    cell_status(child, 0)?;
    cap_drop(child)?;
    cap_drop(mine)?;
    Ok(dt / IPC_ROUNDS)
}

fn lifecycle(image: CapHandle) -> Result<u64, Error> {
    let t0 = now_ns();
    for _ in 0..LIFECYCLE_ROUNDS {
        let (mine, theirs) = channel_create()?;
        let child = cell_spawn(image, &[theirs])?;
        cap_drop(mine)?; // la hija ve PEER_CLOSED y termina
        cell_status(child, 0)?;
        cap_drop(child)?;
    }
    Ok((now_ns() - t0) / LIFECYCLE_ROUNDS / 1000)
}

fn memory() -> Result<u64, Error> {
    let t0 = now_ns();
    for i in 0..MEM_ROUNDS {
        let h = mem_create(MEM_SIZE)?;
        // Siempre la misma dirección: `mem_unmap` la deja libre.
        let m = mem_map(h, 0x6000_0000, Rights::READ | Rights::WRITE)?;
        m.write_u64(0, i);
        mem_unmap(m)?;
        cap_drop(h)?;
    }
    Ok((now_ns() - t0) / MEM_ROUNDS / 1000)
}

fn main() -> i32 {
    let results = [
        ("ipc_ida_y_vuelta_ns", ipc(bootstrap(1), false)),
        ("ipc_call_ns", ipc(bootstrap(1), true)),
        ("ciclo_de_vida_cell_us", lifecycle(bootstrap(2))),
        ("mem_create_map_us", memory()),
    ];
    let mut ok = true;
    for (name, r) in results {
        match r {
            Ok(v) => println!("bench: {} = {}", name, v),
            Err(e) => {
                println!("bench: {} falló: {:?}", name, e);
                ok = false;
            }
        }
    }
    if ok {
        0
    } else {
        1
    }
}
