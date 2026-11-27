//! Cell `puntual`: prueba de las reservas de CPU.
//!
//! El kernel la arranca con una reserva de CPU (`PUNTUAL_CPU` en
//! `kernel/src/main.rs`) junto con varias `acaparadora` que giran sin
//! reserva. Gira durante [`WINDOW_NS`] mirando el reloj: cada salto de más
//! de [`GAP_NS`] entre dos lecturas es tiempo en que no tuvo la CPU. Termina
//! con la parte del tiempo que corrió, en milésimas: ktest comprueba que es
//! al menos su reserva (`verification/lean/Nsos/Cpu.lean`, `reserved`). Sin
//! reservas, el scheduler round-robin le da una parte por cada cell que
//! quiere la CPU, mucho menos.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::{clock_ns, println};

/// Cuánto mide: dos períodos del scheduler.
const WINDOW_NS: u64 = 2_000_000_000;
/// Un salto mayor entre dos lecturas del reloj es tiempo sin la CPU.
const GAP_NS: u64 = 200_000;

nsos_rt::entry!(main);

fn main() -> i32 {
    let start = clock_ns();
    let mut last = start;
    let mut off = 0;
    loop {
        let now = clock_ns();
        if now - last > GAP_NS {
            off += now - last;
        }
        last = now;
        if now - start >= WINDOW_NS {
            break;
        }
    }
    let total = last - start;
    let ran = 1000 - off * 1000 / total;
    println!("puntual: corrió {} milésimas del tiempo", ran);
    ran as i32
}
