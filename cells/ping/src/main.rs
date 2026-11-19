//! Cell `ping`: cliente que ejercita el IPC.
//!
//! Capacidad de arranque 1: extremo de canal hacia `pong`.
//! 1. Diez ecos con verificación de contenido.
//! 2. Crea un canal nuevo y le *mueve* un extremo a `pong`; comprueba que
//!    perdió ese handle y conversa por el canal nuevo.
//! 3. Mide ida y vuelta de IPC (benchmark).
//! 4. Se despide y termina con 0.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use core::fmt::Write;
use nsos_rt::abi::syscall::Syscall;
use nsos_rt::abi::{Error, Message};
use nsos_rt::{
    bootstrap, cap_info, channel_create, channel_recv, channel_send, println, raw_syscall,
};

const ECHO: u64 = 1;
const ECHO_REPLY: u64 = 2;
const NEW_CHANNEL: u64 = 100;
const BYE: u64 = 999;
const BENCH_ROUNDS: u64 = 2000;

nsos_rt::entry!(main);

struct Buf<const N: usize> {
    b: [u8; N],
    n: usize,
}
impl<const N: usize> Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let end = (self.n + bytes.len()).min(N);
        self.b[self.n..end].copy_from_slice(&bytes[..end - self.n]);
        self.n = end;
        Ok(())
    }
}

fn now_ns() -> u64 {
    raw_syscall(Syscall::ClockMonotonic as u64, 0, 0, 0, 0).max(0) as u64
}

fn echo(ch: nsos_rt::abi::CapHandle, data: &[u8]) -> Result<Message, Error> {
    channel_send(ch, &Message::new(ECHO, data)?, 0)?;
    let mut r = Message::default();
    channel_recv(ch, &mut r, 0)?;
    if r.header.label != ECHO_REPLY {
        return Err(Error::BadMessage);
    }
    Ok(r)
}

fn main() -> i32 {
    let ch = bootstrap(1);

    // 1. Ecos.
    for i in 0..10u32 {
        let mut b = Buf::<32> { b: [0; 32], n: 0 };
        let _ = write!(b, "ping {}", i);
        match echo(ch, &b.b[..b.n]) {
            Ok(r) if r.payload() == &b.b[..b.n] => {}
            Ok(_) => {
                println!("ping: eco con datos distintos");
                return 1;
            }
            Err(e) => {
                println!("ping: eco falló: {:?}", e);
                return 2;
            }
        }
    }
    println!("ping: 10 ecos verificados");

    // 2. Transferencia de capacidad.
    let (mine, theirs) = match channel_create() {
        Ok(p) => p,
        Err(e) => {
            println!("ping: channel_create falló: {:?}", e);
            return 3;
        }
    };
    let mut m = Message::new(NEW_CHANNEL, b"").unwrap();
    m.push_cap(theirs).unwrap();
    if channel_send(ch, &m, 0).is_err() {
        return 4;
    }
    if cap_info(theirs) != Err(Error::BadHandle) {
        println!("ping: ERROR, el handle transferido sigue vivo aquí");
        return 5;
    }
    println!("ping: transferí un extremo; mi handle viejo ya es inválido");
    if channel_send(
        mine,
        &Message::new(0, b"hola por el canal nuevo").unwrap(),
        0,
    )
    .is_err()
    {
        return 6;
    }
    let mut r = Message::default();
    if channel_recv(mine, &mut r, 0).is_err() {
        return 7;
    }
    println!(
        "ping: respuesta: \"{}\"",
        core::str::from_utf8(r.payload()).unwrap_or("?")
    );

    // 3. Benchmark de ida y vuelta.
    let t0 = now_ns();
    for _ in 0..BENCH_ROUNDS {
        if echo(ch, b"x").is_err() {
            return 8;
        }
    }
    let dt = now_ns() - t0;
    println!(
        "ping: benchmark IPC: {} idas y vueltas, {} ns promedio",
        BENCH_ROUNDS,
        dt / BENCH_ROUNDS
    );

    // 4. Fin.
    let _ = channel_send(ch, &Message::new(BYE, b"").unwrap(), 0);
    0
}
