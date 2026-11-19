//! Enlace autenticado entre dos máquinas (lo usa la cell `enlace`).
//!
//! Implementa el modelo `verification/lean/Nsos/Link.lean`:
//!
//! * Cada máquina tiene la misma clave compartida de 32 bytes (`psk`).
//! * Al empezar, cada punta manda `HELLO` con su rol y un nonce aleatorio de
//!   16 bytes (sin autenticar). Con los dos nonces, ambas derivan la clave de
//!   la sesión: `HMAC(psk, "nsos-enlace-v1" ‖ nonce_origen ‖ nonce_destino)`.
//!   Como entra el nonce fresco de *cada* punta, una sesión nueva nunca
//!   coincide con una vieja (en el modelo: la sesión solo crece).
//! * Cada trama de datos lleva dirección, secuencia y una etiqueta
//!   HMAC-SHA256 (truncada a 128 bits) sobre todo lo demás. El receptor la
//!   acepta solo si la etiqueta verifica (`Frame.auth`), con la clave de la
//!   sesión actual (`f.sess = s.sess`), en la dirección hacia él
//!   (`f.dir = d`) y con la secuencia exacta que espera
//!   (`f.seq = recv.length`). Es `Link.accepts`.
//!
//! Lo que no se modela ni se implementa: retransmisión (una trama perdida
//! detiene el enlace: negación de servicio) y confidencialidad (los datos
//! viajan en claro).
//!
//! Formato (dentro del payload Ethernet, `ETHERTYPE`):
//! ```text
//! HELLO: "NSL1" | 1 | rol (u8) | nonce (16)
//! DATA:  "NSL1" | 2 | dir (u8) | seq (u64 LE) | len (u16 LE) | datos | etiqueta (16)
//! ```

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

pub mod sha256;

use sha256::{ct_eq, hmac};

/// EtherType experimental local (IEEE 802 "Local Experimental Ethertype 1").
pub const ETHERTYPE: u16 = 0x88B5;
const MAGIC: [u8; 4] = *b"NSL1";
const KIND_HELLO: u8 = 1;
const KIND_DATA: u8 = 2;
/// Bytes de la etiqueta (HMAC-SHA256 truncado).
pub const TAG_LEN: usize = 16;
/// Bytes del nonce de cada punta.
pub const NONCE_LEN: usize = 16;
/// Cabecera de una trama de datos: magia, tipo, dirección, secuencia, largo.
pub const DATA_HEADER: usize = 4 + 1 + 1 + 8 + 2;
/// Bytes que agrega una trama de datos a su contenido.
pub const DATA_OVERHEAD: usize = DATA_HEADER + TAG_LEN;
/// Largo de un `HELLO`.
pub const HELLO_LEN: usize = 4 + 1 + 1 + NONCE_LEN;
const LABEL: &[u8] = b"nsos-enlace-v1";
/// Secuencia reservada para la confirmación de la sesión; `open` nunca la
/// acepta (haría falta mandar 2^64 - 1 mensajes antes).
pub const CONFIRM_SEQ: u64 = u64::MAX;

/// Qué punta es esta máquina. Fija la dirección de cada trama.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Origin = 0,
    Dest = 1,
}

impl Role {
    fn from_u8(v: u8) -> Option<Role> {
        match v {
            0 => Some(Role::Origin),
            1 => Some(Role::Dest),
            _ => None,
        }
    }
    /// La otra punta.
    pub fn peer(self) -> Role {
        match self {
            Role::Origin => Role::Dest,
            Role::Dest => Role::Origin,
        }
    }
    /// Dirección de las tramas que *manda* esta punta (`Link.Dir`):
    /// 0 = hacia el destino, 1 = hacia el origen.
    fn dir_out(self) -> u8 {
        match self {
            Role::Origin => 0,
            Role::Dest => 1,
        }
    }
}

/// Por qué se descartó una trama.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// No es una trama del enlace o está truncada.
    Malformed,
    /// La etiqueta no verifica (otra clave, otra sesión o contenido alterado).
    BadTag,
    /// Dirección equivocada (una trama propia reflejada).
    WrongDirection,
    /// Secuencia distinta de la esperada (repetida, vieja o adelantada).
    BadSequence,
    /// No entra en el búfer de salida.
    TooLong,
}

