//! Identificadores de espacio de direcciones para la TLB (PCID en x86_64).
//!
//! Con KPTI cada entrada y salida del kernel cambia de tablas. Sin PCID,
//! cada cambio vacía la TLB. Con PCID, la TLB guarda traducciones de varios
//! espacios a la vez, cada una marcada con su identificador, y cambiar de
//! tablas sin vaciar es posible. El peligro es usar una traducción vieja:
//! la de otra cell (una fuga entre cells) o la de una página que ya se
//! desmapeó.
//!
//! [`Asids`] reparte [`SLOTS`] lugares entre las raíces de las cells que
//! corren. Cada lugar tiene dos identificadores (tablas del kernel y de
//! usuario). [`Asids::switch`] dice en qué lugar corre una raíz y si hay
//! que vaciar sus identificadores antes de usarlos: siempre que el lugar
//! pase a otra raíz, que la raíz se haya olvidado (sus tablas se liberaron
//! y otra cell puede recibir la misma dirección) o que sus tablas hayan
//! cambiado de una forma que la TLB no ve sola (`invalidate`).
//!
//! Lo que demuestra Kani (`switch_is_safe`), con un modelo de la TLB: si
//! `switch` dice que no hace falta vaciar, la TLB de ese lugar solo tiene
//! traducciones de esa misma raíz y vigentes.

/// Lugares (pares de identificadores).
pub const SLOTS: usize = 8;

/// Qué hacer al pasar a una raíz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switch {
    /// Lugar: sus identificadores son `2 * slot + 1` (kernel) y
    /// `2 * slot + 2` (usuario).
    pub slot: usize,
    /// Vaciar el identificador de las tablas del kernel.
    pub flush_kernel: bool,
    /// Vaciar el identificador de las tablas de usuario.
    pub flush_user: bool,
}

/// Reparto de lugares entre raíces.
#[derive(Clone, Debug)]
pub struct Asids {
    /// Raíz de cada lugar (0 = libre).
    roots: [u64; SLOTS],
    kernel_dirty: [bool; SLOTS],
    user_dirty: [bool; SLOTS],
    /// Próximo lugar a reemplazar.
    next: usize,
}

impl Default for Asids {
    fn default() -> Self {
        Self::new()
    }
}

impl Asids {
    /// Todos los lugares libres.
    pub const fn new() -> Self {
        Self {
            roots: [0; SLOTS],
            kernel_dirty: [false; SLOTS],
            user_dirty: [false; SLOTS],
            next: 0,
        }
    }

    fn find(&self, root: u64) -> Option<usize> {
        if root == 0 {
            return None;
        }
        self.roots.iter().position(|&r| r == root)
    }

    /// Pasa a correr la raíz `root` (distinta de 0).
    pub fn switch(&mut self, root: u64) -> Switch {
        if let Some(i) = self.find(root) {
            let s = Switch {
                slot: i,
                flush_kernel: self.kernel_dirty[i],
                flush_user: self.user_dirty[i],
            };
            self.kernel_dirty[i] = false;
            self.user_dirty[i] = false;
            return s;
        }
        let i = self.next;
        self.next = (self.next + 1) % SLOTS;
        self.roots[i] = root;
        self.kernel_dirty[i] = false;
        self.user_dirty[i] = false;
        Switch {
            slot: i,
            flush_kernel: true,
            flush_user: true,
        }
    }

    /// Las tablas de `root` cambiaron de una forma que la TLB no ve sola
    /// (se desmapeó algo): la próxima vez que se usen sus identificadores,
    /// vaciarlos (los de las tablas del kernel, los de usuario o los dos).
    pub fn invalidate(&mut self, root: u64, kernel: bool, user: bool) {
        if let Some(i) = self.find(root) {
            self.kernel_dirty[i] |= kernel;
            self.user_dirty[i] |= user;
        }
    }

    /// Las tablas de `root` se liberaron: su lugar queda libre (otra cell
    /// puede recibir la misma dirección de raíz).
    pub fn forget(&mut self, root: u64) {
        if let Some(i) = self.find(root) {
            self.roots[i] = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuse_and_flush() {
        let mut a = Asids::new();
        let s = a.switch(0x1000);
        assert!(s.flush_kernel && s.flush_user);
        let s2 = a.switch(0x1000);
        assert_eq!(
            s2,
            Switch {
                slot: s.slot,
                flush_kernel: false,
                flush_user: false
            }
        );
        a.invalidate(0x1000, false, true);
        assert!(a.switch(0x1000).flush_user);
        a.forget(0x1000);
        let s3 = a.switch(0x1000); // la misma dirección, otras tablas
        assert!(s3.flush_kernel && s3.flush_user);
        for r in 1..=SLOTS as u64 {
            a.switch(r * 0x10000);
        }
        assert!(a.switch(0x1000).flush_kernel); // se lo reemplazaron
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Modelo de la TLB de cada lugar: de qué raíz puede tener traducciones
    /// (0 = ninguna) y si siguen vigentes.
    #[derive(Clone, Copy)]
    struct Tlb {
        root: u64,
        fresh: bool,
    }

    /// Con cualquier secuencia de operaciones (cambiar a una raíz,
    /// invalidar, olvidar) sobre unas pocas raíces: cada vez que `switch`
    /// dice que no hace falta vaciar un identificador, la TLB de ese lugar
    /// solo tiene traducciones de esa raíz y vigentes.
    #[kani::proof]
    #[kani::unwind(10)]
    fn switch_is_safe() {
        let mut a = Asids::new();
        let empty = Tlb {
            root: 0,
            fresh: true,
        };
        let mut kernel = [empty; SLOTS];
        let mut user = [empty; SLOTS];
        for _ in 0..4 {
            let root: u64 = kani::any();
            kani::assume(root >= 1 && root <= 3);
            match kani::any::<u8>() % 3 {
                0 => {
                    let s = a.switch(root);
                    assert!(s.slot < SLOTS);
                    for (tlb, flush) in [
                        (&mut kernel[s.slot], s.flush_kernel),
                        (&mut user[s.slot], s.flush_user),
                    ] {
                        if !flush {
                            // Lo que haya en la TLB es de esta raíz y vigente.
                            assert!(tlb.root == 0 || (tlb.root == root && tlb.fresh));
                        }
                        // Corre `root`: la TLB se llena con sus traducciones.
                        *tlb = Tlb { root, fresh: true };
                    }
                }
                1 => {
                    let (k, u): (bool, bool) = (kani::any(), kani::any());
                    a.invalidate(root, k, u);
                    for i in 0..SLOTS {
                        if kernel[i].root == root && k {
                            kernel[i].fresh = false;
                        }
                        if user[i].root == root && u {
                            user[i].fresh = false;
                        }
                    }
                }
                _ => {
                    a.forget(root);
                    // Sus tablas se liberaron: lo que quede en la TLB ya no
                    // vale para ninguna raíz nueva con la misma dirección.
                    for i in 0..SLOTS {
                        if kernel[i].root == root {
                            kernel[i].fresh = false;
                        }
                        if user[i].root == root {
                            user[i].fresh = false;
                        }
                    }
                }
            }
        }
    }
}
