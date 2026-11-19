//! Allocador de marcos físicos basado en bitmap.
//!
//! Un bit por marco de 4 KiB: `1` = ocupado, `0` = libre. Empieza con todo
//! ocupado y el kernel libera las regiones utilizables del mapa de memoria;
//! así, cualquier hueco que el mapa no mencione queda reservado por defecto.

/// Tamaño de un marco.
pub const FRAME_SIZE: u64 = 4096;

/// Errores del allocador.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Marco fuera del rango gestionado.
    OutOfRange,
    /// Se liberó un marco que ya estaba libre.
    DoubleFree,
}

/// Bitmap de marcos físicos sobre un almacenamiento prestado.
pub struct FrameBitmap<'a> {
    bits: &'a mut [u64],
    nframes: usize,
    free: usize,
    hint: usize,
}

impl<'a> FrameBitmap<'a> {
    /// Palabras de `u64` necesarias para `nframes` marcos.
    pub const fn words_for(nframes: usize) -> usize {
        nframes.div_ceil(64)
    }

    /// Crea el bitmap con todos los marcos ocupados. `bits` debe tener al
    /// menos `words_for(nframes)` palabras; si tiene menos, se recorta
    /// `nframes`.
    pub fn new(bits: &'a mut [u64], nframes: usize) -> Self {
        let nframes = nframes.min(bits.len() * 64);
        bits.fill(u64::MAX);
        Self {
            bits,
            nframes,
            free: 0,
            hint: 0,
        }
    }

    /// Marcos gestionados.
    pub fn total(&self) -> usize {
        self.nframes
    }

    /// Marcos libres.
    pub fn free_count(&self) -> usize {
        self.free
    }

    fn is_used(&self, f: usize) -> bool {
        self.bits[f / 64] & (1 << (f % 64)) != 0
    }

    fn set(&mut self, f: usize, used: bool) {
        let (w, b) = (f / 64, f % 64);
        if used {
            self.bits[w] |= 1 << b;
        } else {
            self.bits[w] &= !(1 << b);
        }
    }

    /// Marca como libres los marcos completamente contenidos en
    /// `[base, base+len)` (direcciones físicas en bytes).
    pub fn release_region(&mut self, base: u64, len: u64) {
        let start = base.div_ceil(FRAME_SIZE) as usize;
        let end = ((base.saturating_add(len)) / FRAME_SIZE) as usize;
        for f in start..end.min(self.nframes) {
            if self.is_used(f) {
                self.set(f, false);
                self.free += 1;
            }
        }
    }

    /// Marca como ocupados los marcos que tocan `[base, base+len)`.
    pub fn reserve_region(&mut self, base: u64, len: u64) {
        let start = (base / FRAME_SIZE) as usize;
        let end = base.saturating_add(len).div_ceil(FRAME_SIZE) as usize;
        for f in start..end.min(self.nframes) {
            if !self.is_used(f) {
                self.set(f, true);
                self.free -= 1;
            }
        }
    }

    /// Reserva un marco y devuelve su número.
    pub fn alloc(&mut self) -> Option<usize> {
        if self.free == 0 {
            return None;
        }
        let words = Self::words_for(self.nframes);
        for i in 0..words {
            let w = (self.hint / 64 + i) % words;
            if self.bits[w] != u64::MAX {
                let b = (!self.bits[w]).trailing_zeros() as usize;
                let f = w * 64 + b;
                if f >= self.nframes {
                    continue;
                }
                self.set(f, true);
                self.free -= 1;
                self.hint = f + 1;
                return Some(f);
            }
        }
        None
    }

    /// Reserva `n` marcos contiguos alineados a `align` marcos (potencia de 2).
    pub fn alloc_contiguous(&mut self, n: usize, align: usize) -> Option<usize> {
        if n == 0 || !align.is_power_of_two() || n > self.free {
            return None;
        }
        let mut f = 0;
        while f + n <= self.nframes {
            match (f..f + n).rev().find(|&g| self.is_used(g)) {
                Some(used) => f = (used + 1).next_multiple_of(align),
                None => {
                    for g in f..f + n {
                        self.set(g, true);
                    }
                    self.free -= n;
                    return Some(f);
                }
            }
        }
        None
    }

