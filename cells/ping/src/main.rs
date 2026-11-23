//! Cell `ping`: cliente del contrato `echo`
//! (`nsos_contract::protocols::echo`) que ejercita el IPC.
//!
//! Capacidad de arranque 1: extremo de canal hacia `pong`.
//! 0. Prueba negativa del contrato: con la API cruda (sin contrato) le
//!    manda a `pong` cinco mensajes fuera de protocolo, uno de ellos un
//!    `NewChannel` con un canal creado para otro contrato. `pong` tiene que
//!    descartarlos sin cambiar de estado (el eco siguiente sale bien y no
//!    llega ninguna respuesta de más) y soltar las capacidades que venían
//!    (la otra punta de esos canales ve `PEER_CLOSED`).
//! 1. Diez ecos con verificación de contenido.
//! 2. Crea un canal `note` y le *mueve* un extremo a `pong`; comprueba que
//!    perdió ese handle y conversa por el canal nuevo.
//! 3. Mide ida y vuelta de IPC (benchmark).
//! 4. Se despide y termina con 0.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use core::fmt::Write;
use nsos_rt::abi::cap::signals::PEER_CLOSED;
use nsos_rt::abi::syscall::Syscall;
use nsos_rt::abi::{CapHandle, Error, Message};
use nsos_rt::contract::protocols::{echo, note, store_obj};
use nsos_rt::contract::{Bytes, Chan};
use nsos_rt::{
    bootstrap, cap_info, channel_create, channel_create_for, channel_send, println, raw_syscall,
    wait, wait_item, Sys,
};

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

type Ready = Chan<Sys, echo::client::Ready>;

/// Un eco: devuelve el canal otra vez en `Ready` y lo que contestó `pong`.
fn echo(ch: Ready, data: &[u8]) -> Result<(Ready, Bytes<224>), Error> {
    let data = Bytes::new(data).ok_or(Error::InvalidArgs)?;
    let ch = ch.send(echo::Echo { data })?;
    match ch.recv() {
        Ok(echo::client::recv::Waiting::EchoReply(ch, r)) => Ok((ch, r.data)),
        Err(_) => Err(Error::BadMessage),
    }
}

/// Manda un mensaje armado a mano, sin contrato (como lo haría una cell
/// cualquiera).
fn raw(h: CapHandle, label: u64, cap: Option<CapHandle>) -> Result<(), Error> {
    let mut m = Message::new(label, b"fuera de protocolo")?;
    if let Some(c) = cap {
        m.push_cap(c)?;
    }
    channel_send(h, &m, 0)
}

/// Paso 0. Devuelve el canal, que sigue en `Ready`.
fn violations(ch: Ready) -> Result<Ready, i32> {
    let h = ch.handle();
    let (mine, theirs) = channel_create().map_err(|_| 20)?;
    // Un canal creado para otro protocolo (el de los handles del almacén):
    // no puede pasar por el canal `note` que espera `NewChannel`.
    let (alien_mine, alien) = channel_create_for(store_obj::ID).map_err(|_| 20)?;
    // `EchoReply` es del contrato pero lo manda el servidor; 777 no existe;
    // `Echo` no lleva capacidades; `NewChannel` sin extremo, y con un
    // extremo de otro contrato (`channel_contract` lo delata).
    let sent = raw(h, 2, None)
        .and_then(|_| raw(h, 777, None))
        .and_then(|_| raw(h, 1, Some(theirs)))
        .and_then(|_| raw(h, 100, None))
        .and_then(|_| raw(h, 100, Some(alien)));
    if sent.is_err() {
        return Err(21);
    }
    // `pong` procesa en orden: cuando contesta este eco ya descartó los
    // cinco. Si hubiera contestado alguno, esta respuesta sería otra.
    let (ch, r) = echo(ch, b"despues de los intrusos").map_err(|_| 22)?;
    if r.as_slice() != b"despues de los intrusos" {
        println!("ping: ERROR, pong contestó un mensaje fuera de protocolo");
        return Err(23);
    }
    let mut items = [
        wait_item(mine, PEER_CLOSED),
        wait_item(alien_mine, PEER_CLOSED),
    ];
    if wait(&mut items, 0) != Ok(2) {
        println!("ping: ERROR, pong no soltó las capacidades de los mensajes descartados");
        return Err(24);
    }
    println!(
        "ping: 5 mensajes fuera de protocolo (uno con un canal de otro contrato): pong los descartó, soltó las capacidades y sigue en Ready"
    );
    Ok(ch)
}

fn main() -> i32 {
    let Ok(ch) = Ready::open(Sys, bootstrap(1)) else {
        println!("ping: la capacidad de arranque no es un canal");
        return 1;
    };

    // 0. Prueba negativa del contrato.
    let mut ch = match violations(ch) {
        Ok(ch) => ch,
        Err(code) => return code,
    };

    // 1. Ecos.
    for i in 0..10u32 {
        let mut b = Buf::<32> { b: [0; 32], n: 0 };
        let _ = write!(b, "ping {}", i);
        match echo(ch, &b.b[..b.n]) {
            Ok((c, r)) if r.as_slice() == &b.b[..b.n] => ch = c,
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
    let (mine, theirs) = match note::pair(Sys) {
        Ok(p) => p,
        Err(e) => {
            println!("ping: channel_create falló: {:?}", e);
            return 3;
        }
    };
    let old = theirs.handle();
    let Ok(adopting) = ch.send(echo::NewChannel {
        side: theirs.into_endpoint(),
    }) else {
        return 4;
    };
    if cap_info(old) != Err(Error::BadHandle) {
        println!("ping: ERROR, el handle transferido sigue vivo aquí");
        return 5;
    }
    println!("ping: transferí un extremo; mi handle viejo ya es inválido");
    let text = Bytes::new(b"hola por el canal nuevo").unwrap();
    let Ok(waiting) = mine.send(note::Note { text }) else {
        return 6;
    };
    let Ok(note::client::recv::Waiting::Ack(end, ack)) = waiting.recv() else {
        return 7;
    };
    end.close();
    println!(
        "ping: respuesta: \"{}\"",
        core::str::from_utf8(ack.text.as_slice()).unwrap_or("?")
    );
    let Ok(echo::client::recv::Adopting::Adopted(c, _)) = adopting.recv() else {
        return 9;
    };
    ch = c;

    // 3. Benchmark de ida y vuelta.
    let t0 = now_ns();
    for _ in 0..BENCH_ROUNDS {
        match echo(ch, b"x") {
            Ok((c, _)) => ch = c,
            Err(_) => return 8,
        }
    }
    let dt = now_ns() - t0;
    println!(
        "ping: benchmark IPC: {} idas y vueltas, {} ns promedio",
        BENCH_ROUNDS,
        dt / BENCH_ROUNDS
    );

    // 4. Fin.
    if let Ok(end) = ch.send(echo::Bye {}) {
        end.close();
    }
    0
}
