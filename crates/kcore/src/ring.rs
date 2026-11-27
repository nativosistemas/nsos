//! Búfer circular de bytes de tamaño fijo (la consola del kernel).
//!
//! `debug_log` copia el texto de una cell acá y vuelve enseguida; un hilo
//! del kernel lo saca de a pedazos y lo escribe en el UART con las
//! interrupciones prendidas. Antes, `debug_log` escribía hasta 1024 bytes
//! esperando al UART con las interrupciones apagadas (unos 89 ms a 115200
//! baudios en hardware real).
//!
//! Lo que se verifica con Kani: [`ByteRing::push_all`] es todo o nada (un
//! mensaje nunca queda cortado ni mezclado con otro), y lo que sale con
//! [`ByteRing::pop_into`] es exactamente lo que entró, en el mismo orden.

/// Búfer circular de `N` bytes.
pub struct ByteRing<const N: usize> {
    buf: [u8; N],
    /// Posición del primer byte guardado.
    head: usize,
    /// Bytes guardados.
    len: usize,
}

impl<const N: usize> Default for ByteRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ByteRing<N> {
    /// Búfer vacío.
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            head: 0,
            len: 0,
        }
    }

    /// Bytes guardados.
    pub fn len(&self) -> usize {
        self.len
    }

    /// ¿Está vacío?
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Lugar libre.
    pub fn free(&self) -> usize {
        N - self.len
    }

    /// Guarda todos los bytes de las partes, en orden, o ninguno si no
    /// entran. Devuelve si los guardó.
    pub fn push_all(&mut self, parts: &[&[u8]]) -> bool {
        let need = parts
            .iter()
            .map(|p| p.len())
            .fold(0usize, |a, b| a.saturating_add(b));
        if need > self.free() {
            return false;
        }
        for p in parts {
            self.push_slice(p);
        }
        true
    }

    /// Copia `p` al final, en a lo sumo dos tramos. Quien llama ya comprobó
    /// que entra.
    fn push_slice(&mut self, p: &[u8]) {
        let tail = (self.head + self.len) % N;
        let first = p.len().min(N - tail);
        self.buf[tail..tail + first].copy_from_slice(&p[..first]);
        self.buf[..p.len() - first].copy_from_slice(&p[first..]);
        self.len += p.len();
    }

    /// Guarda un byte si hay lugar.
    pub fn push_byte(&mut self, b: u8) -> bool {
        if self.len == N {
            return false;
        }
        let tail = (self.head + self.len) % N;
        self.buf[tail] = b;
        self.len += 1;
        true
    }

    /// Saca hasta `out.len()` bytes, los más viejos primero. Devuelve
    /// cuántos sacó.
    pub fn pop_into(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len);
        let first = n.min(N - self.head);
        out[..first].copy_from_slice(&self.buf[self.head..self.head + first]);
        out[first..n].copy_from_slice(&self.buf[..n - first]);
        self.head = (self.head + n) % N;
        self.len -= n;
        n
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn fifo_and_all_or_nothing() {
        let mut r = ByteRing::<8>::new();
        assert!(r.push_all(&[b"[a] ", b"hi"]));
        assert!(!r.push_all(&[b"xyz"])); // no entra: no guarda nada
        assert_eq!(r.len(), 6);
        let mut out = [0u8; 4];
        assert_eq!(r.pop_into(&mut out), 4);
        assert_eq!(&out, b"[a] ");
        assert!(r.push_all(&[b"12345"])); // da la vuelta
        let mut rest = Vec::new();
        let mut b = [0u8; 3];
        loop {
            let n = r.pop_into(&mut b);
            if n == 0 {
                break;
            }
            rest.extend_from_slice(&b[..n]);
        }
        assert_eq!(&rest, b"hi12345");
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const N: usize = 4;

    /// Un búfer cualquiera que cumpla el invariante.
    fn any_ring() -> ByteRing<N> {
        let head: usize = kani::any();
        let len: usize = kani::any();
        kani::assume(head < N && len <= N);
        ByteRing {
            buf: kani::any(),
            head,
            len,
        }
    }

    /// El `i`-ésimo byte guardado.
    fn at(r: &ByteRing<N>, i: usize) -> u8 {
        r.buf[(r.head + i) % N]
    }

    /// `push_all` con dos partes cualesquiera: si entran, quedan al final,
    /// en orden, sin tocar lo que había; si no, nada cambia.
    #[kani::proof]
    #[kani::unwind(6)]
    fn push_all_any() {
        let mut r = any_ring();
        let before: [u8; N] = core::array::from_fn(|i| at(&r, i));
        let len0 = r.len;
        let a: [u8; 2] = kani::any();
        let b: [u8; 2] = kani::any();
        let (la, lb): (usize, usize) = (kani::any(), kani::any());
        kani::assume(la <= 2 && lb <= 2);
        let ok = r.push_all(&[&a[..la], &b[..lb]]);
        assert!(ok == (len0 + la + lb <= N));
        if ok {
            assert!(r.len == len0 + la + lb);
            for i in 0..len0 {
                assert!(at(&r, i) == before[i]);
            }
            for i in 0..la {
                assert!(at(&r, len0 + i) == a[i]);
            }
            for i in 0..lb {
                assert!(at(&r, len0 + la + i) == b[i]);
            }
        } else {
            assert!(r.len == len0);
            for i in 0..len0 {
                assert!(at(&r, i) == before[i]);
            }
        }
        assert!(r.head < N && r.len <= N);
    }

    /// `pop_into` saca los más viejos, en orden, y deja el resto igual.
    #[kani::proof]
    #[kani::unwind(6)]
    fn pop_into_any() {
        let mut r = any_ring();
        let before: [u8; N] = core::array::from_fn(|i| at(&r, i));
        let len0 = r.len;
        let mut out = [0u8; 3];
        let k: usize = kani::any();
        kani::assume(k <= 3);
        let n = r.pop_into(&mut out[..k]);
        assert!(n == k.min(len0) && r.len == len0 - n);
        for i in 0..n {
            assert!(out[i] == before[i]);
        }
        for i in 0..r.len {
            assert!(at(&r, i) == before[n + i]);
        }
        assert!(r.head < N && r.len <= N);
    }
}
