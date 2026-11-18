//! ABI de nsos: el contrato binario entre el kernel y las cells.
//!
//! Esta crate es `no_std`, no contiene `unsafe` y se compila igual para el
//! kernel (`x86_64-unknown-none`) y para el host, de modo que todas las reglas
//! de validación se prueban con `cargo test` y pueden fuzzearse.
//!
//! La especificación en prosa está en `docs/abi-ipc-capacidades.md`; si este
//! código y el documento divergen, es un bug de alguno de los dos.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod cap;
pub mod error;
pub mod msg;
pub mod syscall;

pub use cap::{CapHandle, ObjectType, Rights};
pub use error::{Error, SysResult};
pub use msg::{Message, MsgHeader};
pub use syscall::Syscall;

/// Versión del ABI. Se incrementa en cada cambio incompatible.
pub const ABI_VERSION: u32 = 1;
