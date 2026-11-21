//! Cell `init`: supervisor.
//!
//! Capacidades de arranque (imágenes, con `EXEC`):
//! 1. `fragil`: una cell que falla las primeras veces.
//! 2. `dormilona`: una cell que se queda bloqueada.
//! 3. `contador-v1`, 4. `contador-v2`: dos versiones de un servidor con
//!    estado.
//! 5. `cliente`: cliente del contador.
//! 6. `contador-roto`: una versión que rechaza el estado o, si se lo
//!    piden, se cae (para los dos tipos de rollback).
//!
//! Fase 1, reinicio: arranca `fragil` con un canal nuevo en cada intento,
//! le dice qué intento es, espera a que termine y, si no terminó con 0, la
//! vuelve a arrancar (hasta `MAX_ATTEMPTS`). El resto del sistema sigue
//! corriendo mientras tanto.
//!
//! Fase 2, terminación: arranca `dormilona` tres veces (bloqueada en el
//! kernel, girando en anillo 3 y dormida en `wait`), comprueba que sigue
//! viva, la mata con `cell_kill`, la espera con `wait` (`CELL_EXITED`) y
//! verifica que terminó como `KILLED` y que su canal quedó cerrado.
//!
//! Fase 3, actualización en vivo (modelo:
//! `verification/lean/Nsos/Upgrade.lean`): arranca `contador-v1` y su
//! `cliente`. Primero intenta actualizar a `contador-roto`, que rechaza el
//! estado: rollback a la v1, que retoma. Después a `contador-roto` en modo
//! "caer": intenta escribir el estado, el kernel la termina por la excepción
//! y `init` hace rollback igual. Por último actualiza a `contador-v2`, que
//! acepta: commit y la v1 termina. En cada intento la versión nueva arranca
//! sin capacidades de servicio, la v1 entrega estado y extremo del cliente
//! (`UPGRADE_PREPARE` → `UPGRADE_STATE`) y `init` le pasa a la nueva el
//! extremo y una copia **de solo lectura** del estado (`UPGRADE_RESTORE`),
//! quedándose con el estado escribible y una copia del extremo hasta que la
//! nueva decide. El cliente nunca deja de estar conectado y verifica que el
//! contador no pierde ni repite valores.
//!
//! Termina con 0 si todo salió como se esperaba.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::cap::signals;
use nsos_rt::abi::msg::labels::{
    UPGRADE_ACCEPT, UPGRADE_COMMIT, UPGRADE_PREPARE, UPGRADE_REJECT, UPGRADE_RESTORE,
    UPGRADE_ROLLBACK, UPGRADE_STATE,
};
use nsos_rt::abi::syscall::{cell_state, flags, CellStatus, WAIT_FOREVER};
use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    bootstrap, cap_drop, cap_dup, cell_kill, cell_spawn, cell_status, channel_create, channel_recv,
    channel_send, println, wait, wait_item, yield_now,
};

const ATTEMPT: u64 = 1;
const READY: u64 = 2;
const SPIN: u64 = 3;
const WAIT: u64 = 4;
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

/// Cómo espera `dormilona` a que la maten.
#[derive(Clone, Copy)]
enum Sleep {
    /// Bloqueada en `channel_recv`.
    Blocked,
    /// Girando en anillo 3.
    Spinning,
    /// Bloqueada en `wait` sobre un canal propio que nunca recibe nada.
    Waiting,
}

