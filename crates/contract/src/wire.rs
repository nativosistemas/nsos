//! Codificación de los campos de un mensaje sobre [`Message`].
//!
//! Los campos de datos van en orden, en little-endian y sin relleno, en
//! `data`; las capacidades, en orden, en `caps`. [`Bytes`] toma el resto de
//! los datos, así que va al final (como `ECHO`, que es solo el texto). Al
//! decodificar se exige que se consuman **todos** los datos y todas las
//! capacidades: un mensaje con algo de más no es del contrato.

use nsos_abi::msg::{MAX_CAPS, MAX_DATA};
use nsos_abi::{CapHandle, Message};

use crate::chan::{is_channel, Endpoint, State, Transport};

/// Escribe campos en un mensaje.
pub struct Writer<'a> {
    m: &'a mut Message,
    off: usize,
}

impl<'a> Writer<'a> {
    /// Empieza a escribir un mensaje vacío con esta etiqueta.
    pub fn new(m: &'a mut Message, label: u64) -> Writer<'a> {
        *m = Message::default();
        m.header.label = label;
        Writer { m, off: 0 }
    }

    /// Agrega bytes. La macro comprueba al compilar que los campos entran,
    /// así que no se trunca nunca (Kani: `roundtrip_echo`).
    pub fn bytes(&mut self, b: &[u8]) {
        let end = (self.off + b.len()).min(MAX_DATA);
        let n = end - self.off;
        self.m.data[self.off..end].copy_from_slice(&b[..n]);
        self.off = end;
        self.m.header.data_len = end as u16;
    }

    /// Adjunta una capacidad.
    pub fn cap(&mut self, h: CapHandle) {
        let ok = self.m.push_cap(h);
        debug_assert!(ok.is_ok(), "la macro limita las capacidades a MAX_CAPS");
    }
}

/// Lee campos de un mensaje recibido.
pub struct Reader<'a> {
    m: &'a Message,
    off: usize,
    caps: usize,
}

impl<'a> Reader<'a> {
    /// Empieza a leer `m`.
    pub fn new(m: &'a Message) -> Reader<'a> {
        Reader { m, off: 0, caps: 0 }
    }

    /// Los próximos `n` bytes, si están.
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let p = self.m.payload();
        let b = p.get(self.off..self.off.checked_add(n)?)?;
        self.off += n;
        Some(b)
    }

    /// Todos los bytes que quedan.
    pub fn rest(&mut self) -> &'a [u8] {
        let p = self.m.payload();
        let b = p.get(self.off..).unwrap_or(&[]);
        self.off = p.len();
        b
    }

    /// La próxima capacidad, si está.
    pub fn cap(&mut self) -> Option<CapHandle> {
        let h = *self.m.cap_slice().get(self.caps)?;
        self.caps += 1;
        Some(h)
    }

    /// `true` si se consumió todo: datos y capacidades.
    pub fn finish(&self) -> bool {
        self.off == self.m.payload().len() && self.caps == self.m.cap_slice().len()
    }
}

/// Un campo de un mensaje.
pub trait Field: Sized {
    /// Bytes de datos que ocupa como máximo.
    const DATA: usize;
    /// Capacidades que lleva.
    const CAPS: usize;
    /// Lo escribe.
    fn put(self, w: &mut Writer<'_>);
    /// Lo lee; `None` si no tiene la forma esperada.
    fn take<T: Transport>(r: &mut Reader<'_>, t: T) -> Option<Self>;
}

/// Un mensaje de un contrato (lo genera [`contract!`](crate::contract)).
pub trait Msg: Sized {
    /// Etiqueta (`header.label`).
    const LABEL: u64;
    /// Lo escribe en `m` (que se pisa entero).
    fn encode(self, m: &mut Message);
    /// Lo lee de `m`: `None` si la etiqueta no es la suya, falta algo o
    /// sobra algo.
    fn decode<T: Transport>(t: T, m: &Message) -> Option<Self>;
}

macro_rules! int_field {
    ($($t:ty),*) => {$(
        impl Field for $t {
            const DATA: usize = core::mem::size_of::<$t>();
            const CAPS: usize = 0;
            fn put(self, w: &mut Writer<'_>) {
                w.bytes(&self.to_le_bytes());
            }
            fn take<T: Transport>(r: &mut Reader<'_>, _: T) -> Option<Self> {
                let b = r.bytes(core::mem::size_of::<$t>())?;
                Some(<$t>::from_le_bytes(b.try_into().ok()?))
            }
        }
    )*};
}
int_field!(u8, u16, u32, u64);

impl Field for bool {
    const DATA: usize = 1;
    const CAPS: usize = 0;
    fn put(self, w: &mut Writer<'_>) {
        w.bytes(&[self as u8]);
    }
    fn take<T: Transport>(r: &mut Reader<'_>, _: T) -> Option<Self> {
        match r.bytes(1)? {
            [0] => Some(false),
            [1] => Some(true),
            _ => None,
        }
    }
}

impl<const N: usize> Field for [u8; N] {
    const DATA: usize = N;
    const CAPS: usize = 0;
    fn put(self, w: &mut Writer<'_>) {
        w.bytes(&self);
    }
    fn take<T: Transport>(r: &mut Reader<'_>, _: T) -> Option<Self> {
        r.bytes(N)?.try_into().ok()
    }
}

/// Hasta `N` bytes de largo variable. Toma el resto de los datos del
/// mensaje, así que tiene que ser el último campo de datos.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Bytes<const N: usize> {
    len: usize,
    buf: [u8; N],
}

impl<const N: usize> Bytes<N> {
    /// Copia `b`; `None` si es más largo que `N`.
    pub fn new(b: &[u8]) -> Option<Bytes<N>> {
        if b.len() > N {
            return None;
        }
        let mut buf = [0; N];
        buf[..b.len()].copy_from_slice(b);
        Some(Bytes { len: b.len(), buf })
    }

    /// Los bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len.min(N)]
    }
}

impl<const N: usize> core::fmt::Debug for Bytes<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Bytes({:?})", self.as_slice())
    }
}

impl<const N: usize> Field for Bytes<N> {
    const DATA: usize = N;
    const CAPS: usize = 0;
    fn put(self, w: &mut Writer<'_>) {
        w.bytes(self.as_slice());
    }
    fn take<T: Transport>(r: &mut Reader<'_>, _: T) -> Option<Self> {
        Bytes::new(r.rest())
    }
}

/// Un extremo de canal que viaja en el mensaje (se *mueve*: el emisor lo
/// pierde). Al recibirlo se comprueba que sea un canal con `SEND` y `RECV`
/// creado para el contrato de `S`.
impl<S: State> Field for Endpoint<S> {
    const DATA: usize = 0;
    const CAPS: usize = 1;
    fn put(self, w: &mut Writer<'_>) {
        w.cap(self.into_raw());
    }
    fn take<T: Transport>(r: &mut Reader<'_>, t: T) -> Option<Self> {
        let h = r.cap()?;
        is_channel(t, h, S::CONTRACT).then(|| Endpoint::from_raw(h))
    }
}

/// Comprobación de tamaño que hace la macro para cada mensaje.
#[doc(hidden)]
pub const fn fits(data: usize, caps: usize) -> bool {
    data <= MAX_DATA && caps <= MAX_CAPS
}