/// Escribe un `HELLO` en `out` y devuelve su largo.
pub fn hello(role: Role, nonce: &[u8; NONCE_LEN], out: &mut [u8]) -> Result<usize, LinkError> {
    let out = out.get_mut(..HELLO_LEN).ok_or(LinkError::TooLong)?;
    out[..4].copy_from_slice(&MAGIC);
    out[4] = KIND_HELLO;
    out[5] = role as u8;
    out[6..].copy_from_slice(nonce);
    Ok(HELLO_LEN)
}

/// Si `frame` es un `HELLO`, su rol y nonce.
pub fn parse_hello(frame: &[u8]) -> Option<(Role, [u8; NONCE_LEN])> {
    if frame.len() != HELLO_LEN || frame[..4] != MAGIC || frame[4] != KIND_HELLO {
        return None;
    }
    let role = Role::from_u8(frame[5])?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&frame[6..]);
    Some((role, nonce))
}

/// ¿Es una trama de datos (sin verificar nada más)?
pub fn is_data(frame: &[u8]) -> bool {
    frame.len() >= DATA_OVERHEAD && frame[..4] == MAGIC && frame[4] == KIND_DATA
}

/// Campos de una trama de datos, sin verificar la etiqueta.
#[derive(Debug, PartialEq, Eq)]
pub struct Parsed<'a> {
    pub dir: u8,
    pub seq: u64,
    pub data: &'a [u8],
    /// Lo que cubre la etiqueta (cabecera y datos).
    pub covered: &'a [u8],
    pub tag: &'a [u8],
}

/// Separa una trama de datos en sus campos. Rechaza cualquier largo que no
/// cierre exacto.
pub fn parse_data(frame: &[u8]) -> Result<Parsed<'_>, LinkError> {
    if !is_data(frame) {
        return Err(LinkError::Malformed);
    }
    let dir = frame[5];
    let seq = u64::from_le_bytes(frame[6..14].try_into().map_err(|_| LinkError::Malformed)?);
    let len = u16::from_le_bytes([frame[14], frame[15]]) as usize;
    if frame.len() != DATA_OVERHEAD + len {
        return Err(LinkError::Malformed);
    }
    let end = DATA_HEADER + len;
    Ok(Parsed {
        dir,
        seq,
        data: &frame[DATA_HEADER..end],
        covered: &frame[..end],
        tag: &frame[end..],
    })
}

/// Una sesión establecida.
pub struct Session {
    key: [u8; 32],
    role: Role,
    next_send: u64,
    next_recv: u64,
}

impl Session {
    /// Deriva la sesión de la clave compartida y los nonces de las dos
    /// puntas (cada una pone el suyo y el que recibió en el `HELLO`).
    pub fn new(
        psk: &[u8; 32],
        role: Role,
        origin_nonce: &[u8; NONCE_LEN],
        dest_nonce: &[u8; NONCE_LEN],
    ) -> Session {
        Session {
            key: hmac(psk, &[LABEL, origin_nonce, dest_nonce]),
            role,
            next_send: 0,
            next_recv: 0,
        }
    }

    /// Tramas aceptadas hasta ahora (`recv.length` en el modelo).
    pub fn received(&self) -> u64 {
        self.next_recv
    }

    /// Tramas enviadas hasta ahora (`sent.length` en el modelo).
    pub fn sent(&self) -> u64 {
        self.next_send
    }

    fn tag(&self, covered: &[u8]) -> [u8; 32] {
        hmac(&self.key, &[covered])
    }

    /// Arma una trama con `data` (`Link.Step.send`) y devuelve su largo.
    pub fn seal(&mut self, data: &[u8], out: &mut [u8]) -> Result<usize, LinkError> {
        if self.next_send == CONFIRM_SEQ {
            return Err(LinkError::TooLong);
        }
        let total = DATA_OVERHEAD
            .checked_add(data.len())
            .filter(|_| data.len() <= u16::MAX as usize)
            .ok_or(LinkError::TooLong)?;
        let out = out.get_mut(..total).ok_or(LinkError::TooLong)?;
        out[..4].copy_from_slice(&MAGIC);
        out[4] = KIND_DATA;
        out[5] = self.role.dir_out();
        out[6..14].copy_from_slice(&self.next_send.to_le_bytes());
        out[14..16].copy_from_slice(&(data.len() as u16).to_le_bytes());
        let end = DATA_HEADER + data.len();
        out[DATA_HEADER..end].copy_from_slice(data);
        let tag = self.tag(&out[..end]);
        out[end..].copy_from_slice(&tag[..TAG_LEN]);
        self.next_send += 1;
        Ok(total)
    }