fn kill_phase(image: CapHandle, how: Sleep) -> Result<(), Error> {
    let (mine, theirs) = channel_create()?;
    let child = cell_spawn(image, &[theirs])?;
    match how {
        Sleep::Blocked => {}
        Sleep::Spinning => channel_send(mine, &Message::new(SPIN, &[]).unwrap(), 0)?,
        Sleep::Waiting => channel_send(mine, &Message::new(WAIT, &[]).unwrap(), 0)?,
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
    // Dormir en `wait` hasta que termine (`CELL_EXITED`); su canal ya tiene
    // que estar cerrado: con plazo 0, `wait` lo ve enseguida.
    let mut ex = [wait_item(child, signals::CELL_EXITED)];
    if wait(&mut ex, WAIT_FOREVER)? != 1 || ex[0].observed != signals::CELL_EXITED {
        println!("init: wait no vio terminar a dormilona");
        return Err(Error::BadMessage);
    }
    let mut pc = [wait_item(mine, signals::PEER_CLOSED)];
    if wait(&mut pc, 0)? != 1 {
        println!("init: wait no vio cerrado el canal de dormilona");
        return Err(Error::BadMessage);
    }
    let after = cell_status(child, flags::NONBLOCK)?;
    println!(
        "init: dormilona ({}): {}",
        match how {
            Sleep::Blocked => "bloqueada",
            Sleep::Spinning => "girando",
            Sleep::Waiting => "en wait",
        },
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

/// Cómo terminó un intento de actualización.
#[derive(PartialEq)]
enum Outcome {
    Accepted,
    Rejected,
    Crashed,
}

/// Envía `UPGRADE_ROLLBACK [mem, ep]` a la versión vieja.
fn rollback(ctl_old: CapHandle, mem: CapHandle, ep: CapHandle) -> Result<(), Error> {
    let mut back = Message::new(UPGRADE_ROLLBACK, b"").unwrap();
    back.push_cap(mem)?;
    back.push_cap(ep)?;
    channel_send(ctl_old, &back, 0)
}

/// Intenta actualizar el servicio de `ctl_old` a la imagen `img` (con
/// `mode` como datos de `UPGRADE_RESTORE`). Devuelve cómo terminó y la
/// capacidad a la cell nueva.
fn try_upgrade(
    ctl_old: CapHandle,
    img: CapHandle,
    name: &str,
    mode: &[u8],
) -> Result<(Outcome, CapHandle), Error> {
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
    let [mem, ep] = *m.cap_slice() else {
        println!("init: respuesta inesperada de contador-v1");
        return Err(Error::BadMessage);
    };
    if m.header.label != UPGRADE_STATE {
        println!("init: respuesta inesperada de contador-v1");
        return Err(Error::BadMessage);
    }
    // Respaldo hasta que la nueva decida: el estado escribible se queda
    // aquí (la nueva recibe una copia de solo lectura: el kernel no la deja
    // escribirlo) y una copia del extremo mantiene abierto el canal del
    // cliente aunque la nueva se caiga.
    let mem_ro = cap_dup(mem, Rights::READ | Rights::GRANT)?;
    let ep_backup = cap_dup(ep, Rights::ALL)?;
    let mut r = Message::new(UPGRADE_RESTORE, mode).unwrap();
    r.push_cap(mem_ro)?;
    r.push_cap(ep)?;
    channel_send(ctl_new, &r, 0)?;
    let outcome = match channel_recv(ctl_new, &mut m, 0) {
        Ok(()) => match (m.header.label, m.cap_slice()) {
            (UPGRADE_ACCEPT, []) => {
                println!("init: {} aceptó; commit", name);
                channel_send(ctl_old, &Message::new(UPGRADE_COMMIT, b"").unwrap(), 0)?;
                cap_drop(mem)?;
                cap_drop(ep_backup)?;
                Outcome::Accepted
            }
            (UPGRADE_REJECT, &[mem_back, ep_back]) => {
                println!("init: {} rechazó el estado; rollback a contador-v1", name);
                cap_drop(mem_back)?;
                cap_drop(ep_backup)?;
                rollback(ctl_old, mem, ep_back)?;
                Outcome::Rejected
            }
            _ => {
                println!("init: respuesta inesperada de {}", name);
                return Err(Error::BadMessage);
            }
        },
        // La nueva se cayó antes de decidir: su extremo de control se cerró.
        Err(Error::PeerClosed) => {
            println!("init: {} se cayó; rollback a contador-v1", name);
            rollback(ctl_old, mem, ep_backup)?;
            Outcome::Crashed
        }
        Err(e) => return Err(e),
    };
    cap_drop(ctl_new)?;
    Ok((outcome, new))
}

fn expect_faulted(name: &str, h: CapHandle) -> Result<(), Error> {
    let st = cell_status(h, 0)?;
    println!("init: {}: {}", name, describe(&st));
    cap_drop(h)?;
    if st.state == cell_state::FAULTED {
        Ok(())
    } else {
        Err(Error::BadMessage)
    }
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

    let (outcome, broken) = try_upgrade(ctl1, broken_img, "contador-roto", b"")?;
    if outcome != Outcome::Rejected {
        return Err(Error::BadMessage);
    }
    expect_ok("contador-roto", broken)?;

    let (outcome, crashing) = try_upgrade(ctl1, broken_img, "contador-roto (caer)", b"caer")?;
    if outcome != Outcome::Crashed {
        return Err(Error::BadMessage);
    }
    expect_faulted("contador-roto (caer)", crashing)?;

    let (outcome, v2) = try_upgrade(ctl1, v2_img, "contador-v2", b"")?;
    if outcome != Outcome::Accepted {
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
    for how in [Sleep::Blocked, Sleep::Spinning, Sleep::Waiting] {
        if let Err(e) = kill_phase(dormilona, how) {
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
