//! Cell `curioso`: otro cliente del almacén (dueño 2), para probar el
//! aislamiento entre dueños.
//!
//! 1. Abre "contador" por **su** raíz: es su propio objeto, no el del
//!    `escritor` (dueño 1), aunque se llame igual. Lo escribe.
//! 2. Recibe del `escritor` una copia de solo lectura del objeto de él.
//!    Puede leerla (y comprobar que está entera), pero escribir con ella se
//!    rechaza, y copiarla de nuevo da otra de solo lectura.
//!
//! Capacidades de arranque: 1. raíz del dueño 2; 2. canal con `escritor`.
//! Responde `RESULT [1]` al escritor si todo fue como se esperaba y sale
//! con 0; si no, `RESULT [0]` y sale con 1.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::{CapHandle, Message, Rights};
use nsos_rt::{bootstrap, channel_recv, channel_send, mem_create, mem_map, println};
use nsos_store::proto::{self, demo_word, u32_at, u64_at};

const MEM_VADDR: u64 = 0x4000_0000;
const SIZE: usize = 16 * 1024;
const SHARE: u64 = 1;
const RESULT: u64 = 2;

fn call(ch: CapHandle, m: Message) -> Option<Message> {
    channel_send(ch, &m, 0).ok()?;
    let mut r = Message::default();
    channel_recv(ch, &mut r, 0).ok()?;
    Some(r)
}

fn with_mem(label: u64, p: &[u8], mem: CapHandle) -> Option<Message> {
    let mut m = Message::new(label, p).ok()?;
    m.push_cap(mem).ok()?;
    Some(m)
}

fn check(what: &str, ok: bool) -> bool {
    println!("curioso: {} ... {}", what, if ok { "ok" } else { "FALLO" });
    ok
}

fn run() -> Option<bool> {
    let root = bootstrap(1);
    let peer = bootstrap(2);
    let mut ok = true;

    // 1. Mi "contador" (dueño 2) es otro objeto.
    let mut p = [0u8; 9];
    p[0] = proto::OPEN_WRITE;
    p[1..].copy_from_slice(b"contador");
    let r = call(root, Message::new(proto::OPEN, &p).ok()?)?;
    let [mine] = *r.cap_slice() else {
        return Some(false);
    };
    let v0 = u64_at(r.payload(), 0);
    let mem = mem_create(SIZE).ok()?;
    let map = mem_map(mem, MEM_VADDR, Rights::READ | Rights::WRITE).ok()?;
    map.write_bytes(0, b"soy el curioso");
    let mut wp = [0u8; 12];
    wp[..8].copy_from_slice(&proto::ANY_VERSION.to_le_bytes());
    wp[8..].copy_from_slice(&14u32.to_le_bytes());
    let r = call(mine, with_mem(proto::WRITE, &wp, mem)?)?;
    let [mut mem] = *r.cap_slice() else {
        return Some(false);
    };
    ok &= check(
        "mi \"contador\" es mío (otro dueño, otro objeto) y lo puedo escribir",
        r.header.label == proto::WRITTEN && u64_at(r.payload(), 0) == v0 + 1,
    );

    // 2. La copia de solo lectura del escritor.
    let mut m = Message::default();
    channel_recv(peer, &mut m, 0).ok()?;
    let [ro] = *m.cap_slice() else {
        return Some(false);
    };
    ok &= check(
        "me llegó una copia de solo lectura",
        m.header.label == SHARE,
    );
    let r = call(ro, Message::new(proto::STAT, &[]).ok()?)?;
    let v = u64_at(r.payload(), 0);
    ok &= check(
        "la copia dice que no escribe",
        r.header.label == proto::INFO && r.payload().get(12) == Some(&0),
    );
    let r = call(ro, with_mem(proto::READ, &[], mem)?)?;
    let [m2] = *r.cap_slice() else {
        return Some(false);
    };
    mem = m2;
    let len = u32_at(r.payload(), 8) as usize;
    let map = mem_map(mem, MEM_VADDR, Rights::READ).ok()?;
    let entero = (0..len / 8).all(|k| map.read_u64(k * 8) == demo_word(v, k));
    ok &= check(
        "puedo leer el objeto del escritor, entero",
        r.header.label == proto::DATA && len == SIZE && entero,
    );
    let mut wp = [0u8; 12];
    wp[..8].copy_from_slice(&proto::ANY_VERSION.to_le_bytes());
    wp[8..].copy_from_slice(&8u32.to_le_bytes());
    let r = call(ro, with_mem(proto::WRITE, &wp, mem)?)?;
    ok &= check(
        "escribir con la copia de solo lectura: rechazado",
        r.header.label == proto::ERR && u32_at(r.payload(), 0) == proto::E_READ_ONLY,
    );
    let r = call(ro, Message::new(proto::DUP_RO, &[]).ok()?)?;
    let [ro2] = *r.cap_slice() else {
        return Some(false);
    };
    let r2 = call(ro2, Message::new(proto::STAT, &[]).ok()?)?;
    ok &= check(
        "copiar la copia da otra de solo lectura",
        r.header.label == proto::OPENED && r2.payload().get(12) == Some(&0),
    );
    let res = Message::new(RESULT, &[ok as u8]).ok()?;
    channel_send(peer, &res, 0).ok()?;
    Some(ok)
}

nsos_rt::entry!(main);

fn main() -> i32 {
    match run() {
        Some(true) => 0,
        _ => {
            println!("curioso: algo no salió como se esperaba");
            1
        }
    }
}
