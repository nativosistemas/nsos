//! Índices y tamaños que no se pueden usar fuera de rango ni siquiera de
//! forma especulativa (Spectre v1).
//!
//! Cuando el kernel comprueba un valor que viene de una cell (un índice, un
//! tamaño) y después lo usa, la CPU puede predecir mal la comprobación y
//! ejecutar el uso con un valor fuera de rango antes de darse cuenta. Lo que
//! lea así deja rastros en la caché. Estas funciones calculan el valor ya
//! acotado *sin ramas*, a partir del valor y del tope, así que aun con la
//! predicción equivocada el valor que se usa está en rango (como
//! `array_index_nospec` de Linux). No son barreras: el compilador podría,
//! en teoría, volver a poner una rama; donde hace falta más, el kernel usa
//! `arch::cpu::speculation_barrier`.

/// `i` si `i < len`, y si no `0`, sin ramas. Vale para `i` y `len` menores
/// que 2^63.
pub fn index(i: usize, len: usize) -> usize {
    // Bit alto en 0 sii i < len (con i, len < 2^63): entonces la máscara es
    // todo unos; si no, todo ceros.
    let mask = (!(i | len.wrapping_sub(1).wrapping_sub(i)) as isize >> (usize::BITS - 1)) as usize;
    i & mask
}

/// `n` si `n <= max`, y si no `0`, sin ramas: para un tamaño que viene de
/// una cell y ya se comprobó contra `max`. Vale para `n` y `max` menores que
/// 2^63 - 1.
pub fn bound(n: usize, max: usize) -> usize {
    index(n, max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_and_out_of_range() {
        assert_eq!(index(3, 4), 3);
        assert_eq!(index(4, 4), 0);
        assert_eq!(index(usize::MAX >> 1, 4), 0);
        assert_eq!(bound(16, 16), 16);
        assert_eq!(bound(17, 16), 0);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// `index` da exactamente `i` dentro del rango y `0` fuera, con
    /// cualquier `i` y `len` menores que 2^63.
    #[kani::proof]
    fn index_any() {
        let (i, len): (usize, usize) = (kani::any(), kani::any());
        kani::assume(i < 1 << 63 && len < 1 << 63);
        let r = index(i, len);
        assert!(r == if i < len { i } else { 0 });
    }

    /// `bound` da `n` si no pasa de `max`, y `0` si pasa.
    #[kani::proof]
    fn bound_any() {
        let (n, max): (usize, usize) = (kani::any(), kani::any());
        kani::assume(n < 1 << 63 && max < (1 << 63) - 1);
        let r = bound(n, max);
        assert!(r == if n <= max { n } else { 0 });
    }
}
