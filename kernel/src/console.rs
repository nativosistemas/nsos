//! Consola de depuración del kernel sobre el puerto serie.
//!
//! Hasta que arranca el hilo de la consola ([`start`]), todo se escribe
//! directo en el UART. Después, todo pasa por un búfer circular ([`RING`]):
//! los mensajes del kernel y los de `debug_log` se copian enteros (nunca se
//! mezclan a mitad de línea) y el hilo `consola` los escribe en el UART de a
//! [`CHUNK`] bytes, con las interrupciones apagadas solo mientras escribe
//! cada pedazo. Así `debug_log` ya no espera al UART con las interrupciones
//! apagadas (`docs/amenazas.md`, punto 16): copia el texto y vuelve, y si el
//! búfer está lleno la cell espera dormida. Antes de apagar QEMU y en un
//! pánico, el búfer se vacía entero ([`flush`]).

use crate::arch::serial::Serial;
use crate::sched::{self, ThreadId};
use crate::sync::IrqSpinLock;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use nsos_kcore::ring::ByteRing;

static SERIAL: IrqSpinLock<Serial> = IrqSpinLock::new(Serial::console());

/// Tamaño del búfer de la consola.
const RING_SIZE: usize = 64 * 1024;

/// Bytes que escribe el hilo de la consola por vez (con las interrupciones
/// apagadas): unos 1,4 ms a 115200 baudios en hardware real.
const CHUNK: usize = 16;

struct Console {
    ring: ByteRing<RING_SIZE>,
    /// El hilo de la consola está dormido esperando texto.
    idle: bool,
    /// Cells esperando lugar en el búfer.
    waiters: Vec<ThreadId>,
}

static RING: IrqSpinLock<Console> = IrqSpinLock::new(Console {
    ring: ByteRing::new(),
    idle: false,
    waiters: Vec::new(),
});

/// Bytes escritos en el UART desde el arranque (para comprobar que
/// `debug_log` no escribe ninguno).
static UART_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes escritos en el UART desde el arranque.
pub fn uart_bytes() -> u64 {
    UART_BYTES.load(Ordering::Relaxed)
}

/// Escribe en el UART y lo cuenta.
fn uart_write(s: &mut Serial, bytes: &[u8]) {
    UART_BYTES.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    s.write_bytes(bytes);
}

/// El hilo de la consola ya corre: la salida pasa por el búfer.
static BUFFERED: AtomicBool = AtomicBool::new(false);
static DRAINER: AtomicU64 = AtomicU64::new(0);

/// Inicializa el UART.
pub fn init() {
    SERIAL.lock().init();
}

/// Arranca el hilo de la consola. Desde acá, la salida pasa por el búfer.
pub fn start() {
    let id = sched::spawn_daemon("consola", drain_forever);
    DRAINER.store(id, Ordering::Release);
    BUFFERED.store(true, Ordering::Release);
}

fn drain_forever() {
    let mut buf = [0u8; CHUNK];
    loop {
        let (n, waiters) = {
            let mut c = RING.lock();
            let n = c.ring.pop_into(&mut buf);
            if n == 0 {
                // Dormir hasta que llegue texto (`wake_drainer`, bajo este
                // mismo lock: no se pierde el aviso).
                c.idle = true;
                sched::block_current();
            }
            let waiters = if n > 0 {
                core::mem::take(&mut c.waiters)
            } else {
                Vec::new()
            };
            (n, waiters)
        };
        if n == 0 {
            crate::arch::yield_now();
            continue;
        }
        uart_write(&mut SERIAL.lock(), &buf[..n]);
        for t in waiters {
            sched::wake(t);
        }
    }
}

/// Con el lock del búfer tomado, después de guardar texto.
fn wake_drainer(c: &mut Console) {
    if c.idle {
        c.idle = false;
        sched::wake(DRAINER.load(Ordering::Acquire));
    }
}

/// Escribe todo lo que haya en el búfer directo en el UART.
fn drain_locked(c: &mut Console) {
    let mut buf = [0u8; CHUNK];
    let mut s = SERIAL.lock();
    loop {
        let n = c.ring.pop_into(&mut buf);
        if n == 0 {
            break;
        }
        uart_write(&mut s, &buf[..n]);
    }
}

/// Vacía el búfer en el UART (antes de apagar QEMU).
pub fn flush() {
    if !BUFFERED.load(Ordering::Acquire) {
        return;
    }
    // `try_lock`: si el pánico pasó con el búfer tomado, no esperar.
    if let Some(mut c) = RING.try_lock() {
        drain_locked(&mut c);
    }
}

/// Guarda una línea de una cell (`prefix` y `text`, enteros o nada). Si no
/// hay lugar, deja al hilo actual registrado y marcado bloqueado, y
/// devuelve `false`: quien llama cede la CPU y reintenta.
pub fn log_or_wait(parts: &[&[u8]]) -> bool {
    if !BUFFERED.load(Ordering::Acquire) {
        let mut s = SERIAL.lock();
        for p in parts {
            uart_write(&mut s, p);
        }
        return true;
    }
    let mut c = RING.lock();
    if c.ring.push_all(parts) {
        wake_drainer(&mut c);
        return true;
    }
    let me = sched::current();
    if !c.waiters.contains(&me) {
        c.waiters.push(me);
    }
    sched::block_current();
    false
}

/// Escribe en el búfer los bytes de un mensaje del kernel; si se llena, lo
/// vacía en el UART ahí mismo (el kernel no espera).
struct RingWriter<'a>(&'a mut Console);

impl fmt::Write for RingWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if !self.0.ring.push_byte(b) {
                drain_locked(self.0);
                self.0.ring.push_byte(b);
            }
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    if !BUFFERED.load(Ordering::Acquire) {
        let _ = SERIAL.lock().write_fmt(args);
        return;
    }
    let mut c = RING.lock();
    let _ = RingWriter(&mut c).write_fmt(args);
    wake_drainer(&mut c);
}

/// Escritura de emergencia para el panic handler: si el lock está tomado
/// (pánico mientras se imprimía) usa una instancia nueva del UART en vez de
/// quedar bloqueado para siempre.
pub fn _print_panic(args: fmt::Arguments) {
    // Lo que quedó en el búfer sale antes que el pánico.
    if let Some(mut c) = RING.try_lock() {
        if BUFFERED.load(Ordering::Acquire) {
            drain_locked(&mut c);
        }
    }
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
