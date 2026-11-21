//! Cell `cliente`: usa el servidor `contador` mientras lo actualizan.
//!
//! Capacidad de arranque 1: canal con el servidor. Pide `INC` una y otra vez
//! y comprueba que el contador avanza de a uno, sin saltos ni retrocesos,
//! aunque en el medio haya un intento de actualización rechazado (rollback)
//! y uno aceptado. Termina con 0 cuando vio al menos `MIN_V1` respuestas de
//! la versión 1 (antes y después del rollback) y `MIN_V2` de la 2.
//!
//! Cada pedido es un `channel_call` (envío y respuesta en una syscall). El
//! pedido que queda encolado durante la actualización viaja con el extremo
//! del servidor, así que la respuesta llega igual al mismo `call`.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::WAIT_FOREVER;
use nsos_rt::abi::Message;
use nsos_rt::{bootstrap, channel_call, println};

const INC: u64 = 1;
const COUNT: u64 = 2;
const MIN_V1: u32 = 10;
const MIN_V2: u32 = 5;
const MAX_REQUESTS: u64 = 10_000;

nsos_rt::entry!(main);

fn main() -> i32 {
    let srv = bootstrap(1);
    let mut seen = [0u32; 3];
    let mut last_version = 0;
    for expected in 1..=MAX_REQUESTS {
        let mut m = Message::new(INC, b"").unwrap();
        if channel_call(srv, &mut m, WAIT_FOREVER).is_err() || m.header.label != COUNT {
            println!("cliente: se cortó la conexión en el pedido {}", expected);
            return 2;
        }
        let d = m.payload();
        let count = u64::from_le_bytes(d[..8].try_into().unwrap());
        let version = u32::from_le_bytes(d[8..12].try_into().unwrap());
        if count != expected || !(1..=2).contains(&version) || version < last_version {
            println!(
                "cliente: respuesta inválida: contador {} (esperaba {}), versión {}",
                count, expected, version
            );
            return 3;
        }
        if version != last_version {
            println!(
                "cliente: ahora me atiende la versión {} (contador {})",
                version, count
            );
            last_version = version;
        }
        seen[version as usize] += 1;
        if seen[1] >= MIN_V1 && seen[2] >= MIN_V2 {
            println!(
                "cliente: {} pedidos sin saltos ({} a v1, {} a v2)",
                count, seen[1], seen[2]
            );
            return 0;
        }
    }
    println!("cliente: nunca vi la versión 2");
    4
}
