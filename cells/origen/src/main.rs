//! Cell `origen`: supervisor de la máquina de la que se va un servicio
//! (hito 10, modelo: `verification/lean/Nsos/Migration.lean`).
//!
//! Capacidades de arranque:
//! 1. Canal de control con `enlace` (el puerto serie hacia la otra máquina).
//! 2. Imagen de `contador-v1`. 3. Imagen de `cliente`.
//!
//! Arranca `contador-v1` y su `cliente` y migra el servicio dos veces con el
//! mismo protocolo que una actualización local; para este supervisor el
//! enlace hace de "versión nueva":
//! 1. pide `contador-roto` en la otra máquina, que rechaza el estado:
//!    rollback y la v1 retoma;
//! 2. pide `contador-v2`, que acepta: commit, la v1 termina y el cliente
//!    sigue conectado al mismo canal, ahora atendido desde la otra máquina.
//!
//! Termina con 0 si la v1 y el cliente terminaron bien.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::labels::{
    UPGRADE_ACCEPT, UPGRADE_COMMIT, UPGRADE_PREPARE, UPGRADE_REJECT, UPGRADE_RESTORE,
    UPGRADE_ROLLBACK, UPGRADE_STATE,
};
use nsos_rt::abi::syscall::cell_state;
use nsos_rt::abi::{CapHandle, Error, Message};
use nsos_rt::{
    bootstrap, cap_drop, cell_spawn, cell_status, channel_create, channel_recv, channel_send,
    println,
};

nsos_rt::entry!(main);

/// Migra el servicio de `ctl_old` a la otra máquina, donde arranca
/// `remote`. Devuelve si la otra punta aceptó.
fn try_migrate(ctl_old: CapHandle, link: CapHandle, remote: &str) -> Result<bool, Error> {
    println!(
        "origen: pido a contador-v1 su estado (para {} en la otra máquina)",
        remote
    );
    channel_send(ctl_old, &Message::new(UPGRADE_PREPARE, b"")?, 0)?;
    let mut m = Message::default();
    channel_recv(ctl_old, &mut m, 0)?;
    if m.header.label != UPGRADE_STATE || m.cap_slice().len() != 2 {
        return Err(Error::BadMessage);
    }
    let mut r = Message::new(UPGRADE_RESTORE, remote.as_bytes())?;
    for &c in m.cap_slice() {
        r.push_cap(c)?;
    }
    channel_send(link, &r, 0)?;
    channel_recv(link, &mut m, 0)?;
    match (m.header.label, m.cap_slice().len()) {
        (UPGRADE_ACCEPT, 0) => {
            println!("origen: {} aceptó en la otra máquina; commit", remote);
            channel_send(ctl_old, &Message::new(UPGRADE_COMMIT, b"")?, 0)?;
            Ok(true)
        }
        (UPGRADE_REJECT, 2) => {
            println!("origen: {} rechazó; rollback a contador-v1", remote);
            let mut back = Message::new(UPGRADE_ROLLBACK, b"")?;
            for &c in m.cap_slice() {
                back.push_cap(c)?;
            }
            channel_send(ctl_old, &back, 0)?;
            Ok(false)
        }
        _ => Err(Error::BadMessage),
    }
}

fn expect_ok(name: &str, h: CapHandle) -> Result<(), Error> {
    let st = cell_status(h, 0)?;
    cap_drop(h)?;
    if st.state == cell_state::EXITED && st.code == 0 {
        println!("origen: {} terminó bien", name);
        Ok(())
    } else {
        println!(
            "origen: {} terminó mal (estado {}, código {})",
            name, st.state, st.code
        );
        Err(Error::BadMessage)
    }
}

fn run() -> Result<(), Error> {
    let link = bootstrap(1);
    let (srv_end, client_end) = channel_create()?;
    let (ctl, ctl_child) = channel_create()?;
    let v1 = cell_spawn(bootstrap(2), &[ctl_child, srv_end])?;
    let client = cell_spawn(bootstrap(3), &[client_end])?;
    if try_migrate(ctl, link, "contador-roto")? {
        return Err(Error::BadMessage);
    }
    if !try_migrate(ctl, link, "contador-v2")? {
        return Err(Error::BadMessage);
    }
    expect_ok("contador-v1", v1)?;
    expect_ok("cliente", client)?;
    cap_drop(ctl)?;
    cap_drop(link)
}

fn main() -> i32 {
    match run() {
        Ok(()) => {
            println!("origen: migración OK");
            0
        }
        Err(e) => {
            println!("origen: migración falló: {:?}", e);
            1
        }
    }
}
