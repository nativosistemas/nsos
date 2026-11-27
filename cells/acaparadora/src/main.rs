//! Cell `acaparadora`: carga para la prueba de las reservas de CPU.
//!
//! Sin reserva de CPU, gira sin ceder la CPU durante [`SPIN_NS`] y termina
//! con 0. El kernel arranca varias a la vez que `puntual`: entre todas
//! quieren la CPU entera, y aun así `puntual` tiene que recibir su reserva.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::clock_ns;

/// Cuánto gira (un poco más que la medición de `puntual`).
const SPIN_NS: u64 = 2_500_000_000;

nsos_rt::entry!(main);

fn main() -> i32 {
    let start = clock_ns();
    while clock_ns() - start < SPIN_NS {
        core::hint::spin_loop();
    }
    0
}
