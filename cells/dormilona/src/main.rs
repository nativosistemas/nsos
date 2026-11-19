//! Cell `dormilona`: nunca termina sola. Sirve para probar `cell_kill`.
//!
//! Capacidad de arranque 1: canal hacia `init`. Espera un mensaje:
//! * si no llega, queda bloqueada en el kernel para siempre;
//! * si llega `SPIN`, gira en anillo 3 para siempre (solo la saca el timer).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::Message;
use nsos_rt::{bootstrap, channel_recv, println};

const SPIN: u64 = 3;

nsos_rt::entry!(main);

fn main() -> i32 {
    println!("dormilona: espero un mensaje");
    let mut m = Message::default();
    if channel_recv(bootstrap(1), &mut m, 0).is_ok() && m.header.label == SPIN {
        println!("dormilona: giro en anillo 3");
        loop {
            core::hint::spin_loop();
        }
    }
    println!("dormilona: ¡no debería llegar acá!");
    1
}
