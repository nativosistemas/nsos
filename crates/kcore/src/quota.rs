//! Cuenta de memoria de una cell: cuota disponible y marcos usados.
//!
//! Es la implementación del modelo `verification/lean/Nsos/Quota.lean`.
//! Cada operación hace exactamente lo que el paso correspondiente del
//! modelo le hace a la cell que lo paga o recibe:
//!
//! | Operación | Paso del modelo |
//! |---|---|
//! | [`Quota::charge`] | `alloc c n` |
//! | [`Quota::refund`] | `release c n` |
//! | [`Quota::split`] | `spawn p d q` (lado del supervisor y de la hija) |
//! | [`Quota::absorb`] | `kill p d` (lado del supervisor) |
//!
//! Las pruebas de Kani de abajo demuestran esas ecuaciones con cualquier
//! valor: el modelo en Lean demuestra que, con ellas, ninguna cell deja sin
//! memoria a otra (`QuotaModel.no_starvation`). Lo que hace el kernel
//! alrededor (una cuenta por cell, a quién le devuelve la de una cell
//! muerta) está en `kernel/src/quota.rs`.

/// Cuenta de memoria, en marcos.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Quota {
    /// Marcos que todavía se pueden usar.
    pub avail: u64,
    /// Marcos en uso.
    pub used: u64,
}

impl Quota {
    /// Una cuenta con `avail` marcos disponibles y ninguno usado.
    pub const fn new(avail: u64) -> Self {
        Self { avail, used: 0 }
    }

    /// Cobra `n` marcos: solo si entran en la cuota disponible. Si no, no
    /// cambia nada.
    pub fn charge(&mut self, n: u64) -> bool {
        if n > self.avail {
            return false;
        }
        let Some(used) = self.used.checked_add(n) else {
            return false;
        };
        self.avail -= n;
        self.used = used;
        true
    }

    /// Devuelve `n` marcos usados a la cuota disponible. Si no hay tantos
    /// usados, no cambia nada.
    pub fn refund(&mut self, n: u64) -> bool {
        if n > self.used {
            return false;
        }
        let Some(avail) = self.avail.checked_add(n) else {
            return false;
        };
        self.used -= n;
        self.avail = avail;
        true
    }

    /// Separa `q` marcos de la cuota disponible para una cuenta nueva (una
    /// cell hija). Si no hay tantos, no cambia nada.
    pub fn split(&mut self, q: u64) -> Option<Quota> {
        if q > self.avail {
            return None;
        }
        self.avail -= q;
        Some(Quota::new(q))
    }

    /// Recibe la cuenta de una hija que murió: su cuota disponible vuelve a
    /// la disponible, y sus marcos usados pasan a ser usados de esta cuenta
    /// (se devuelven con [`Quota::refund`] cuando se liberan). Si no entra
    /// en 64 bits, no cambia nada.
    pub fn absorb(&mut self, child: Quota) -> bool {
        let (Some(avail), Some(used)) = (
            self.avail.checked_add(child.avail),
            self.used.checked_add(child.used),
        ) else {
            return false;
        };
        self.avail = avail;
        self.used = used;
        true
    }

    /// Marcos de la cuenta, disponibles más usados.
    pub fn total(&self) -> u128 {
        self.avail as u128 + self.used as u128
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_refund_split_absorb() {
        let mut root = Quota::new(100);
        let mut child = root.split(30).unwrap();
        assert_eq!((root.avail, child.avail), (70, 30));
        assert!(child.charge(25));
        assert!(!child.charge(6)); // se pasa de la cuota
        assert!(child.refund(5));
        assert!(!child.refund(21));
        assert_eq!(
            child,
            Quota {
                avail: 10,
                used: 20
            }
        );
        assert!(root.absorb(child));
        assert_eq!(
            root,
            Quota {
                avail: 80,
                used: 20
            }
        );
        assert!(root.split(81).is_none());
        assert_eq!(root.total(), 100);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn any_quota() -> Quota {
        Quota {
            avail: kani::any(),
            used: kani::any(),
        }
    }

    /// `charge` es el paso `alloc c n` del modelo: si `n` entra en la cuota
    /// disponible, la baja en `n` y sube los usados en `n`; si no, no cambia
    /// nada. Nunca se pasa de la cuota.
    #[kani::proof]
    fn charge_any() {
        let q0 = any_quota();
        let mut q = q0;
        let n: u64 = kani::any();
        let ok = q.charge(n);
        kani::assume(q0.used.checked_add(n).is_some());
        assert!(ok == (n <= q0.avail));
        if ok {
            assert!(q.avail == q0.avail - n && q.used == q0.used + n);
        } else {
            assert!(q == q0);
        }
        assert!(q.total() == q0.total());
    }

    /// `refund` es el paso `release c n`: si hay `n` usados, vuelven a la
    /// cuota disponible; si no, no cambia nada.
    #[kani::proof]
    fn refund_any() {
        let q0 = any_quota();
        let mut q = q0;
        let n: u64 = kani::any();
        let ok = q.refund(n);
        kani::assume(q0.avail.checked_add(n).is_some());
        assert!(ok == (n <= q0.used));
        if ok {
            assert!(q.avail == q0.avail + n && q.used == q0.used - n);
        } else {
            assert!(q == q0);
        }
        assert!(q.total() == q0.total());
    }

    /// `split` es el paso `spawn p d q`: el supervisor pierde `q` de su
    /// cuota disponible, la hija empieza con exactamente `q` y nada usado, y
    /// entre los dos suman lo mismo que antes. Sin `q` disponible, nada.
    #[kani::proof]
    fn split_any() {
        let p0 = any_quota();
        let mut p = p0;
        let q: u64 = kani::any();
        match p.split(q) {
            Some(c) => {
                assert!(q <= p0.avail);
                assert!(p.avail == p0.avail - q && p.used == p0.used);
                assert!(c == Quota::new(q));
                assert!(p.total() + c.total() == p0.total());
            }
            None => assert!(q > p0.avail && p == p0),
        }
    }

    /// `absorb` es el paso `kill p d` (con los usados de la hija pasando al
    /// supervisor): lo disponible y lo usado de la hija se suman a los del
    /// supervisor, sin perder ni agregar marcos.
    #[kani::proof]
    fn absorb_any() {
        let p0 = any_quota();
        let c = any_quota();
        let mut p = p0;
        let ok = p.absorb(c);
        if ok {
            assert!(p.avail == p0.avail + c.avail && p.used == p0.used + c.used);
            assert!(p.total() == p0.total() + c.total());
        } else {
            assert!(p == p0);
        }
        // Mientras el total del sistema entre en 64 bits, nunca falla.
        if p0.avail as u128 + c.avail as u128 <= u64::MAX as u128
            && p0.used as u128 + c.used as u128 <= u64::MAX as u128
        {
            assert!(ok);
        }
    }
}