    /// Arma la confirmación de la sesión: una trama de datos vacía con la
    /// secuencia reservada `CONFIRM_SEQ`. Demuestra a la otra punta que
    /// derivamos la misma clave. No consume secuencia y repetirla no
    /// entrega nada (no es un mensaje del modelo, solo establece la sesión).
    pub fn confirm(&self, out: &mut [u8]) -> Result<usize, LinkError> {
        let out = out.get_mut(..DATA_OVERHEAD).ok_or(LinkError::TooLong)?;
        out[..4].copy_from_slice(&MAGIC);
        out[4] = KIND_DATA;
        out[5] = self.role.dir_out();
        out[6..14].copy_from_slice(&CONFIRM_SEQ.to_le_bytes());
        out[14..16].copy_from_slice(&0u16.to_le_bytes());
        let tag = self.tag(&out[..DATA_HEADER]);
        out[DATA_HEADER..].copy_from_slice(&tag[..TAG_LEN]);
        Ok(DATA_OVERHEAD)
    }

    /// ¿Es una confirmación válida de la otra punta para esta sesión?
    pub fn is_confirm(&self, frame: &[u8]) -> bool {
        match parse_data(frame) {
            Ok(p) => {
                p.seq == CONFIRM_SEQ
                    && p.data.is_empty()
                    && p.dir == self.role.peer().dir_out()
                    && ct_eq(&self.tag(p.covered)[..TAG_LEN], p.tag)
            }
            Err(_) => false,
        }
    }

