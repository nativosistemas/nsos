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

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Palabras del bitmap en las pruebas: hasta 128 marcos, así entran una
    /// palabra llena, una a medias y la vuelta del `hint` entre palabras.
    const W: usize = 2;

    /// Bits de la palabra `w` que corresponden a marcos `< n`.
    fn below(w: usize, n: usize) -> u64 {
        let lo = w * 64;
        if n >= lo + 64 {
            u64::MAX
        } else if n <= lo {
            0
        } else {
            (1u64 << (n - lo)) - 1
        }
    }

    /// Bits de la palabra `w` que corresponden a marcos en `[lo, hi)`.
    fn range(w: usize, lo: usize, hi: usize) -> u64 {
        below(w, hi) & !below(w, lo)
    }

    fn used_in(bits: &[u64], f: usize) -> bool {
        bits[f / 64] & (1 << (f % 64)) != 0
    }

    /// Marcos libres entre los primeros `nframes`, contados por palabra.
    fn free_in(bits: &[u64], nframes: usize) -> usize {
        (0..W)
            .map(|w| (!bits[w] & below(w, nframes)).count_ones() as usize)
            .sum()
    }

    /// Un bitmap cualquiera que cumpla el invariante: bits arbitrarios
    /// (también los que sobran después de `nframes`), `free` igual a la
    /// cantidad de marcos libres y `hint` cualquiera.
    fn any_state(bits: &mut [u64; W], max: usize) -> FrameBitmap<'_> {
        let nframes: usize = kani::any();
        kani::assume(nframes <= max);
        let free = free_in(bits, nframes);
        FrameBitmap {
            bits,
            nframes,
            free,
            hint: kani::any(),
        }
    }

    /// El invariante: `free` cuenta exactamente los marcos libres.
    fn invariant(b: &FrameBitmap<'_>) -> bool {
        b.nframes <= b.bits.len() * 64 && b.free == free_in(b.bits, b.nframes)
    }

    /// Ningún marco gestionado fuera de `[lo, hi)` cambió.
    fn same_outside(b: &FrameBitmap<'_>, before: &[u64; W], lo: usize, hi: usize) -> bool {
        (0..W).all(|w| (b.bits[w] ^ before[w]) & below(w, b.nframes) & !range(w, lo, hi) == 0)
    }

    /// Todos los marcos de `[lo, hi)` estaban libres antes y están ocupados
    /// ahora.
    fn taken(b: &FrameBitmap<'_>, before: &[u64; W], lo: usize, hi: usize) -> bool {
        (0..W).all(|w| {
            let r = range(w, lo, hi);
            before[w] & r == 0 && b.bits[w] & r == r
        })
    }

    /// `alloc` nunca entrega un marco ocupado: si devuelve `f`, `f` estaba
    /// libre, ahora está ocupado y ningún otro cambió. Si no devuelve nada,
    /// no había marcos libres. Así dos cells nunca reciben el mismo marco.
    #[kani::proof]
    #[kani::unwind(3)]
    fn alloc_any() {
        let before: [u64; W] = kani::any();
        let mut bits = before;
        let mut b = any_state(&mut bits, W * 64);
        let free0 = b.free;
        match b.alloc() {
            Some(f) => {
                assert!(f < b.nframes);
                assert!(taken(&b, &before, f, f + 1));
                assert!(same_outside(&b, &before, f, f + 1));
                assert!(b.free == free0 - 1);
            }
            None => {
                assert!(free0 == 0);
                assert!(same_outside(&b, &before, 0, 0));
            }
        }
        assert!(invariant(&b));
    }

    /// `free_frame` solo libera un marco ocupado y dentro del rango. Un
    /// marco libre o fuera de rango se rechaza sin cambiar nada.
    #[kani::proof]
    #[kani::unwind(3)]
    fn free_any() {
        let before: [u64; W] = kani::any();
        let mut bits = before;
        let mut b = any_state(&mut bits, W * 64);
        let free0 = b.free;
        let f: usize = kani::any();
        match b.free_frame(f) {
            Ok(()) => {
                assert!(f < b.nframes);
                assert!(used_in(&before, f));
                assert!(!b.is_used(f));
                assert!(same_outside(&b, &before, f, f + 1));
                assert!(b.free == free0 + 1);
            }
            Err(e) => {
                if f >= b.nframes {
                    assert!(e == FrameError::OutOfRange);
                } else {
                    assert!(e == FrameError::DoubleFree && !used_in(&before, f));
                }
                assert!(same_outside(&b, &before, 0, 0));
            }
        }
        assert!(invariant(&b));
    }

    /// Marcos en las pruebas que recorren el bitmap marco a marco: cruzan
    /// el borde entre palabras sin que CBMC explote.
    const SMALL: usize = 72;

    /// `alloc_contiguous` entrega `n` marcos que estaban todos libres,
    /// alineados y dentro del rango, y no toca ningún otro. `n` es concreto
    /// en cada prueba: con `n` simbólico, CBMC desenrolla el bucle interno
    /// tantas veces como el externo y se queda sin memoria.
    fn alloc_contiguous_n(n: usize) {
        let before: [u64; W] = kani::any();
        let mut bits = before;
        let mut b = any_state(&mut bits, SMALL);
        let free0 = b.free;
        let align: usize = kani::any();
        kani::assume(align <= 8);
        match b.alloc_contiguous(n, align) {
            Some(f) => {
                assert!(f % align == 0);
                assert!(f + n <= b.nframes);
                assert!(taken(&b, &before, f, f + n));
                assert!(same_outside(&b, &before, f, f + n));
                assert!(b.free == free0 - n);
            }
            None => assert!(same_outside(&b, &before, 0, 0)),
        }
        assert!(invariant(&b));
    }

    #[kani::proof]
    #[kani::unwind(74)]
    fn alloc_contiguous_2() {
        alloc_contiguous_n(2);
    }

    #[kani::proof]
    #[kani::unwind(74)]
    fn alloc_contiguous_3() {
        alloc_contiguous_n(3);
    }

    /// `n == 0` nunca entrega nada.
    #[kani::proof]
    #[kani::unwind(3)]
    fn alloc_contiguous_0() {
        let mut bits: [u64; W] = kani::any();
        let mut b = any_state(&mut bits, SMALL);
        assert!(b.alloc_contiguous(0, kani::any()).is_none());
    }

    /// Las regiones del mapa de memoria (solo al arrancar), con cualquier
    /// dirección y largo: sin pánicos ni desbordes, el invariante se
    /// mantiene, `reserve_region` ocupa todo marco que toca la región y
    /// `release_region` solo libera marcos enteros dentro de ella.
    #[kani::proof]
    #[kani::unwind(74)]
    fn regions_any() {
        let before: [u64; W] = kani::any();
        let mut bits = before;
        let mut b = any_state(&mut bits, SMALL);
        let base: u64 = kani::any();
        let len: u64 = kani::any();
        let f: usize = kani::any();
        kani::assume(f < b.nframes);
        let (start, end) = (f as u64 * FRAME_SIZE, (f as u64 + 1) * FRAME_SIZE);
        let reserve = kani::any();
        if reserve {
            b.reserve_region(base, len);
        } else {
            b.release_region(base, len);
        }
        let inside = base <= start && end <= base.saturating_add(len);
        let touches = base < end && start < base.saturating_add(len);
        if reserve {
            assert!(b.is_used(f) == (used_in(&before, f) || touches));
        } else {
            assert!(b.is_used(f) == (used_in(&before, f) && !inside));
        }
        assert!(invariant(&b));
    }

    /// `new` con cualquier tamaño: todo ocupado y sin marcos libres.
    #[kani::proof]
    #[kani::unwind(3)]
    fn new_any() {
        let mut bits: [u64; W] = kani::any();
        let b = FrameBitmap::new(&mut bits, kani::any());
        assert!(b.total() <= W * 64 && b.free_count() == 0);
        assert!(invariant(&b));
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
