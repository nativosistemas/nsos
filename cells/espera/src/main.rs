//! Cell `espera`: pruebas de la syscall `wait` (hito 12).
//!
//! Arranca sin capacidades, crea sus propios canales y comprueba la
//! semántica de `wait`: con plazo 0 solo mira; con plazo vence a tiempo;
//! cada ítem informa sus señales (`READABLE`, `WRITABLE`, `PEER_CLOSED`)
//! aunque no estén pedidas; con varios ítems cuenta solo los listos; y
//! `WRITABLE` sigue a la cola del peer. Termina con 0 si todo dio lo
//! esperado. Las esperas entre cells las prueban `contador-v1` (duerme en
//! `wait` hasta que el `cliente` le escribe) e `init` (`CELL_EXITED` y
//! `cell_kill` de una cell dormida en `wait`).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::cap::signals::{PEER_CLOSED, READABLE, WRITABLE};
use nsos_rt::abi::syscall::flags::NONBLOCK;
use nsos_rt::abi::{Error, Message};
use nsos_rt::{
    cap_drop, channel_create, channel_recv, channel_send, clock_ns, println, wait, wait_item,
};

/// Plazo de la espera que tiene que vencer.
const SLEEP_NS: u64 = 20_000_000;

nsos_rt::entry!(main);

fn check(name: &str, ok: bool) -> bool {
    println!("espera: {:<48} {}", name, if ok { "ok" } else { "¡FALLÓ!" });
    ok
}

fn run() -> Result<bool, Error> {
    let mut ok = true;
    let (a, b) = channel_create()?;
    let (c, _d) = channel_create()?;

    let mut it = [wait_item(b, READABLE)];
    ok &= check(
        "plazo 0 sobre un canal vacío: TimedOut",
        wait(&mut it, 0) == Err(Error::TimedOut) && it[0].observed == WRITABLE,
    );

    let t0 = clock_ns();
    let r = wait(&mut it, SLEEP_NS);
    let dt = clock_ns().saturating_sub(t0);
    println!("espera: plazo de {} ns vencido a los {} ns", SLEEP_NS, dt);
    ok &= check(
        "plazo de 20 ms: TimedOut, no antes",
        r == Err(Error::TimedOut) && dt >= SLEEP_NS,
    );

    channel_send(a, &Message::new(7, b"hola").unwrap(), 0)?;
    ok &= check(
        "con un mensaje: READABLE (y WRITABLE)",
        wait(&mut it, 0) == Ok(1) && it[0].observed == READABLE | WRITABLE,
    );

    let mut two = [wait_item(c, READABLE), wait_item(b, READABLE)];
    ok &= check(
        "dos ítems, uno listo: cuenta 1",
        wait(&mut two, 0) == Ok(1)
            && two[0].observed & READABLE == 0
            && two[1].observed & READABLE != 0,
    );
    let mut m = Message::default();
    channel_recv(b, &mut m, 0)?;

    // Llenar la cola de `b`: `a` deja de ser WRITABLE hasta que `b` lea.
    let mut sent = 0;
    while channel_send(a, &Message::new(1, b"").unwrap(), NONBLOCK).is_ok() {
        sent += 1;
    }
    let mut w = [wait_item(a, WRITABLE)];
    ok &= check(
        "cola del peer llena: sin WRITABLE",
        sent > 0 && wait(&mut w, 0) == Err(Error::TimedOut),
    );
    channel_recv(b, &mut m, 0)?;
    ok &= check(
        "el peer leyó uno: WRITABLE",
        wait(&mut w, 0) == Ok(1) && w[0].observed == WRITABLE,
    );
    while channel_recv(b, &mut m, NONBLOCK).is_ok() {}

    cap_drop(a)?;
    let mut pc = [wait_item(b, READABLE | PEER_CLOSED)];
    ok &= check(
        "el peer se cerró: PEER_CLOSED",
        wait(&mut pc, 0) == Ok(1) && pc[0].observed == PEER_CLOSED,
    );
    Ok(ok)
}

fn main() -> i32 {
    match run() {
        Ok(true) => {
            println!("espera: wait OK");
            0
        }
        Ok(false) => 1,
        Err(e) => {
            println!("espera: error inesperado {:?}", e);
            2
        }
    }
}
