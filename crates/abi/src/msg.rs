//! Formato de los mensajes IPC.
//!
//! Un mensaje ocupa exactamente [`MSG_SIZE`] bytes (256) con layout `repr(C)`
//! fijo. Se copia entero entre el espacio de la cell emisora y la cola del
//! canal; los datos grandes viajan como capacidades `Memory` transferidas
//! (zero-copy por transferencia de propiedad, ver la especificación).

use crate::cap::CapHandle;
use crate::error::Error;

/// Tamaño total de un mensaje en bytes.
pub const MSG_SIZE: usize = 256;
/// Tamaño de la cabecera.
pub const HEADER_SIZE: usize = 16;
/// Máximo de capacidades adjuntas por mensaje.
pub const MAX_CAPS: usize = 4;
/// Máximo de bytes de datos en línea.
pub const MAX_DATA: usize = MSG_SIZE - HEADER_SIZE - MAX_CAPS * 4;

/// Etiquetas reservadas para protocolos del sistema (rango alto).
pub mod labels {
    /// Inicio del rango reservado. Las cells de usuario usan etiquetas menores.
    pub const SYSTEM_BASE: u64 = 0xFFFF_0000_0000_0000;
    /// Supervisor → cell: preparar actualización en vivo (serializar estado).
    pub const UPGRADE_PREPARE: u64 = SYSTEM_BASE + 1;
    /// Cell vieja → supervisor: estado serializado (cap `Memory` adjunta).
    pub const UPGRADE_STATE: u64 = SYSTEM_BASE + 2;
    /// Supervisor → cell nueva: restaurar estado (cap `Memory` adjunta).
    pub const UPGRADE_RESTORE: u64 = SYSTEM_BASE + 3;
    /// Comprobación de vida.
    pub const PING: u64 = SYSTEM_BASE + 4;
    /// Respuesta a `PING`.
    pub const PONG: u64 = SYSTEM_BASE + 5;
}

/// Cabecera de mensaje (16 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct MsgHeader {
    /// Etiqueta definida por el protocolo (método, tipo de evento...).
    pub label: u64,
    /// Bytes válidos en `data` (`<= MAX_DATA`).
    pub data_len: u16,
    /// Capacidades válidas en `caps` (`<= MAX_CAPS`).
    pub cap_count: u8,
    /// Reservado, debe ser 0.
    pub flags: u8,
    /// Reservado, debe ser 0.
    pub reserved: u32,
}

/// Mensaje IPC completo (256 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, align(8))]
pub struct Message {
    /// Cabecera.
    pub header: MsgHeader,
    /// Capacidades transferidas. Al enviar son handles del emisor; al recibir
    /// son handles nuevos en el CSpace del receptor.
    pub caps: [CapHandle; MAX_CAPS],
    /// Datos en línea.
    pub data: [u8; MAX_DATA],
}

const _: () = assert!(core::mem::size_of::<MsgHeader>() == HEADER_SIZE);
const _: () = assert!(core::mem::size_of::<Message>() == MSG_SIZE);
const _: () = assert!(core::mem::align_of::<Message>() == 8);

impl Default for Message {
    fn default() -> Self {
        Message {
            header: MsgHeader::default(),
            caps: [CapHandle::INVALID; MAX_CAPS],
            data: [0; MAX_DATA],
        }
    }
}

impl Message {
    /// Crea un mensaje con etiqueta y datos. Falla si los datos no caben.
    pub fn new(label: u64, data: &[u8]) -> Result<Message, Error> {
        if data.len() > MAX_DATA {
            return Err(Error::BadMessage);
        }
        let mut m = Message::default();
        m.header.label = label;
        m.header.data_len = data.len() as u16;
        m.data[..data.len()].copy_from_slice(data);
        Ok(m)
    }

    /// Adjunta una capacidad. Falla si ya hay `MAX_CAPS` o el handle es nulo.
    pub fn push_cap(&mut self, h: CapHandle) -> Result<(), Error> {
        let n = self.header.cap_count as usize;
        if n >= MAX_CAPS || !h.is_valid() {
            return Err(Error::BadMessage);
        }
        self.caps[n] = h;
        self.header.cap_count += 1;
        Ok(())
    }

    /// Datos válidos.
    pub fn payload(&self) -> &[u8] {
        &self.data[..(self.header.data_len as usize).min(MAX_DATA)]
    }

    /// Capacidades válidas.
    pub fn cap_slice(&self) -> &[CapHandle] {
        &self.caps[..(self.header.cap_count as usize).min(MAX_CAPS)]
    }

    /// Reglas que el kernel aplica a todo mensaje entrante desde una cell.
    ///
    /// No comprueba que los handles existan (eso depende del CSpace), solo la
    /// forma: longitudes, campos reservados, handles no nulos y sin
    /// duplicados, y slots sobrantes en cero (no se filtra basura).
    pub fn validate(&self) -> Result<(), Error> {
        let h = &self.header;
        if h.data_len as usize > MAX_DATA
            || h.cap_count as usize > MAX_CAPS
            || h.flags != 0
            || h.reserved != 0
        {
            return Err(Error::BadMessage);
        }
        let n = h.cap_count as usize;
        for (i, c) in self.caps.iter().enumerate() {
            if i < n {
                if !c.is_valid() || self.caps[..i].contains(c) {
                    return Err(Error::BadMessage);
                }
            } else if *c != CapHandle::INVALID {
                return Err(Error::BadMessage);
            }
        }
        Ok(())
    }

