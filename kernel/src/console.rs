//! Consola de depuración del kernel sobre el puerto serie.

use crate::arch::serial::Serial;
use crate::sync::IrqSpinLock;
use core::fmt::{self, Write};

static SERIAL: IrqSpinLock<Serial> = IrqSpinLock::new(Serial::console());

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
            let _ = Serial::console().write_fmt(args);
        }
    }
}

/// Texto que todavía no salió por el UART (ARM64: antes de mapearlo).
#[cfg(target_arch = "aarch64")]
pub fn with_early_text(f: impl FnOnce(&[u8])) {
    if let Some(s) = SERIAL.try_lock() {
        if let Some(t) = s.early_text() {
            f(t);
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
