//! Protocolo entre la cell `almacen` y sus clientes (etiquetas y códigos).
//!
//! Por una **raíz** (un canal por dueño que reparte el supervisor):
//! * `OPEN [flags u8 | nombre]` → `OPENED [versión u64 | largo u32 | escribe u8]`
//!   con un **handle** adjunto: un extremo de canal que da acceso a ese
//!   objeto y a nada más. `flags & OPEN_WRITE` pide escritura. El objeto no
//!   necesita existir (versión 0): se crea al escribirlo.
//!
//! Por un handle:
//! * `STAT` → `INFO [versión u64 | largo u32 | escribe u8]`
//! * `READ` con un `Memory` escribible adjunto → `DATA [versión u64 | largo u32]`
//!   y el `Memory` de vuelta, con los datos al principio.
//! * `WRITE [versión esperada u64 | largo u32]` con un `Memory` adjunto (los
//!   datos) → `WRITTEN [versión u64]` y el `Memory` de vuelta. La versión
//!   esperada [`ANY_VERSION`] escribe sin condición; otra exige que la actual
//!   sea esa (0 = que no exista).
//! * `DUP_RO` → `OPENED [...]` con un handle nuevo de **solo lectura** al
//!   mismo objeto, para dárselo a otra cell.
//!
//! Cualquier error → `ERR [código u32 | versión actual u64]` (y el `Memory`
//! de vuelta si vino uno).

pub const OPEN: u64 = 10;
pub const OPENED: u64 = 11;
pub const STAT: u64 = 12;
pub const INFO: u64 = 13;
pub const READ: u64 = 14;
pub const DATA: u64 = 15;
pub const WRITE: u64 = 16;
pub const WRITTEN: u64 = 17;
pub const DUP_RO: u64 = 18;
pub const ERR: u64 = 99;

pub const OPEN_WRITE: u8 = 1;
pub const ANY_VERSION: u64 = u64::MAX;

pub const E_NOT_FOUND: u32 = 1;
/// El handle es de solo lectura.
pub const E_READ_ONLY: u32 = 2;
pub const E_VERSION: u32 = 3;
pub const E_TOO_BIG: u32 = 4;
pub const E_NO_SPACE: u32 = 5;
pub const E_TOO_MANY: u32 = 6;
pub const E_DEVICE: u32 = 7;
pub const E_BAD_REQUEST: u32 = 8;
pub const E_NO_HANDLES: u32 = 9;

/// Código de error del protocolo para un error del almacén.
pub fn code(e: crate::Error) -> u32 {
    use crate::Error::*;
    match e {
        Dev | Foreign | Unformatted => E_DEVICE,
        NoSpace => E_NO_SPACE,
        TooBig => E_TOO_BIG,
        TooManyObjects => E_TOO_MANY,
        NotFound => E_NOT_FOUND,
        VersionMismatch(_) => E_VERSION,
    }
}

/// Lee un `u64` LE en `off` (0 si no alcanza).
pub fn u64_at(p: &[u8], off: usize) -> u64 {
    let mut v = [0u8; 8];
    if let Some(s) = p.get(off..off + 8) {
        v.copy_from_slice(s);
    }
    u64::from_le_bytes(v)
}

/// Lee un `u32` LE en `off` (0 si no alcanza).
pub fn u32_at(p: &[u8], off: usize) -> u32 {
    let mut v = [0u8; 4];
    if let Some(s) = p.get(off..off + 4) {
        v.copy_from_slice(s);
    }
    u32::from_le_bytes(v)
}

/// `[versión u64 | largo u32 | escribe u8]`.
pub fn info(version: u64, len: u32, write: bool) -> [u8; 13] {
    let mut p = [0u8; 13];
    p[..8].copy_from_slice(&version.to_le_bytes());
    p[8..12].copy_from_slice(&len.to_le_bytes());
    p[12] = write as u8;
    p
}

/// Contenido de prueba de las cells `escritor` y `curioso`: la palabra `k`
/// (de 8 bytes) de la versión `v` de un objeto. La palabra 0 es la versión
/// misma, así que una mezcla de dos versiones se nota en cualquier palabra.
pub fn demo_word(v: u64, k: usize) -> u64 {
    if k == 0 {
        v
    } else {
        v.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (k as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)
    }
}
