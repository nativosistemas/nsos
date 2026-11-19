//! Núcleo lógico del kernel de nsos.
//!
//! Aquí va toda la lógica del kernel que no necesita tocar hardware:
//! estructuras de datos de memoria, colas, tablas de capacidades... Se compila
//! sin `unsafe` y se prueba en el host (y con Miri). El kernel la usa pasándole
//! memoria ya validada.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod cspace;
pub mod elf;
pub mod frame_bitmap;
pub mod ipc_gc;
pub mod paging;

pub use cspace::CSpace;
pub use frame_bitmap::FrameBitmap;
