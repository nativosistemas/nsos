//! Cell `pong`: servidor del contrato `echo`
//! (`nsos_contract::protocols::echo`).
//!
//! Capacidad de arranque 1: extremo de canal hacia `ping` (o `bench`).
//! * `Echo`: responde `EchoReply` con los mismos datos.
//! * `NewChannel`: adopta el extremo de un canal `note`, recibe la nota por
//!   él, contesta `Ack` y después `Adopted` por el canal principal.
//! * `Bye`: termina con código 0.
//!
//! Lo que no sigue el contrato (una etiqueta que no corresponde, datos o
//! capacidades de más o de menos) lo descarta el decodificador, que suelta
//! sus capacidades; `pong` sigue en el mismo estado y al final informa
//! cuántos descartó.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::contract::protocols::{echo, note};
use nsos_rt::contract::{Bytes, Chan, RecvError};
use nsos_rt::{bootstrap, println, Sys};

use echo::server::recv::Ready;

nsos_rt::entry!(main);

/// Atiende un `NewChannel`: la conversación completa por el canal `note`.
fn adopt(side: Chan<Sys, note::server::Start>) -> Result<(), i32> {
    let note::server::recv::Start::Note(side, n) = side.recv().map_err(|e| {
        println!("pong: el canal transferido falló: {:?}", e);
        4
    })?;
    let text = core::str::from_utf8(n.text.as_slice()).unwrap_or("?");
    println!("pong: recibí por el canal transferido: \"{}\"", text);
    let ack = note::Ack {
        text: Bytes::new(b"recibido por el canal nuevo").unwrap(),
    };
    side.send(ack).map_err(|_| 5)?.close();
    Ok(())
}

fn main() -> i32 {
    let Ok(mut ch) = Chan::<Sys, echo::server::Ready>::open(Sys, bootstrap(1)) else {
        println!("pong: la capacidad de arranque no es un canal");
        return 1;
    };
    println!("pong: esperando mensajes");
    let (mut served, mut dropped) = (0u64, 0u64);
    loop {
        ch = match ch.recv() {
            Ok(Ready::Echo(c, m)) => match c.send(echo::EchoReply { data: m.data }) {
                Ok(c) => {
                    served += 1;
                    c
                }
                Err(_) => return 2,
            },
            Ok(Ready::NewChannel(c, m)) => {
                if let Err(code) = adopt(Chan::attach(Sys, m.side)) {
                    return code;
                }
                match c.send(echo::Adopted {}) {
                    Ok(c) => c,
                    Err(_) => return 6,
                }
            }
            Ok(Ready::Bye(end, _)) => {
                println!(
                    "pong: atendí {} ecos y descarté {} mensajes fuera de protocolo, chau",
                    served, dropped
                );
                end.close();
                return 0;
            }
            Err(RecvError::Violation { chan, label }) => {
                dropped += 1;
                println!(
                    "pong: mensaje fuera de protocolo (etiqueta {}), descartado",
                    label
                );
                chan
            }
            Err(RecvError::WouldBlock(c)) => c,
            Err(RecvError::Failed(e)) => {
                println!("pong: recv falló: {:?}", e);
                return 1;
            }
        }
    }
}
