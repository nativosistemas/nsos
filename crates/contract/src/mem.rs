//! Objetos `Memory` tipados por sus derechos.
//!
//! `Mem<RW>` es una capacidad a un objeto `Memory` con `READ` y `WRITE`;
//! `Mem<RO>`, una con `READ` y **sin** `WRITE`. Un `Mem<RO>` solo se obtiene
//! atenuando uno escribible ([`Mem::read_only`], que hace `cap_dup` sin
//! `WRITE`) o recibiéndolo en un mensaje, y al recibirlo se comprueba que no
//! tenga `WRITE`. Así, un mensaje cuyo campo es `Mem<RO>` (por ejemplo
//! `restore::Restore`) nunca lleva una capacidad escribible: lo que en la
//! actualización en vivo garantizaba la disciplina del supervisor
//! (`Upgrade.restoring_mem_at_sup`) ahora lo garantiza el tipo.
//!
//! Como [`Endpoint`](crate::Endpoint), no suelta el handle al destruirse: se
//! suelta con [`Mem::release`].

use core::marker::PhantomData;

use nsos_abi::{CapHandle, Error, ObjectType, Rights};

use crate::chan::Transport;
use crate::wire::{Field, Reader, Writer};

/// Derechos de un [`Mem`].
pub trait Access {
    /// Derechos que tiene que tener.
    const NEED: Rights;
    /// Derechos que no puede tener.
    const FORBID: Rights;
}

/// Lectura y escritura.
#[derive(Debug)]
pub struct RW;
/// Solo lectura: tiene `READ` y no tiene `WRITE`.
#[derive(Debug)]
pub struct RO;

impl Access for RW {
    const NEED: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
    const FORBID: Rights = Rights::NONE;
}

impl Access for RO {
    const NEED: Rights = Rights::READ;
    const FORBID: Rights = Rights::WRITE;
}

/// Capacidad a un objeto `Memory` con los derechos de `A`.
#[must_use = "un Mem descartado deja el handle ocupado; usá Mem::release"]
#[derive(Debug)]
pub struct Mem<A: Access> {
    h: CapHandle,
    _a: PhantomData<A>,
}

/// `true` si `h` es un objeto `Memory` con los derechos de `A`.
pub(crate) fn is_mem<T: Transport, A: Access>(t: T, h: CapHandle) -> bool {
    match t.cap_info(h) {
        Ok(i) => {
            let r = Rights(i.rights);
            i.object_type == ObjectType::Memory as u32
                && r.contains(A::NEED)
                && (r & A::FORBID) == Rights::NONE
        }
        Err(_) => false,
    }
}

impl<A: Access> Mem<A> {
    /// Toma un handle crudo (por ejemplo de `mem_create`) comprobando que
    /// sea un objeto `Memory` con los derechos de `A`.
    pub fn open<T: Transport>(t: T, h: CapHandle) -> Result<Mem<A>, Error> {
        if is_mem::<T, A>(t, h) {
            Ok(Mem { h, _a: PhantomData })
        } else {
            Err(Error::WrongType)
        }
    }

    /// El handle (para `mem_map`).
    pub fn handle(&self) -> CapHandle {
        self.h
    }

    /// Suelta el handle (el kernel quita también sus mapeos).
    pub fn release<T: Transport>(self, t: T) {
        t.cap_drop(self.h);
    }
}

impl Mem<RW> {
    /// Una copia de solo lectura (`READ | GRANT`, para poder pasarla). Esta
    /// se queda igual.
    pub fn read_only<T: Transport>(&self, t: T) -> Result<Mem<RO>, Error> {
        let h = t.cap_dup(self.h, Rights(Rights::READ.0 | Rights::GRANT.0))?;
        // `cap_dup` solo atenúa, pero se comprueba igual: el tipo promete
        // que no hay `WRITE`.
        Mem::open(t, h).inspect_err(|_| t.cap_drop(h))
    }
}

impl<A: Access> Field for Mem<A> {
    const DATA: usize = 0;
    const CAPS: usize = 1;
    fn put(self, w: &mut Writer<'_>) {
        w.cap(self.h);
    }
    fn take<T: Transport>(r: &mut Reader<'_>, t: T) -> Option<Self> {
        let h = r.cap()?;
        is_mem::<T, A>(t, h).then_some(Mem { h, _a: PhantomData })
    }
}
