//! Handles a objetos: qué objeto representa cada uno y si permite escribir.
//!
//! En la cell `almacen`, cada handle es un extremo de canal: el cliente
//! tiene uno y el almacén el otro, guardado en el mismo índice que el
//! [`Handle`] de esta tabla. Tener el extremo es la autoridad; el almacén
//! no sabe (ni necesita saber) qué cell lo usa.
//!
//! Reglas (modelo: parte B de `verification/lean/Nsos/Store.lean`):
//! * [`Handles::open`] toma el dueño de la **raíz** por la que llegó el
//!   pedido, nunca del mensaje (`Store.change_needs_owner`).
//! * [`Handles::dup_ro`] da siempre solo lectura (`Store.rw_handle_from_owner`).
//! * Escribir exige un handle con `write` ([`Handles::writable`]).

use crate::layout::Name;

pub const MAX_HANDLES: usize = 32;

/// Un objeto abierto.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handle {
    pub owner: u32,
    pub name: Name,
    pub write: bool,
}

pub struct Handles {
    slots: [Option<Handle>; MAX_HANDLES],
}

impl Default for Handles {
    fn default() -> Self {
        Self::new()
    }
}

impl Handles {
    pub const fn new() -> Self {
        Handles {
            slots: [None; MAX_HANDLES],
        }
    }

    fn free_slot(&self) -> Option<usize> {
        self.slots.iter().position(|s| s.is_none())
    }

    /// `OPEN` por la raíz de `root_owner`: handle nuevo al objeto `name` de
    /// ese dueño. `None` si no hay lugar.
    pub fn open(&mut self, root_owner: u32, name: Name, write: bool) -> Option<usize> {
        let i = self.free_slot()?;
        self.slots[i] = Some(Handle {
            owner: root_owner,
            name,
            write,
        });
        Some(i)
    }

    /// `DUP_RO` por el handle `src`: handle nuevo al mismo objeto, de solo
    /// lectura.
    pub fn dup_ro(&mut self, src: usize) -> Option<usize> {
        let h = self.get(src)?;
        let i = self.free_slot()?;
        self.slots[i] = Some(Handle { write: false, ..h });
        Some(i)
    }

    pub fn get(&self, h: usize) -> Option<Handle> {
        *self.slots.get(h)?
    }

    /// El handle `h`, solo si permite escribir.
    pub fn writable(&self, h: usize) -> Option<Handle> {
        self.get(h).filter(|x| x.write)
    }

    pub fn close(&mut self, h: usize) {
        if let Some(s) = self.slots.get_mut(h) {
            *s = None;
        }
    }

    /// Índices de los handles abiertos.
    pub fn open_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_some())
            .map(|(i, _)| i)
    }
}
