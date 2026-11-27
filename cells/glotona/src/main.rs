//! Cell `glotona`: prueba negativa de las cuotas de memoria.
//!
//! Arranca sin capacidades y con una cuota chica (`GLOTONA_QUOTA` en
//! `kernel/src/main.rs`), y pide objetos `Memory` de 1 MiB (256 marcos, el máximo de
//! `mem_create`) hasta que el kernel le dice `NoMemory`. Termina con la
//! cantidad de objetos que consiguió: ktest comprueba que no se pasó de su
//! cuota. Sin cuotas, se quedaría con toda la memoria del sistema y las
//! demás cells no conseguirían la suya (`QuotaModel.no_starvation`).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::Error;
use nsos_rt::{mem_create, println};

/// Tamaño de cada pedido.
const SIZE: usize = 1 << 20;

nsos_rt::entry!(main);

fn main() -> i32 {
    let mut n = 0;
    loop {
        match mem_create(SIZE) {
            Ok(_) => n += 1,
            Err(Error::NoMemory) => break,
            Err(e) => {
                println!("glotona: error inesperado {:?}", e);
                return -1;
            }
        }
    }
    println!("glotona: {} objetos de 1 MiB antes de NoMemory", n);
    n
}
