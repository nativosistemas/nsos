//! Consola de depuración del kernel sobre el puerto serie.

use crate::arch::serial::Serial;
use crate::sync::IrqSpinLock;
use core::fmt::{self, Write};

static SERIAL: IrqSpinLock<Serial> = IrqSpinLock::new(Serial::com1());

/// Inicializa el UART.
pub fn init() {
    SERIAL.lock().init();
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let _ = SERIAL.lock().write_fmt(args);
}

/// Escritura de emergencia para el panic handler: si el lock está tomado
/// (pánico mientras se imprimía) usa una instancia nueva del UART en vez de
/// quedar bloqueado para siempre.
pub fn _print_panic(args: fmt::Arguments) {
    match SERIAL.try_lock() {
        Some(mut s) => {
            let _ = s.write_fmt(args);
        }
        None => {
            let _ = Serial::com1().write_fmt(args);
        }
    }
}

/// `print!` del kernel.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => { $crate::console::_print(format_args!($($arg)*)) };
}

/// `println!` del kernel.
#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => { $crate::kprint!("{}\n", format_args!($($arg)*)) };
}