    /// Verifica una trama (`Link.accepts`) y, si la acepta, devuelve sus
    /// datos y avanza la secuencia (`Link.Step.deliver`). Si la rechaza, el
    /// estado no cambia.
    pub fn open<'a>(&mut self, frame: &'a [u8]) -> Result<&'a [u8], LinkError> {
        let p = parse_data(frame)?;
        if !ct_eq(&self.tag(p.covered)[..TAG_LEN], p.tag) {
            return Err(LinkError::BadTag);
        }
        if p.dir != self.role.peer().dir_out() {
            return Err(LinkError::WrongDirection);
        }
        if p.seq != self.next_recv || p.seq == CONFIRM_SEQ {
            return Err(LinkError::BadSequence);
        }
        self.next_recv += 1;
        Ok(p.data)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Con cualquier trama de hasta 40 bytes, `parse_data` no entra en
    /// pánico y, si acepta, los campos cubren la trama exactamente.
    #[kani::proof]
    #[kani::unwind(42)]
    fn parse_data_any() {
        let buf: [u8; 40] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= 40);
        if let Ok(p) = parse_data(&buf[..n]) {
            assert!(p.covered.len() + p.tag.len() == n);
            assert!(p.tag.len() == TAG_LEN);
            assert!(p.covered.len() == DATA_HEADER + p.data.len());
        }
    }

    /// Con cualquier entrada de hasta 24 bytes, `parse_hello` no entra en
    /// pánico y solo acepta el largo exacto.
    #[kani::proof]
    #[kani::unwind(26)]
    fn parse_hello_any() {
        let buf: [u8; 24] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= 24);
        if parse_hello(&buf[..n]).is_some() {
            assert!(n == HELLO_LEN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    const PSK: [u8; 32] = [7; 32];
    const NO: [u8; 16] = [1; 16];
    const ND: [u8; 16] = [2; 16];

    fn pair() -> (Session, Session) {
        (
            Session::new(&PSK, Role::Origin, &NO, &ND),
            Session::new(&PSK, Role::Dest, &NO, &ND),
        )
    }

    fn seal(s: &mut Session, data: &[u8]) -> Vec<u8> {
        let mut buf = [0u8; 256];
        let n = s.seal(data, &mut buf).unwrap();
        buf[..n].to_vec()
    }

    #[test]
    fn roundtrip_both_directions() {
        let (mut a, mut b) = pair();
        for i in 0..5u8 {
            let f = seal(&mut a, &[i; 3]);
            assert_eq!(b.open(&f).unwrap(), &[i; 3]);
            let g = seal(&mut b, &[i]);
            assert_eq!(a.open(&g).unwrap(), &[i]);
        }
        assert_eq!(
            (a.sent(), a.received(), b.sent(), b.received()),
            (5, 5, 5, 5)
        );
    }

    #[test]
    fn any_bit_flip_is_rejected() {
        let (mut a, b) = pair();
        let f = seal(&mut a, b"migrar");
        for i in 0..f.len() {
            for bit in 0..8 {
                let mut g = f.clone();
                g[i] ^= 1 << bit;
                let mut b2 = Session { ..b };
                assert!(b2.open(&g).is_err(), "byte {} bit {}", i, bit);
                assert_eq!(b2.received(), 0, "un rechazo no cambia el estado");
            }
        }
    }

    #[test]
    fn replay_is_rejected() {
        let (mut a, mut b) = pair();
        let f = seal(&mut a, b"uno");
        assert!(b.open(&f).is_ok());
        assert_eq!(b.open(&f), Err(LinkError::BadSequence));
        let g = seal(&mut a, b"dos");
        assert!(b.open(&g).is_ok());
        assert_eq!(b.open(&f), Err(LinkError::BadSequence));
    }

    #[test]
    fn reorder_is_rejected() {
        let (mut a, mut b) = pair();
        let f1 = seal(&mut a, b"1");
        let f2 = seal(&mut a, b"2");
        assert_eq!(b.open(&f2), Err(LinkError::BadSequence));
        assert!(b.open(&f1).is_ok());
        assert!(b.open(&f2).is_ok());
    }

    #[test]
    fn reflection_is_rejected() {
        let (mut a, _) = pair();
        let mut a2 = Session::new(&PSK, Role::Origin, &NO, &ND);
        let f = seal(&mut a, b"eco");
        assert_eq!(a2.open(&f), Err(LinkError::WrongDirection));
    }

    #[test]
    fn other_session_or_key_is_rejected() {
        let (mut a, _) = pair();
        let f = seal(&mut a, b"vieja");
        // Sesión nueva: la otra punta trajo un nonce fresco.
        let mut fresh = Session::new(&PSK, Role::Dest, &NO, &[3; 16]);
        assert_eq!(fresh.open(&f), Err(LinkError::BadTag));
        let mut wrong_key = Session::new(&[8; 32], Role::Dest, &NO, &ND);
        assert_eq!(wrong_key.open(&f), Err(LinkError::BadTag));
    }

    #[test]
    fn hello_roundtrip_and_garbage() {
        let mut buf = [0u8; 64];
        let n = hello(Role::Dest, &ND, &mut buf).unwrap();
        assert_eq!(parse_hello(&buf[..n]), Some((Role::Dest, ND)));
        assert_eq!(parse_hello(&buf[..n - 1]), None);
        buf[5] = 9;
        assert_eq!(parse_hello(&buf[..n]), None);
        assert!(parse_data(&buf[..n]).is_err());
    }

    #[test]
    fn truncated_and_padded_are_malformed() {
        let (mut a, mut b) = pair();
        let mut f = seal(&mut a, b"abc");
        assert_eq!(b.open(&f[..f.len() - 1]), Err(LinkError::Malformed));
        f.push(0);
        assert_eq!(b.open(&f), Err(LinkError::Malformed));
    }

    #[test]
    fn confirm_is_session_bound_and_not_a_message() {
        let (a, mut b) = pair();
        let mut buf = [0u8; 64];
        let n = a.confirm(&mut buf).unwrap();
        assert!(b.is_confirm(&buf[..n]));
        // No es un mensaje: `open` no lo entrega y no cambia el estado.
        assert_eq!(b.open(&buf[..n]), Err(LinkError::BadSequence));
        assert_eq!(b.received(), 0);
        // No sirve en otra sesión, ni reflejado, ni alterado.
        let other = Session::new(&PSK, Role::Dest, &NO, &[9; 16]);
        assert!(!other.is_confirm(&buf[..n]));
        let a2 = Session::new(&PSK, Role::Origin, &NO, &ND);
        assert!(!a2.is_confirm(&buf[..n]));
        buf[0] ^= 1;
        assert!(!b.is_confirm(&buf[..n]));
    }

    #[test]
    fn too_long_does_not_advance() {
        let (mut a, _) = pair();
        let mut small = [0u8; 20];
        assert_eq!(a.seal(b"0123456789", &mut small), Err(LinkError::TooLong));
        assert_eq!(a.sent(), 0);
    }
}
