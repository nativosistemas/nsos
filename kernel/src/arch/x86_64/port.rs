//! Acceso a puertos de E/S x86 (`in`/`out`).
//!
//! # Invariantes (`unsafe`)
//! Escribir en un puerto arbitrario puede reconfigurar hardware (PIC, PIT,
//! controladores DMA...). Por eso [`Port::new`] es `unsafe`: quien construye
//! el `Port` afirma que ese número de puerto corresponde al dispositivo que va
//! a manejar y que nadie más lo usa concurrentemente. Una vez construido, leer
//! y escribir es seguro.

use core::arch::asm;
use core::marker::PhantomData;

/// Ancho de acceso soportado.
pub trait PortWidth: Copy {
    /// # Safety
    /// Ver invariantes del módulo.
    unsafe fn read(port: u16) -> Self;
    /// # Safety
    /// Ver invariantes del módulo.
    unsafe fn write(port: u16, v: Self);
}

impl PortWidth for u8 {
    unsafe fn read(port: u16) -> u8 {
        let v: u8;
        // SAFETY: delegada al llamador (Port::new).
        unsafe {
            asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags))
        };
        v
    }
    unsafe fn write(port: u16, v: u8) {
        // SAFETY: delegada al llamador (Port::new).
        unsafe {
            asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags))
        };
    }
}

impl PortWidth for u16 {
    unsafe fn read(port: u16) -> u16 {
        let v: u16;
        // SAFETY: delegada al llamador (Port::new).
        unsafe {
            asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack, preserves_flags))
        };
        v
    }
    unsafe fn write(port: u16, v: u16) {
        // SAFETY: delegada al llamador (Port::new).
        unsafe {
            asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack, preserves_flags))
        };
    }
}

impl PortWidth for u32 {
    unsafe fn read(port: u16) -> u32 {
        let v: u32;
        // SAFETY: delegada al llamador (Port::new).
        unsafe {
            asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack, preserves_flags))
        };
        v
    }
    unsafe fn write(port: u16, v: u32) {
        // SAFETY: delegada al llamador (Port::new).
        unsafe {
            asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack, preserves_flags))
        };
    }
}

/// Un puerto de E/S con ancho fijo.
#[derive(Clone, Copy)]
pub struct Port<T: PortWidth> {
    port: u16,
    _w: PhantomData<T>,
}

impl<T: PortWidth> Port<T> {
    /// # Safety
    /// El llamador garantiza que `port` pertenece al dispositivo que va a
    /// manejar y que el acceso no rompe invariantes de otro código.
    pub const unsafe fn new(port: u16) -> Self {
        Self {
            port,
            _w: PhantomData,
        }
    }

    /// Lee el puerto.
    pub fn read(&self) -> T {
        // SAFETY: establecido en `new`.
        unsafe { T::read(self.port) }
    }

    /// Escribe el puerto.
    pub fn write(&self, v: T) {
        // SAFETY: establecido en `new`.
        unsafe { T::write(self.port, v) }
    }
}
