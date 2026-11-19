//! Registros e instrucciones de la CPU ARM64 (EL1).

use core::arch::asm;

/// Lee un registro de sistema.
macro_rules! read_sysreg {
    ($r:literal) => {{
        let v: u64;
        // SAFETY: leer este registro de sistema en EL1 no tiene efectos.
        unsafe { core::arch::asm!(concat!("mrs {}, ", $r), out(reg) v, options(nomem, nostack, preserves_flags)) };
        v
    }};
}
pub(crate) use read_sysreg;

/// ¿Están habilitadas las interrupciones (`DAIF.I` = 0)?
pub fn interrupts_enabled() -> bool {
    read_sysreg!("daif") & (1 << 7) == 0
}

/// Habilita las interrupciones (IRQ).
pub fn enable_interrupts() {
    // SAFETY: solo cambia `DAIF.I`; los handlers ya están instalados.
    unsafe { asm!("msr daifclr, #2", options(nomem, nostack)) };
}

/// Deshabilita las interrupciones (IRQ).
pub fn disable_interrupts() {
    // SAFETY: solo cambia `DAIF.I`.
    unsafe { asm!("msr daifset, #2", options(nomem, nostack)) };
}

/// Ejecuta `f` con interrupciones deshabilitadas y restaura el estado.
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let was = interrupts_enabled();
    disable_interrupts();
    let r = f();
    if was {
        enable_interrupts();
    }
    r
}

/// Espera a la próxima interrupción.
pub fn wait_for_interrupt() {
    // SAFETY: `wfi` solo detiene el núcleo hasta una interrupción.
    unsafe { asm!("wfi", options(nomem, nostack)) };
}

/// Contador virtual del generic timer.
pub fn counter() -> u64 {
    // SAFETY: `isb` ordena la lectura del contador; no toca memoria.
    unsafe { asm!("isb", options(nomem, nostack)) };
    read_sysreg!("cntvct_el0")
}

/// Frecuencia del contador en Hz.
pub fn counter_hz() -> u64 {
    read_sysreg!("cntfrq_el0")
}