    /// Serializa a bytes little-endian (sin `unsafe`).
    pub fn to_bytes(&self) -> [u8; MSG_SIZE] {
        let mut b = [0u8; MSG_SIZE];
        b[0..8].copy_from_slice(&self.header.label.to_le_bytes());
        b[8..10].copy_from_slice(&self.header.data_len.to_le_bytes());
        b[10] = self.header.cap_count;
        b[11] = self.header.flags;
        b[12..16].copy_from_slice(&self.header.reserved.to_le_bytes());
        for (i, c) in self.caps.iter().enumerate() {
            let o = HEADER_SIZE + i * 4;
            b[o..o + 4].copy_from_slice(&c.0.to_le_bytes());
        }
        b[HEADER_SIZE + MAX_CAPS * 4..].copy_from_slice(&self.data);
        b
    }

    /// Deserializa desde bytes y valida. Es la función que el kernel usa
    /// sobre memoria controlada por la cell, y el objetivo del fuzzing.
    pub fn from_bytes(b: &[u8; MSG_SIZE]) -> Result<Message, Error> {
        let mut m = Message::default();
        m.header.label = u64::from_le_bytes(b[0..8].try_into().unwrap());
        m.header.data_len = u16::from_le_bytes([b[8], b[9]]);
        m.header.cap_count = b[10];
        m.header.flags = b[11];
        m.header.reserved = u32::from_le_bytes(b[12..16].try_into().unwrap());
        for (i, cap) in m.caps.iter_mut().enumerate() {
            let o = HEADER_SIZE + i * 4;
            *cap = CapHandle(u32::from_le_bytes(b[o..o + 4].try_into().unwrap()));
        }
        m.data.copy_from_slice(&b[HEADER_SIZE + MAX_CAPS * 4..]);
        m.validate()?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(MAX_DATA, 224);
    }

    #[test]
    fn roundtrip() {
        let mut m = Message::new(7, b"hola cell").unwrap();
        m.push_cap(CapHandle::new(3, 1).unwrap()).unwrap();
        let b = m.to_bytes();
        let m2 = Message::from_bytes(&b).unwrap();
        assert_eq!(m, m2);
        assert_eq!(m2.payload(), b"hola cell");
        assert_eq!(m2.cap_slice().len(), 1);
    }

    #[test]
    fn too_much_data() {
        assert_eq!(Message::new(0, &[0; MAX_DATA + 1]), Err(Error::BadMessage));
        assert!(Message::new(0, &[0; MAX_DATA]).is_ok());
    }

    #[test]
    fn rejects_bad_headers() {
        let base = Message::new(1, b"x").unwrap();
        let mut m = base;
        m.header.data_len = MAX_DATA as u16 + 1;
        assert_eq!(m.validate(), Err(Error::BadMessage));
        let mut m = base;
        m.header.cap_count = 5;
        assert_eq!(m.validate(), Err(Error::BadMessage));
        let mut m = base;
        m.header.flags = 1;
        assert_eq!(m.validate(), Err(Error::BadMessage));
        let mut m = base;
        m.header.reserved = 1;
        assert_eq!(m.validate(), Err(Error::BadMessage));
    }

    #[test]
    fn rejects_bad_caps() {
        let mut m = Message::default();
        // cap_count = 1 pero handle nulo
        m.header.cap_count = 1;
        assert_eq!(m.validate(), Err(Error::BadMessage));
        // basura en slot no usado
        let mut m = Message::default();
        m.caps[2] = CapHandle(9);
        assert_eq!(m.validate(), Err(Error::BadMessage));
        // duplicado
        let mut m = Message::default();
        let h = CapHandle::new(4, 0).unwrap();
        m.push_cap(h).unwrap();
        m.caps[1] = h;
        m.header.cap_count = 2;
        assert_eq!(m.validate(), Err(Error::BadMessage));
        // demasiadas
        let mut m = Message::default();
        for s in 1..=4 {
            m.push_cap(CapHandle::new(s, 0).unwrap()).unwrap();
        }
        assert_eq!(
            m.push_cap(CapHandle::new(5, 0).unwrap()),
            Err(Error::BadMessage)
        );
    }

    /// Fuzz determinista barato: ninguna entrada hace entrar en pánico al
    /// decodificador, y todo lo aceptado vuelve a serializarse idéntico.
    #[test]
    fn pseudo_fuzz_from_bytes() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut accepted = 0;
        for _ in 0..20_000 {
            let mut b = [0u8; MSG_SIZE];
            for chunk in b.chunks_mut(8) {
                let v = next().to_le_bytes();
                chunk.copy_from_slice(&v[..chunk.len()]);
            }
            // Sesgar hacia cabeceras plausibles para cubrir más ramas.
            if next() % 2 == 0 {
                b[9] = 0;
                b[10] %= 6;
                b[11] = 0;
                b[12..16].fill(0);
                let n = b[10] as usize;
                for i in n..MAX_CAPS {
                    let o = HEADER_SIZE + i * 4;
                    b[o..o + 4].fill(0);
                }
            }
            if let Ok(m) = Message::from_bytes(&b) {
                accepted += 1;
                assert_eq!(m.to_bytes(), b);
            }
        }
        assert!(accepted > 0);
    }
}
