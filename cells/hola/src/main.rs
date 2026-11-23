//! Cell `hola`: cliente del driver `virtcon`.
//!
//! Capacidad de arranque 1: canal hacia el driver de consola virtio
//! (cliente del contrato `console`). Envía algunas líneas con `Write`,
//! verifica cada `Written` y termina con 0. Al terminar, el kernel cierra su
//! extremo y el driver también termina.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::contract::protocols::console;
use nsos_rt::contract::{Bytes, Chan};
use nsos_rt::{bootstrap, println, Sys};

const LINES: [&[u8]; 3] = [
    b"hola desde una cell de nsos\n",
    b"este texto viaja por IPC hasta el driver virtio en user-space\n",
    b"chau\n",
];

nsos_rt::entry!(main);

fn main() -> i32 {
    let Ok(mut drv) = Chan::<Sys, console::client::Ready>::open(Sys, bootstrap(1)) else {
        println!("hola: la capacidad de arranque no es un canal");
        return 1;
    };
    for line in LINES {
        let Some(text) = Bytes::new(line) else {
            return 1;
        };
        drv = match drv.call(console::Write { text }) {
            Ok(console::client::recv::Writing::Written(c, w)) if w.n as usize == line.len() => c,
            Ok(console::client::recv::Writing::Written(_, w)) => {
                println!("hola: el driver escribió {} bytes de {}", w.n, line.len());
                return 3;
            }
            Err(e) => {
                println!("hola: falló: {:?}", e);
                return 2;
            }
        };
    }
    println!("hola: {} líneas escritas por virtio-console", LINES.len());
    0
}
