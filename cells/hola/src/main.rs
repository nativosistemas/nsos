//! Cell `hola`: cliente del driver `virtcon`.
//!
//! Capacidad de arranque 1: canal hacia el driver de consola virtio.
//! Envía algunas líneas con `WRITE`, verifica cada `WRITTEN` y termina con 0.
//! Al terminar, el kernel cierra su extremo y el driver también termina.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::Message;
use nsos_rt::{bootstrap, channel_recv, channel_send, println};

const WRITE: u64 = 1;
const WRITTEN: u64 = 2;

const LINES: [&[u8]; 3] = [
    b"hola desde una cell de nsos\n",
    b"este texto viaja por IPC hasta el driver virtio en user-space\n",
    b"chau\n",
];

nsos_rt::entry!(main);

fn main() -> i32 {
    let drv = bootstrap(1);
    let mut r = Message::default();
    for line in LINES {
        let m = Message::new(WRITE, line).unwrap();
        if channel_send(drv, &m, 0).is_err() {
            println!("hola: send falló");
            return 1;
        }
        if channel_recv(drv, &mut r, 0).is_err() {
            println!("hola: recv falló");
            return 2;
        }
        let ok = r.header.label == WRITTEN
            && r.payload().get(..4) == Some(&(line.len() as u32).to_le_bytes()[..]);
        if !ok {
            println!("hola: respuesta inesperada (etiqueta {})", r.header.label);
            return 3;
        }
    }
    println!("hola: {} líneas escritas por virtio-console", LINES.len());
    0
}
