//! Cell `dormilona`: nunca termina sola. Sirve para probar `cell_kill`.
//!
//! Capacidad de arranque 1: canal hacia `init`. Espera un mensaje:
//! * si no llega, queda bloqueada en el kernel para siempre;
//! * si llega `SPIN`, gira en anillo 3 para siempre (solo la saca el timer);
//! * si llega `WAIT`, duerme en `wait` sobre un canal propio al que nadie
//!   le escribe (prueba que `cell_kill` saca a una cell de `wait`).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::cap::signals::READABLE;
use nsos_rt::abi::syscall::WAIT_FOREVER;
use nsos_rt::abi::Message;
use nsos_rt::{bootstrap, channel_create, channel_recv, println, wait, wait_item};

const SPIN: u64 = 3;
const WAIT: u64 = 4;

nsos_rt::entry!(main);

fn main() -> i32 {
    println!("dormilona: espero un mensaje");
    let mut m = Message::default();
    if channel_recv(bootstrap(1), &mut m, 0).is_ok() {
        if m.header.label == SPIN {
            println!("dormilona: giro en anillo 3");
            loop {
                core::hint::spin_loop();
            }
        }
        if m.header.label == WAIT {
            if let Ok((a, _b)) = channel_create() {
                println!("dormilona: duermo en wait");
                let r = wait(&mut [wait_item(a, READABLE)], WAIT_FOREVER);
                println!("dormilona: ¡wait volvió! {:?}", r);
            }
        }
    }
    println!("dormilona: ¡no debería llegar acá!");
    1
}