    /// Libera un marco.
    pub fn free_frame(&mut self, f: usize) -> Result<(), FrameError> {
        if f >= self.nframes {
            return Err(FrameError::OutOfRange);
        }
        if !self.is_used(f) {
            return Err(FrameError::DoubleFree);
        }
        self.set(f, false);
        self.free += 1;
        if f < self.hint {
            self.hint = f;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn make(nframes: usize) -> Vec<u64> {
        vec![0; FrameBitmap::words_for(nframes)]
    }

    #[test]
    fn starts_full() {
        let mut s = make(100);
        let mut b = FrameBitmap::new(&mut s, 100);
        assert_eq!(b.free_count(), 0);
        assert_eq!(b.alloc(), None);
    }

    #[test]
    fn release_rounds_inwards() {
        let mut s = make(16);
        let mut b = FrameBitmap::new(&mut s, 16);
        // [100, 3*4096+100) solo contiene completos los marcos 1 y 2
        b.release_region(100, 3 * FRAME_SIZE);
        assert_eq!(b.free_count(), 2);
        let a = b.alloc().unwrap();
        let c = b.alloc().unwrap();
        assert_eq!((a, c), (1, 2));
        assert_eq!(b.alloc(), None);
    }

    #[test]
    fn reserve_rounds_outwards() {
        let mut s = make(16);
        let mut b = FrameBitmap::new(&mut s, 16);
        b.release_region(0, 16 * FRAME_SIZE);
        b.reserve_region(FRAME_SIZE + 1, 10); // toca solo el marco 1
        assert_eq!(b.free_count(), 15);
        b.reserve_region(FRAME_SIZE * 3 - 1, 2); // toca 2 y 3
        assert_eq!(b.free_count(), 13);
    }

    #[test]
    fn alloc_free_cycle_and_double_free() {
        let mut s = make(130);
        let mut b = FrameBitmap::new(&mut s, 130);
        b.release_region(0, 130 * FRAME_SIZE);
        let mut got = Vec::new();
        while let Some(f) = b.alloc() {
            got.push(f);
        }
        assert_eq!(got.len(), 130);
        got.sort();
        got.dedup();
        assert_eq!(got.len(), 130);
        assert!(got.iter().all(|&f| f < 130));
        b.free_frame(64).unwrap();
        assert_eq!(b.free_frame(64), Err(FrameError::DoubleFree));
        assert_eq!(b.free_frame(130), Err(FrameError::OutOfRange));
        assert_eq!(b.alloc(), Some(64));
    }

    #[test]
    fn contiguous_respects_alignment_and_holes() {
        let mut s = make(64);
        let mut b = FrameBitmap::new(&mut s, 64);
        b.release_region(0, 64 * FRAME_SIZE);
        b.reserve_region(5 * FRAME_SIZE, FRAME_SIZE); // hueco en 5
        let f = b.alloc_contiguous(8, 8).unwrap();
        assert_eq!(f, 8);
        let g = b.alloc_contiguous(4, 1).unwrap();
        assert_eq!(g, 0);
        assert_eq!(b.free_count(), 64 - 1 - 8 - 4);
        assert_eq!(b.alloc_contiguous(100, 1), None);
        assert_eq!(b.alloc_contiguous(2, 3), None); // alineación inválida
    }

    #[test]
    fn storage_too_small_clamps() {
        let mut s = vec![0u64; 1];
        let b = FrameBitmap::new(&mut s, 1000);
        assert_eq!(b.total(), 64);
    }

    /// Modelo de referencia: secuencia pseudoaleatoria de operaciones
    /// comparada contra un `Vec<bool>`.
    #[test]
    fn matches_reference_model() {
        const N: usize = 300;
        let mut s = make(N);
        let mut b = FrameBitmap::new(&mut s, N);
        let mut model = vec![true; N];
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        for _ in 0..5000 {
            match rnd() % 4 {
                0 => {
                    let base = (rnd() % N) as u64 * FRAME_SIZE;
                    let len = (rnd() % 8) as u64 * FRAME_SIZE;
                    b.release_region(base, len);
                    let lo = ((base / FRAME_SIZE) as usize).min(N);
                    let hi = (((base + len) / FRAME_SIZE) as usize).min(N);
                    model[lo..hi].fill(false);
                }
                1 => {
                    if let Some(f) = b.alloc() {
                        assert!(!model[f]);
                        model[f] = true;
                    } else {
                        assert!(model.iter().all(|&u| u));
                    }
                }
                2 => {
                    let f = rnd() % N;
                    let r = b.free_frame(f);
                    if model[f] {
                        assert_eq!(r, Ok(()));
                        model[f] = false;
                    } else {
                        assert_eq!(r, Err(FrameError::DoubleFree));
                    }
                }
                _ => {
                    let n = 1 + rnd() % 4;
                    if let Some(f) = b.alloc_contiguous(n, 1) {
                        assert!(model[f..f + n].iter().all(|&u| !u));
                        model[f..f + n].fill(true);
                    }
                }
            }
            assert_eq!(b.free_count(), model.iter().filter(|&&u| !u).count());
        }
    }
}
