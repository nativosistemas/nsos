//! Cell `pong`: servidor de eco.
//!
//! Capacidad de arranque 1: extremo de canal hacia `ping`.
//! Protocolo:
//! * `ECHO`: responde `ECHO_REPLY` con los mismos datos.
//! * `NEW_CHANNEL` con una capacidad adjunta: la adopta como segundo canal,
//!   recibe un mensaje por él y contesta por el mismo canal.
//! * `BYE`: termina con código 0.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::{Error, Message};
use nsos_rt::{bootstrap, channel_recv, channel_send, println};

const ECHO: u64 = 1;
const ECHO_REPLY: u64 = 2;
const NEW_CHANNEL: u64 = 100;
const BYE: u64 = 999;

nsos_rt::entry!(main);

fn main() -> i32 {
    let ch = bootstrap(1);
    println!("pong: esperando mensajes");
    let mut m = Message::default();
    let mut served = 0u64;
    loop {
        if let Err(e) = channel_recv(ch, &mut m, 0) {
            println!("pong: recv falló: {:?}", e);
            return 1;
        }
        match m.header.label {
            ECHO => {
                let reply = Message::new(ECHO_REPLY, m.payload()).unwrap();
                if channel_send(ch, &reply, 0).is_err() {
                    return 2;
                }
                served += 1;
            }
            NEW_CHANNEL => {
                let Some(&side) = m.cap_slice().first() else {
                    println!("pong: NEW_CHANNEL sin capacidad");
                    return 3;
                };
                let mut q = Message::default();
                if channel_recv(side, &mut q, 0).is_err() {
                    return 4;
                }
                let text = core::str::from_utf8(q.payload()).unwrap_or("?");
                println!("pong: recibí por el canal transferido: \"{}\"", text);
                let r = Message::new(ECHO_REPLY, b"recibido por el canal nuevo").unwrap();
                if channel_send(side, &r, 0).is_err() {
                    return 5;
                }
            }
            BYE => {
                println!("pong: atendí {} ecos, chau", served);
                return 0;
            }
            other => {
                println!("pong: etiqueta desconocida {}", other);
                let _ = Error::BadMessage;
                return 6;
            }
        }
    }
}
