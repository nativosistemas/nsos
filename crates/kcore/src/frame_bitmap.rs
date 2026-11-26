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

/// Marcos completamente contenidos en `[base, base+len)`: `[start, end)`.
fn inner_frames(base: u64, len: u64) -> (usize, usize) {
    let start = base.div_ceil(FRAME_SIZE) as usize;
    let end = (base.saturating_add(len) / FRAME_SIZE) as usize;
    (start, end)
}

/// Marcos que tocan `[base, base+len)`: `[start, end)`.
fn outer_frames(base: u64, len: u64) -> (usize, usize) {
    let start = (base / FRAME_SIZE) as usize;
    let end = base.saturating_add(len).div_ceil(FRAME_SIZE) as usize;
    (start, end)
}

/// Primer comienzo alineado a `align` después del marco ocupado `used`.
/// Todo comienzo entre `used + 1` y el resultado está desalineado.
fn next_start(used: usize, align: usize) -> usize {
    (used + 1).next_multiple_of(align)
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
        let (start, end) = inner_frames(base, len);
        self.mark(start, end, false);
    }

    /// Marca como ocupados los marcos que tocan `[base, base+len)`.
    pub fn reserve_region(&mut self, base: u64, len: u64) {
        let (start, end) = outer_frames(base, len);
        self.mark(start, end, true);
    }

    /// Deja los marcos de `[start, end)` que gestiona el bitmap ocupados
    /// (`used`) o libres, y lleva la cuenta de libres.
    fn mark(&mut self, start: usize, end: usize, used: bool) {
        for f in start..end.min(self.nframes) {
            if self.is_used(f) != used {
                self.set(f, used);
                if used {
                    self.free -= 1;
                } else {
                    self.free += 1;
                }
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
            match self.last_used(f, n) {
                Some(used) => f = next_start(used, align),
                None => {
                    self.mark(f, f + n, true);
                    return Some(f);
                }
            }
        }
        None
    }

    /// El último marco ocupado de `[f, f+n)`, si hay alguno.
    fn last_used(&self, f: usize, n: usize) -> Option<usize> {
        (f..f + n).rev().find(|&g| self.is_used(g))
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

    /// `n == 0` o una alineación que no es potencia de 2 nunca entregan
    /// nada.
    #[kani::proof]
    #[kani::unwind(3)]
    fn alloc_contiguous_0() {
        let mut bits: [u64; W] = kani::any();
        let mut b = any_state(&mut bits, W * 64);
        let (n, align): (usize, usize) = (kani::any(), kani::any());
        kani::assume(n == 0 || !align.is_power_of_two());
        assert!(b.alloc_contiguous(n, align).is_none());
    }

    // `alloc_contiguous` entero con 128 marcos es demasiado para CBMC:
    // desenrolla el bucle interno en cada vuelta del externo y se queda sin
    // memoria (también con `n` y `align` concretos). Se prueba por piezas;
    // juntas dan la propiedad del bucle (ver `verification/kani.md`).

    /// `last_used`: si no hay marco ocupado, toda la ventana está libre; si
    /// lo hay, está en la ventana, está ocupado y después de él (dentro de
    /// la ventana) todo está libre. Saltar a `next_start` desde ahí no
    /// saltea ningún comienzo posible: todo comienzo hasta `used` contiene
    /// a `used`.
    #[kani::proof]
    #[kani::unwind(5)]
    fn last_used_any() {
        let mut bits: [u64; W] = kani::any();
        let b = any_state(&mut bits, W * 64);
        let (f, n): (usize, usize) = (kani::any(), kani::any());
        kani::assume(n <= 3 && f + n <= b.nframes);
        match b.last_used(f, n) {
            None => assert!((0..W).all(|w| b.bits[w] & range(w, f, f + n) == 0)),
            Some(u) => {
                assert!(f <= u && u < f + n && b.is_used(u));
                assert!((0..W).all(|w| b.bits[w] & range(w, u + 1, f + n) == 0));
            }
        }
    }

    /// `next_start`: el resultado está alineado, es mayor que `used`, y
    /// entre `used` y el resultado no hay ningún comienzo alineado. Así el
    /// bucle solo prueba comienzos alineados y no se saltea ninguno.
    #[kani::proof]
    fn next_start_any() {
        let used: usize = kani::any();
        let k: u32 = kani::any();
        kani::assume(used < W * 64 && k < 8);
        let align = 1usize << k;
        let s = next_start(used, align);
        assert!(s % align == 0 && s > used && s - used <= align);
    }

    /// Marcos de las pruebas de `mark` y de `alloc_contiguous` chico.
    const SMALL: usize = 72;

    /// `mark` deja exactamente los marcos de `[start, end)` (dentro del
    /// rango) con el estado pedido, no toca ningún otro y mantiene la cuenta
    /// de libres. Con cualquier `start` y `end`, sin pánicos ni desbordes.
    #[kani::proof]
    #[kani::unwind(74)]
    fn mark_any() {
        let before: [u64; W] = kani::any();
        let mut bits = before;
        let mut b = any_state(&mut bits, SMALL);
        let (start, end, used): (usize, usize, bool) = (kani::any(), kani::any(), kani::any());
        b.mark(start, end, used);
        let hi = end.min(b.nframes);
        assert!(same_outside(&b, &before, start, hi));
        let full = if used { u64::MAX } else { 0 };
        assert!((0..W).all(|w| b.bits[w] & range(w, start, hi) == full & range(w, start, hi)));
        assert!(invariant(&b));
    }

    /// Los marcos que toman las regiones del mapa de memoria, con cualquier
    /// dirección y largo de 64 bits: `inner_frames` da exactamente los
    /// marcos enteros dentro de la región y `outer_frames` los que la
    /// tocan. El final de la región satura en `u64::MAX`.
    #[kani::proof]
    fn frames_any() {
        let (base, len, g): (u64, u64, u64) = (kani::any(), kani::any(), kani::any());
        let (lo, hi) = (g as u128 * 4096, (g as u128 + 1) * 4096);
        let (rb, re) = (base as u128, base.saturating_add(len) as u128);
        let (s, e) = inner_frames(base, len);
        assert!((s as u64 <= g && g < e as u64) == (rb <= lo && hi <= re));
        let (s, e) = outer_frames(base, len);
        assert!((s as u64 <= g && g < e as u64) == (lo < re && rb < hi));
    }

    /// `alloc_contiguous` entero, con hasta 8 marcos y cualquier `n` y
    /// alineación: los marcos que entrega estaban libres, están alineados y
    /// dentro del rango, y no toca ningún otro.
    #[kani::proof]
    #[kani::unwind(10)]
    fn alloc_contiguous_small() {
        let before: [u64; W] = kani::any();
        let mut bits = before;
        let mut b = any_state(&mut bits, 8);
        let free0 = b.free;
        let (n, align): (usize, usize) = (kani::any(), kani::any());
        kani::assume(n <= 8 && align <= 8);
        match b.alloc_contiguous(n, align) {
            Some(f) => {
                assert!(f % align == 0 && f + n <= b.nframes);
                assert!(taken(&b, &before, f, f + n));
                assert!(same_outside(&b, &before, f, f + n));
                assert!(b.free == free0 - n);
            }
            None => assert!(same_outside(&b, &before, 0, 0)),
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
