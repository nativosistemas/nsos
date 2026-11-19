//! Cell `fragil`: falla a propósito para ejercitar el supervisor.
//!
//! Capacidad de arranque 1: canal hacia `init`. Recibe `ATTEMPT` con el
//! número de intento (u32) y:
//! * intento 1: escribe en la dirección 0 → fallo de página; el kernel la
//!   termina (`Faulted`);
//! * intento 2: entra en pánico → el runtime termina con código 101;
//! * intento 3 o más: responde `READY` y termina con 0.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::Message;
use nsos_rt::{bootstrap, channel_recv, channel_send, println};

const ATTEMPT: u64 = 1;
const READY: u64 = 2;

nsos_rt::entry!(main);

fn main() -> i32 {
    let sup = bootstrap(1);
    let mut m = Message::default();
    if channel_recv(sup, &mut m, 0).is_err() || m.header.label != ATTEMPT {
        return 1;
    }
    let n = m
        .payload()
        .get(..4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(0);
    println!("fragil: intento {}", n);
    match n {
        1 => {
            let p = core::ptr::null_mut::<u64>();
            // Error deliberado: la página 0 nunca está mapeada.
            #[allow(unsafe_code)]
            // SAFETY: ninguna; se espera el #PF y que el kernel termine la cell.
            unsafe {
                p.write_volatile(1)
            };
            2
        }
        2 => panic!("falla a propósito en el intento {}", n),
        _ => {
            let r = Message::new(READY, b"listo").unwrap();
            if channel_send(sup, &r, 0).is_err() {
                return 3;
            }
            0
        }
    }
}
