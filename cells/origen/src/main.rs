//! Cell `origen`: supervisor de la máquina de la que se va un servicio
//! (hito 10, modelo: `verification/lean/Nsos/Migration.lean`).
//!
//! Capacidades de arranque:
//! 1. Canal de control con `enlace` (la red autenticada hacia la otra
//!    máquina); `origen` es el cliente del contrato `restore` y lo usa para
//!    todos los intentos.
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
//! Al enlace le pasa una copia **de solo lectura** del estado (`Mem<RO>`) y
//! se queda con la escribible: si la otra máquina rechaza, la v1 retoma con
//! el original, que el enlace no pudo tocar.
//!
//! Termina con 0 si la v1 y el cliente terminaron bien.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::cell_state;
use nsos_rt::abi::{CapHandle, Error};
use nsos_rt::contract::protocols::{counter, handover, restore};
use nsos_rt::contract::{Bytes, Chan};
use nsos_rt::{bootstrap, cap_drop, cell_spawn, cell_status, println, Sys};

nsos_rt::entry!(main);

type Old = Chan<Sys, handover::client::Serving>;
type Link = Chan<Sys, restore::client::Start>;

/// Migra el servicio de `old` a la otra máquina, donde arranca `remote`.
/// Si la otra punta rechazó, devuelve los dos canales para otro intento.
fn try_migrate(old: Old, link: Link, remote: &str) -> Result<Option<(Old, Link)>, Error> {
    println!(
        "origen: pido a contador-v1 su estado (para {} en la otra máquina)",
        remote
    );
    let old = old.send(handover::Prepare {})?;
    let Ok(handover::client::recv::Handing::State(old, st)) = old.recv() else {
        return Err(Error::BadMessage);
    };
    let link = link.send(restore::Restore {
        state: st.state.read_only(Sys)?,
        service: st.service,
        mode: Bytes::new(remote.as_bytes()).ok_or(Error::InvalidArgs)?,
    })?;
    match link.recv() {
        Ok(restore::client::recv::Deciding::Accept(end, _)) => {
            println!("origen: {} aceptó en la otra máquina; commit", remote);
            end.close();
            st.state.release(Sys);
            old.send(handover::Commit {})?.close();
            Ok(None)
        }
        Ok(restore::client::recv::Deciding::Reject(link, r)) => {
            println!("origen: {} rechazó; rollback a contador-v1", remote);
            r.state.release(Sys);
            let old = old.send(handover::Rollback {
                state: st.state,
                service: r.service,
            })?;
            Ok(Some((old, link)))
        }
        Err(_) => Err(Error::BadMessage),
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
    let link = Link::open(Sys, bootstrap(1))?;
    let (client_end, srv_end) = counter::pair(Sys)?;
    let (ctl, ctl_child) = handover::pair(Sys)?;
    let v1 = cell_spawn(bootstrap(2), &[ctl_child.into_raw(), srv_end.into_raw()])?;
    let client = cell_spawn(bootstrap(3), &[client_end.into_raw()])?;
    let Some((ctl, link)) = try_migrate(ctl, link, "contador-roto")? else {
        return Err(Error::BadMessage);
    };
    if try_migrate(ctl, link, "contador-v2")?.is_some() {
        return Err(Error::BadMessage);
    }
    expect_ok("contador-v1", v1)?;
    expect_ok("cliente", client)
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
