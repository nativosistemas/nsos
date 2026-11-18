//! Núcleo lógico del kernel de nsos.
//!
//! Aquí va toda la lógica del kernel que no necesita tocar hardware:
//! estructuras de datos de memoria, colas, tablas de capacidades... Se compila
//! sin `unsafe` y se prueba en el host (y con Miri). El kernel la usa pasándole
//! memoria ya validada.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod frame_bitmap;

pub use frame_bitmap::FrameBitmap;
