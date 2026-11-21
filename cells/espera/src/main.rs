//! Cell `espera`: pruebas de las syscalls `wait` (hito 12) y `channel_call`
//! (hito 13).
//!
//! Arranca sin capacidades, crea sus propios canales y comprueba la
//! semántica de `wait`: con plazo 0 solo mira; con plazo vence a tiempo;
//! cada ítem informa sus señales (`READABLE`, `WRITABLE`, `PEER_CLOSED`)
//! aunque no estén pedidas; con varios ítems cuenta solo los listos; y
//! `WRITABLE` sigue a la cola del peer. Termina con 0 si todo dio lo
//! esperado. Las esperas entre cells las prueban `contador-v1` (duerme en
//! `wait` hasta que el `cliente` le escribe) e `init` (`CELL_EXITED` y
//! `cell_kill` de una cell dormida en `wait`).
//!
//! `channel_call`: con la respuesta ya en la cola vuelve enseguida con ella
//! (y el pedido llegó al otro extremo); sin respuesta vence el plazo (el
//! pedido igual se mandó); adjuntar el propio extremo o no tener `RECV` se
//! rechaza sin mandar nada. Entre cells lo usan `cliente` y `bench`.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::cap::signals::{PEER_CLOSED, READABLE, WRITABLE};
use nsos_rt::abi::syscall::flags::NONBLOCK;
use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    cap_drop, cap_dup, channel_call, channel_create, channel_recv, channel_send, clock_ns, println,
    wait, wait_item,
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

/// `b` tiene exactamente un mensaje con etiqueta `label`.
fn got_one(b: CapHandle, label: u64) -> bool {
    let mut m = Message::default();
    channel_recv(b, &mut m, NONBLOCK).is_ok()
        && m.header.label == label
        && channel_recv(b, &mut m, NONBLOCK) == Err(Error::WouldBlock)
}

fn call_tests() -> Result<bool, Error> {
    const REQ: u64 = 10;
    const REP: u64 = 11;
    let mut ok = true;
    let (a, b) = channel_create()?;

    channel_send(b, &Message::new(REP, b"listo").unwrap(), 0)?;
    let mut m = Message::new(REQ, b"pedido").unwrap();
    let r = channel_call(a, &mut m, 0);
    ok &= check(
        "call con la respuesta en la cola: la recibe",
        r == Ok(()) && m.header.label == REP && m.payload() == b"listo",
    );
    ok &= check("  y el pedido llegó al otro extremo", got_one(b, REQ));

    let mut m = Message::new(REQ, b"").unwrap();
    let t0 = clock_ns();
    let r = channel_call(a, &mut m, SLEEP_NS);
    let dt = clock_ns().saturating_sub(t0);
    ok &= check(
        "call sin respuesta: TimedOut al vencer el plazo",
        r == Err(Error::TimedOut) && dt >= SLEEP_NS,
    );
    ok &= check("  pero el pedido se mandó", got_one(b, REQ));

    let mut m = Message::new(REQ, b"").unwrap();
    m.push_cap(a)?;
    ok &= check(
        "call adjuntando su propio extremo: InvalidArgs",
        channel_call(a, &mut m, 0) == Err(Error::InvalidArgs),
    );
    let only_send = cap_dup(a, Rights::SEND | Rights::DUPLICATE)?;
    let mut m = Message::new(REQ, b"").unwrap();
    ok &= check(
        "call sin derecho RECV: AccessDenied",
        channel_call(only_send, &mut m, 0) == Err(Error::AccessDenied),
    );
    let mut m = Message::default();
    ok &= check(
        "  ninguno de los dos mandó nada",
        channel_recv(b, &mut m, NONBLOCK) == Err(Error::WouldBlock),
    );

    cap_drop(b)?;
    let mut m = Message::new(REQ, b"").unwrap();
    ok &= check(
        "call con el peer cerrado: PeerClosed",
        channel_call(a, &mut m, 0) == Err(Error::PeerClosed),
    );
    Ok(ok)
}

fn main() -> i32 {
    match run().and_then(|ok| Ok(ok & call_tests()?)) {
        Ok(true) => {
            println!("espera: wait y channel_call OK");
            0
        }
        Ok(false) => 1,
        Err(e) => {
            println!("espera: error inesperado {:?}", e);
            2
        }
    }
}
