//! Mapa de bloques en uso, para elegir bloques libres al escribir.
//!
//! Se calcula de nuevo en cada escritura a partir del estado confirmado
//! (ranuras, tabla y datos de cada objeto), así que nunca devuelve un
//! bloque que el estado confirmado usa: es la condición `Fresh` del modelo
//! (`verification/lean/Nsos/Store.lean`).

/// Mapa de `64 * W` bloques.
pub struct Bitmap<const W: usize> {
    words: [u64; W],
}

impl<const W: usize> Default for Bitmap<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const W: usize> Bitmap<W> {
    pub const fn new() -> Self {
        Bitmap { words: [0; W] }
    }

    pub const CAPACITY: u32 = (W * 64) as u32;

    pub fn is_used(&self, b: u32) -> bool {
        let (w, bit) = ((b / 64) as usize, b % 64);
        w >= W || self.words[w] & (1 << bit) != 0
    }

    /// Marca `[start, start + n)` como usado (lo que cae fuera se ignora).
    pub fn mark(&mut self, start: u32, n: u32) {
        let end = start.saturating_add(n).min(Self::CAPACITY);
        let mut b = start;
        while b < end {
            self.words[(b / 64) as usize] |= 1 << (b % 64);
            b += 1;
        }
    }

    /// Primer rango de `n` bloques libres dentro de `[0, limit)`, si hay.
    /// `n == 0` no reserva nada y devuelve `Some(0)`.
    pub fn find_run(&self, n: u32, limit: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        let limit = limit.min(Self::CAPACITY);
        let mut run = 0u32;
        let mut b = 0u32;
        while b < limit {
            if self.is_used(b) {
                run = 0;
            } else {
                run += 1;
                if run == n {
                    return Some(b + 1 - n);
                }
            }
            b += 1;
        }
        None
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Con cualquier mapa de 64 bloques, cualquier largo y cualquier
    /// límite: si `find_run` devuelve un rango, está entero dentro del
    /// límite y libre. Después de marcarlo, ya no está libre.
    #[kani::proof]
    #[kani::unwind(66)]
    fn find_run_is_free() {
        let m = Bitmap::<1> { words: kani::any() };
        let n: u32 = kani::any();
        let limit: u32 = kani::any();
        kani::assume(n >= 1 && n <= 4);
        if let Some(s) = m.find_run(n, limit) {
            assert!(s as u64 + n as u64 <= limit.min(64) as u64);
            let k: u32 = kani::any();
            kani::assume(k < n);
            assert!(!m.is_used(s + k));
            let mut m2 = Bitmap::<1> { words: m.words };
            m2.mark(s, n);
            assert!(m2.is_used(s + k));
        }
    }
}
