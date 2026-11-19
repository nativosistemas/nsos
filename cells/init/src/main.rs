//! Cell `init`: supervisor.
//!
//! Capacidades de arranque (imágenes, con `EXEC`):
//! 1. `fragil`: una cell que falla las primeras veces.
//! 2. `dormilona`: una cell que se queda bloqueada.
//! 3. `contador-v1`, 4. `contador-v2`: dos versiones de un servidor con
//!    estado.
//! 5. `cliente`: cliente del contador.
//! 6. `contador-roto`: una versión que rechaza el estado (para el rollback).
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
//! Fase 3, actualización en vivo (modelo:
//! `verification/lean/Nsos/Upgrade.lean`): arranca `contador-v1` y su
//! `cliente`. Primero intenta actualizar a `contador-roto`, que rechaza el
//! estado: rollback a la v1, que retoma. Después actualiza a `contador-v2`,
//! que acepta: commit y la v1 termina. En cada intento la versión nueva
//! arranca sin capacidades de servicio, la v1 entrega estado y extremo del
//! cliente (`UPGRADE_PREPARE` → `UPGRADE_STATE`) y `init` se los pasa a la
//! nueva (`UPGRADE_RESTORE`). El cliente nunca deja de estar conectado y
//! verifica que el contador no pierde ni repite valores.
//!
//! Termina con 0 si todo salió como se esperaba.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::labels::{
    UPGRADE_ACCEPT, UPGRADE_COMMIT, UPGRADE_PREPARE, UPGRADE_REJECT, UPGRADE_RESTORE,
    UPGRADE_ROLLBACK, UPGRADE_STATE,
};
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

fn expect_ok(name: &str, h: CapHandle) -> Result<(), Error> {
    let st = cell_status(h, 0)?;
    println!("init: {}: {}", name, describe(&st));
    cap_drop(h)?;
    if st.state == cell_state::EXITED && st.code == 0 {
        Ok(())
    } else {
        Err(Error::BadMessage)
    }
}

/// Intenta actualizar el servicio de `ctl_old` a la imagen `img`. Devuelve
/// si la versión nueva aceptó y la capacidad a su cell.
fn try_upgrade(ctl_old: CapHandle, img: CapHandle, name: &str) -> Result<(bool, CapHandle), Error> {
    // La versión nueva arranca sin capacidades de servicio.
    let (ctl_new, ctl_child) = channel_create()?;
    let new = cell_spawn(img, &[ctl_child])?;

    println!(
        "init: pido a contador-v1 que entregue su estado (para {})",
        name
    );
    channel_send(ctl_old, &Message::new(UPGRADE_PREPARE, b"").unwrap(), 0)?;
    let mut m = Message::default();
    channel_recv(ctl_old, &mut m, 0)?;
    if m.header.label != UPGRADE_STATE || m.cap_slice().len() != 2 {
        println!("init: respuesta inesperada de contador-v1");
        return Err(Error::BadMessage);
    }
    // Reenviar estado y canal del cliente, tal cual, a la nueva.
    let mut r = Message::new(UPGRADE_RESTORE, b"").unwrap();
    for &c in m.cap_slice() {
        r.push_cap(c)?;
    }
    channel_send(ctl_new, &r, 0)?;
    channel_recv(ctl_new, &mut m, 0)?;
    let accepted = match (m.header.label, m.cap_slice().len()) {
        (UPGRADE_ACCEPT, 0) => {
            println!("init: {} aceptó; commit", name);
            channel_send(ctl_old, &Message::new(UPGRADE_COMMIT, b"").unwrap(), 0)?;
            true
        }
        (UPGRADE_REJECT, 2) => {
            println!("init: {} rechazó el estado; rollback a contador-v1", name);
            let mut back = Message::new(UPGRADE_ROLLBACK, b"").unwrap();
            for &c in m.cap_slice() {
                back.push_cap(c)?;
            }
            channel_send(ctl_old, &back, 0)?;
            false
        }
        _ => {
            println!("init: respuesta inesperada de {}", name);
            return Err(Error::BadMessage);
        }
    };
    cap_drop(ctl_new)?;
    Ok((accepted, new))
}

fn upgrade_phase(
    v1_img: CapHandle,
    v2_img: CapHandle,
    broken_img: CapHandle,
    client_img: CapHandle,
) -> Result<(), Error> {
    let (srv_end, client_end) = channel_create()?;
    let (ctl1, ctl1_child) = channel_create()?;
    let v1 = cell_spawn(v1_img, &[ctl1_child, srv_end])?;
    let client = cell_spawn(client_img, &[client_end])?;

    let (accepted, broken) = try_upgrade(ctl1, broken_img, "contador-roto")?;
    if accepted {
        return Err(Error::BadMessage);
    }
    expect_ok("contador-roto", broken)?;

    let (accepted, v2) = try_upgrade(ctl1, v2_img, "contador-v2")?;
    if !accepted {
        return Err(Error::BadMessage);
    }
    expect_ok("contador-v1", v1)?;
    expect_ok("cliente", client)?;
    expect_ok("contador-v2", v2)?;
    cap_drop(ctl1)
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
    if let Err(e) = upgrade_phase(bootstrap(3), bootstrap(4), bootstrap(6), bootstrap(5)) {
        println!("init: fase de actualización falló: {:?}", e);
        return 3;
    }
    println!("init: supervisión OK");
    0
}
