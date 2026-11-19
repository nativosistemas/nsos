//! Partes del despacho de interrupciones comunes a todas las arquitecturas.
//!
//! Cada arquitectura tiene su propio `arch::*::trap` que decodifica la
//! excepción (vector en x86_64, `ESR_EL1` en ARM64) y llama a las funciones
//! genéricas: syscalls, fallos de cells, scheduler. Antes de reanudar pasa
//! por [`finish`].

use crate::arch::TrapFrame;
use crate::sched;
use core::sync::atomic::{AtomicU64, Ordering};

static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);

/// Cantidad de breakpoints atendidos; lo usan los tests.
pub fn breakpoints() -> u64 {
    BREAKPOINTS.load(Ordering::Relaxed)
}

/// Cuenta un breakpoint del kernel (`int3` / `brk`).
pub fn note_breakpoint() {
    BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
}

/// Último paso antes de reanudar el frame `next`.
///
/// Si alguien pidió matar la cell del hilo elegido (`cell_kill`) y el frame
/// vuelve a la cell, no se vuelve: el frame se reescribe para que el hilo
/// continúe en modo kernel, en su propia pila de kernel, en
/// [`kill_trampoline`]. Así la cell siempre muere desde su propio hilo (como
/// con `exit`) y nunca a mitad de una syscall.
pub fn finish(next: *mut TrapFrame) -> *mut TrapFrame {
    // SAFETY: `next` es el frame guardado del hilo que se va a reanudar: lo
    // escribió su stub de entrada en su pila de kernel (o el scheduler al
    // crearlo) y nadie más lo toca hasta reanudarlo.
    let frame = unsafe { &mut *next };
    if frame.is_user_mode() && sched::current_cell().is_some_and(|c| c.kill_requested()) {
        frame.redirect_to_kernel(kill_trampoline as *const () as u64, next as u64);
    }
    next
}

/// Termina la cell del hilo actual por `cell_kill` y el hilo con ella.
extern "C" fn kill_trampoline() -> ! {
    if let Some(cell) = sched::current_cell() {
        crate::cell::terminate(&cell, crate::cell::ExitStatus::Killed);
    }
    sched::exit()
}
