//! Capa de arquitectura. Hoy solo x86_64; ARM64 vendrá después.

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;
