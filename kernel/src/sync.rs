//! Primitivas de sincronización del kernel.

use crate::arch::cpu;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// Spinlock simple: no deshabilita interrupciones. Sirve solo para datos que
/// ningún handler de interrupción toca; si no, usar [`IrqSpinLock`].
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: el acceso a `value` está serializado por `locked` (acquire/release).
unsafe impl<T: Send> Sync for SpinLock<T> {}
// SAFETY: mover el lock mueve el `T`, que es `Send`.
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Crea el lock.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Adquiere el lock, girando hasta conseguirlo.
    pub fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        SpinGuard { lock: self }
    }

    /// Intenta adquirir el lock sin girar.
    pub fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| SpinGuard { lock: self })
    }
}

/// Guarda RAII del spinlock.
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: tenemos el lock.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: tenemos el lock en exclusiva.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

/// Spinlock que deshabilita interrupciones mientras está tomado.
///
/// Obligatorio para todo dato que también se toque desde un handler de
/// interrupción (scheduler, allocadores, consola): si un hilo tuviera el lock
/// y lo interrumpiera un handler que lo pide, el núcleo quedaría bloqueado.
pub struct IrqSpinLock<T> {
    inner: SpinLock<T>,
}

impl<T> IrqSpinLock<T> {
    /// Crea el lock.
    pub const fn new(value: T) -> Self {
        Self {
            inner: SpinLock::new(value),
        }
    }

    /// Adquiere el lock con interrupciones deshabilitadas.
    pub fn lock(&self) -> IrqSpinGuard<'_, T> {
        let was_enabled = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        IrqSpinGuard {
            guard: Some(self.inner.lock()),
            was_enabled,
        }
    }

    /// Intenta adquirir el lock sin girar.
    pub fn try_lock(&self) -> Option<IrqSpinGuard<'_, T>> {
        let was_enabled = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        match self.inner.try_lock() {
            Some(g) => Some(IrqSpinGuard {
                guard: Some(g),
                was_enabled,
            }),
            None => {
                if was_enabled {
                    cpu::enable_interrupts();
                }
                None
            }
        }
    }
}

/// Guarda del `IrqSpinLock`: al soltarse libera el lock y restaura IF.
pub struct IrqSpinGuard<'a, T> {
    guard: Option<SpinGuard<'a, T>>,
    was_enabled: bool,
}

impl<T> Deref for IrqSpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_ref().unwrap()
    }
}

impl<T> DerefMut for IrqSpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_mut().unwrap()
    }
}

impl<T> Drop for IrqSpinGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
        if self.was_enabled {
            cpu::enable_interrupts();
        }
    }
}
