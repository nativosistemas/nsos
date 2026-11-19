//! Cell `init`: supervisor.
//!
//! Capacidades de arranque (imágenes, con `EXEC`):
//! 1. `fragil`: una cell que falla las primeras veces.
//! 2. `dormilona`: una cell que se queda bloqueada.
//!
//! Fase 1, reinicio: arranca `fragil` con un canal nuevo en cada intento,
//! le dice qué intento es, espera a que termine y, si no terminó con 0, la
//! vuelve a arrancar (hasta `MAX_ATTEMPTS`). El resto del sistema sigue
//! corriendo mientras tanto.
//!
//! Fase 2, terminación: arranca `dormilona` dos veces (bloqueada en el
//! kernel y girando en anillo 3), comprueba que sigue viva, la mata con
//! `cell_kill` y verifica que terminó como `KILLED` y que su canal quedó
//! cerrado.
//!
//! Termina con 0 si todo salió como se esperaba.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::{cell_state, flags, CellStatus};
use nsos_rt::abi::{CapHandle, Error, Message};
use nsos_rt::{
    bootstrap, cap_drop, cell_kill, cell_spawn, cell_status, channel_create, channel_recv,
    channel_send, println, yield_now,
};

const ATTEMPT: u64 = 1;
const READY: u64 = 2;
const SPIN: u64 = 3;
const MAX_ATTEMPTS: u32 = 5;

nsos_rt::entry!(main);

fn describe(s: &CellStatus) -> &'static str {
    match s.state {
        cell_state::RUNNING => "sigue corriendo",
        cell_state::EXITED if s.code == 0 => "terminó bien",
        cell_state::EXITED => "terminó con error",
        cell_state::FAULTED => "la terminó el kernel por una excepción",
        _ => "la mataron",
    }
}

/// Un intento: arranca `fragil` y espera a que termine.
fn attempt(image: CapHandle, n: u32) -> Result<CellStatus, Error> {
    let (mine, theirs) = channel_create()?;
    let child = cell_spawn(image, &[theirs])?;
    channel_send(mine, &Message::new(ATTEMPT, &n.to_le_bytes()).unwrap(), 0)?;
    let st = cell_status(child, 0)?;
    if st.state == cell_state::EXITED && st.code == 0 {
        let mut m = Message::default();
        channel_recv(mine, &mut m, 0)?;
        if m.header.label != READY {
            return Err(Error::BadMessage);
        }
    }
    cap_drop(child)?;
    cap_drop(mine)?;
    Ok(st)
}

fn restart_phase(image: CapHandle) -> Result<u32, Error> {
    for n in 1..=MAX_ATTEMPTS {
        let st = attempt(image, n)?;
        println!(
            "init: fragil, intento {}: {} (estado {}, código {}, vector {})",
            n,
            describe(&st),
            st.state,
            st.code,
            st.vector
        );
        if st.state == cell_state::EXITED && st.code == 0 {
            return Ok(n);
        }
        println!("init: la reinicio");
    }
    Err(Error::TimedOut)
}

fn kill_phase(image: CapHandle, spin: bool) -> Result<(), Error> {
    let (mine, theirs) = channel_create()?;
    let child = cell_spawn(image, &[theirs])?;
    if spin {
        channel_send(mine, &Message::new(SPIN, &[]).unwrap(), 0)?;
    }
    for _ in 0..10 {
        yield_now();
    }
    let before = cell_status(child, flags::NONBLOCK)?;
    if before.state != cell_state::RUNNING {
        println!("init: dormilona debería seguir viva: {}", describe(&before));
        return Err(Error::BadMessage);
    }
    cell_kill(child)?;
    let after = cell_status(child, 0)?;
    println!(
        "init: dormilona ({}): {}",
        if spin { "girando" } else { "bloqueada" },
        describe(&after)
    );
    if after.state != cell_state::KILLED {
        return Err(Error::BadMessage);
    }
    let mut m = Message::default();
    if channel_recv(mine, &mut m, flags::NONBLOCK) != Err(Error::PeerClosed) {
        println!("init: el canal de dormilona debería estar cerrado");
        return Err(Error::BadMessage);
    }
    cap_drop(child)?;
    cap_drop(mine)
}

fn main() -> i32 {
    let fragil = bootstrap(1);
    let dormilona = bootstrap(2);
    match restart_phase(fragil) {
        Ok(n) => println!("init: fragil quedó en pie tras {} intentos", n),
        Err(e) => {
            println!("init: fase de reinicio falló: {:?}", e);
            return 1;
        }
    }
    for spin in [false, true] {
        if let Err(e) = kill_phase(dormilona, spin) {
            println!("init: fase de terminación falló: {:?}", e);
            return 2;
        }
    }
    println!("init: supervisión OK");
    0
}
