//! Cell `parlanchina`: prueba de la latencia de `debug_log`.
//!
//! Escribe varias líneas del tamaño máximo de `debug_log` (1024 bytes). El
//! kernel mide cuánto tarda en guardar cada una y ktest comprueba que es
//! poco: ya no espera al UART (`docs/amenazas.md`, punto 16). Antes, cada
//! una de estas líneas tenía al kernel con las interrupciones apagadas
//! mientras salía byte por byte por el UART.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::debug_log;

/// Cuántas líneas escribe.
const LINES: usize = 8;

nsos_rt::entry!(main);

fn main() -> i32 {
    let mut line = [b'.'; 1024];
    let head = b"parlanchina: ";
    line[..head.len()].copy_from_slice(head);
    line[1023] = b'\n';
    let Ok(text) = core::str::from_utf8(&line) else {
        return -1;
    };
    for _ in 0..LINES {
        debug_log(text);
    }
    0
}
