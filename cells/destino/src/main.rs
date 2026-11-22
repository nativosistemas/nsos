//! Cell `destino`: supervisor de la máquina a la que llega un servicio
//! (hito 10, modelo: `verification/lean/Nsos/Migration.lean`).
//!
//! Capacidades de arranque:
//! 1. Canal de control con `enlace` (la red autenticada hacia la otra máquina).
//! 2. Imagen de `contador-roto`. 3. Imagen de `contador-v2`.
//!
//! Habla el contrato `restore` de los dos lados: es el servidor para el
//! enlace (varios intentos por el mismo canal) y el cliente para cada
//! versión que arranca. Cuando el enlace trae un estado (`Restore`, con el
//! nombre de la versión pedida como modo), arranca esa versión sin
//! capacidades de servicio, le pasa estado y extremo y devuelve su decisión
//! al enlace. Los recursos son locales: el estado es una copia (de solo
//! lectura) del de la otra máquina, así que un rechazo no puede dañar el
//! original. Termina con 0 cuando la
//! versión aceptada termina bien (el cliente remoto cerró).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::cell_state;
use nsos_rt::abi::{CapHandle, Error};
use nsos_rt::contract::protocols::restore;
use nsos_rt::contract::{Bytes, Chan};
use nsos_rt::{bootstrap, cap_drop, cell_spawn, cell_status, println, Sys};

const MAX_ATTEMPTS: u32 = 8;

nsos_rt::entry!(main);

fn wait_ok(name: &str, h: CapHandle) -> Result<(), Error> {
    let st = cell_status(h, 0)?;
    cap_drop(h)?;
    if st.state == cell_state::EXITED && st.code == 0 {
        println!("destino: {} terminó bien", name);
        Ok(())
    } else {
        println!(
            "destino: {} terminó mal (estado {}, código {})",
            name, st.state, st.code
        );
        Err(Error::BadMessage)
    }
}

fn run() -> Result<(), Error> {
    let mut link = Chan::<Sys, restore::server::Start>::open(Sys, bootstrap(1))?;
    for _ in 0..MAX_ATTEMPTS {
        let Ok(restore::server::recv::Start::Restore(deciding, r)) = link.recv() else {
            return Err(Error::BadMessage);
        };
        let (name, img) = match r.mode.as_slice() {
            b"contador-roto" => ("contador-roto", bootstrap(2)),
            b"contador-v2" => ("contador-v2", bootstrap(3)),
            _ => {
                // No es un error de la otra máquina: se rechaza como haría
                // una versión que no entiende el estado.
                println!("destino: no tengo esa versión; la rechazo");
                link = deciding.send(restore::Reject {
                    state: r.state,
                    service: r.service,
                })?;
                continue;
            }
        };
        println!("destino: llegó un estado; arranco {}", name);
        let (ctl, ctl_child) = restore::pair(Sys)?;
        let cell = cell_spawn(img, &[ctl_child.into_raw()])?;
        let ctl = ctl.send(restore::Restore {
            state: r.state,
            service: r.service,
            mode: Bytes::new(b"").unwrap(),
        })?;
        match ctl.recv() {
            Ok(restore::client::recv::Deciding::Accept(end, _)) => {
                end.close();
                deciding.send(restore::Accept {})?.close();
                return wait_ok(name, cell);
            }
            Ok(restore::client::recv::Deciding::Reject(_start, back)) => {
                // Pasan por el enlace, que las descarta: son copias locales.
                link = deciding.send(restore::Reject {
                    state: back.state,
                    service: back.service,
                })?;
                wait_ok(name, cell)?;
            }
            Err(_) => return Err(Error::BadMessage),
        }
    }
    Err(Error::TimedOut)
}

fn main() -> i32 {
    match run() {
        Ok(()) => {
            println!("destino: migración OK");
            0
        }
        Err(e) => {
            println!("destino: falló: {:?}", e);
            1
        }
    }
}
