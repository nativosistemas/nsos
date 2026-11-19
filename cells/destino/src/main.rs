//! Cell `destino`: supervisor de la máquina a la que llega un servicio
//! (hito 10, modelo: `verification/lean/Nsos/Migration.lean`).
//!
//! Capacidades de arranque:
//! 1. Canal de control con `enlace` (la red autenticada hacia la otra máquina).
//! 2. Imagen de `contador-roto`. 3. Imagen de `contador-v2`.
//!
//! Cuando el enlace trae un estado (`UPGRADE_RESTORE [mem, ep]` con el
//! nombre de la versión pedida), arranca esa versión sin capacidades de
//! servicio, le pasa `[mem, ep]` y devuelve su decisión al enlace. Los
//! recursos son locales: `mem` es una copia del estado de la otra máquina,
//! así que un rechazo no puede dañar el original. Termina con 0 cuando la
//! versión aceptada termina bien (el cliente remoto cerró).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::labels::{UPGRADE_ACCEPT, UPGRADE_REJECT, UPGRADE_RESTORE};
use nsos_rt::abi::syscall::cell_state;
use nsos_rt::abi::{CapHandle, Error, Message};
use nsos_rt::{
    bootstrap, cap_drop, cell_spawn, cell_status, channel_create, channel_recv, channel_send,
    println,
};

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
    let link = bootstrap(1);
    let mut m = Message::default();
    for _ in 0..MAX_ATTEMPTS {
        channel_recv(link, &mut m, 0)?;
        if m.header.label != UPGRADE_RESTORE || m.cap_slice().len() != 2 {
            return Err(Error::BadMessage);
        }
        let (name, img) = match m.payload() {
            b"contador-roto" => ("contador-roto", bootstrap(2)),
            b"contador-v2" => ("contador-v2", bootstrap(3)),
            _ => {
                // No es un error de la otra máquina: se rechaza como haría
                // una versión que no entiende el estado.
                println!("destino: no tengo esa versión; la rechazo");
                let mut back = Message::new(UPGRADE_REJECT, b"")?;
                for &c in m.cap_slice() {
                    back.push_cap(c)?;
                }
                channel_send(link, &back, 0)?;
                continue;
            }
        };
        println!("destino: llegó un estado; arranco {}", name);
        let (ctl, ctl_child) = channel_create()?;
        let cell = cell_spawn(img, &[ctl_child])?;
        let mut r = Message::new(UPGRADE_RESTORE, b"")?;
        for &c in m.cap_slice() {
            r.push_cap(c)?;
        }
        channel_send(ctl, &r, 0)?;
        channel_recv(ctl, &mut m, 0)?;
        cap_drop(ctl)?;
        match (m.header.label, m.cap_slice().len()) {
            (UPGRADE_ACCEPT, 0) => {
                channel_send(link, &Message::new(UPGRADE_ACCEPT, b"")?, 0)?;
                return wait_ok(name, cell);
            }
            (UPGRADE_REJECT, 2) => {
                // Pasan por el enlace, que las descarta: son copias locales.
                let mut back = Message::new(UPGRADE_REJECT, b"")?;
                for &c in m.cap_slice() {
                    back.push_cap(c)?;
                }
                channel_send(link, &back, 0)?;
                wait_ok(name, cell)?;
            }
            _ => return Err(Error::BadMessage),
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
