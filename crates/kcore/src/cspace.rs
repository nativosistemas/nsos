//! Tabla de capacidades de una cell (CSpace).
//!
//! Genérica sobre el tipo de objeto `T` (en el kernel, una referencia
//! contada a un objeto del kernel), para poder probarla en el host.

use alloc::vec::Vec;
use nsos_abi::cap::{CapHandle, GEN_LIMIT, MAX_SLOTS};
use nsos_abi::{Error, Rights};

/// Una capacidad: objeto + derechos.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cap<T> {
    /// Objeto referido.
    pub object: T,
    /// Derechos sobre el objeto.
    pub rights: Rights,
}

struct Slot<T> {
    /// Generación actual. Llega a `GEN_LIMIT` cuando el slot se retira; como
    /// ningún handle tiene esa generación, ya no puede nombrar nada.
    generation: u32,
    cap: Option<Cap<T>>,
}

/// Avanza la generación de un slot recién vaciado. Devuelve `true` si el
/// slot se puede reusar, o `false` si agotó sus generaciones y queda retirado.
fn retire<T>(s: &mut Slot<T>) -> bool {
    s.generation += 1;
    s.generation < GEN_LIMIT
}

/// `i` si `i < len`, y si no `0`, calculado sin ramas (como
/// `array_index_nospec` de Linux). Mitigación parcial de Spectre v1: aunque
/// la CPU prediga mal el control de rango que sigue, el índice que usa
/// especulativamente ya está dentro de la tabla. Vale para `i` y `len`
/// menores que 2^63 (un slot es de 32 bits). No es una barrera: el
/// compilador podría, en teoría, volver a poner una rama; la barrera de
/// verdad (`lfence`) está en la entrada de las syscalls.
fn index_nospec(i: usize, len: usize) -> usize {
    // Bit alto en 0 sii i < len (con i, len < 2^63): entonces la máscara es
    // todo unos; si no, todo ceros.
    let mask = (!(i | len.wrapping_sub(1).wrapping_sub(i)) as isize >> (usize::BITS - 1)) as usize;
    i & mask
}

/// Tabla de capacidades.
pub struct CSpace<T> {
    // slots[0] existe pero nunca se usa (el handle 0 es inválido).
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    limit: u32,
    /// Máximo de slots (vivos, libres o retirados). Acota la memoria que una
    /// cell puede consumir reciclando handles.
    slot_limit: u32,
    live: usize,
    /// Inserciones reservadas con [`CSpace::reserve`] y todavía sin hacer.
    /// Cuentan contra `limit` y tienen un slot asegurado.
    reserved: usize,
}

/// Una inserción reservada: [`CSpace::insert_reserved`] la usa sin poder
/// fallar. Hay que usarla o devolverla con [`CSpace::cancel`] en la misma
/// `CSpace` que la dio.
#[must_use]
#[derive(Debug)]
pub struct Reservation(());

impl<T> CSpace<T> {
    /// Crea un CSpace con como mucho `limit` capacidades vivas.
    ///
    /// Puede tener hasta `4 * limit + 1` slots. Cada slot da `GEN_LIMIT`
    /// handles distintos antes de retirarse, así que una cell puede emitir
    /// unos `16 * limit * 1024` handles en toda su vida (16 millones con el
    /// límite del kernel). Después, `insert` devuelve `NoMemory`.
    pub fn new(limit: u32) -> Self {
        let slots = alloc::vec![Slot {
            generation: 0,
            cap: None,
        }];
        let limit = limit.min(MAX_SLOTS - 1);
        Self {
            slots,
            free: Vec::new(),
            limit,
            slot_limit: (limit.saturating_mul(4) + 1).min(MAX_SLOTS),
            live: 0,
            reserved: 0,
        }
    }

    /// Capacidades vivas.
    pub fn len(&self) -> usize {
        self.live
    }

    /// `true` si no hay capacidades.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Slots que todavía se pueden ocupar: libres más los que faltan crear.
    fn available(&self) -> usize {
        self.free.len() + (self.slot_limit as usize).saturating_sub(self.slots.len())
    }

    /// `true` si entran `n` capacidades más sin tocar lo reservado.
    fn room_for(&self, n: usize) -> bool {
        self.live + self.reserved + n <= self.limit as usize
            && self.reserved + n <= self.available()
    }

    /// Inserta una capacidad y devuelve su handle.
    pub fn insert(&mut self, cap: Cap<T>) -> Result<CapHandle, Error> {
        if !self.room_for(1) {
            return Err(Error::NoMemory);
        }
        Ok(self.place(cap))
    }

    /// Ocupa un slot con `cap`. Quien llama ya comprobó que hay lugar.
    fn place(&mut self, cap: Cap<T>) -> CapHandle {
        let slot = match self.free.pop() {
            Some(s) => s,
            None => {
                self.slots.push(Slot {
                    generation: 0,
                    cap: None,
                });
                (self.slots.len() - 1) as u32
            }
        };
        let s = &mut self.slots[slot as usize];
        s.cap = Some(cap);
        self.live += 1;
        CapHandle::new(slot, s.generation).expect("slot dentro de rango")
    }

    /// Reserva lugar para una capacidad. Hasta usarla o devolverla, ninguna
    /// otra inserción le quita el lugar. `cell_spawn` la pide antes de crear
    /// la cell, para no tener que dejarla corriendo sin supervisor.
    pub fn reserve(&mut self) -> Result<Reservation, Error> {
        if !self.room_for(1) {
            return Err(Error::NoMemory);
        }
        self.reserved += 1;
        Ok(Reservation(()))
    }

    /// Inserta en el lugar reservado. No falla.
    pub fn insert_reserved(&mut self, r: Reservation, cap: Cap<T>) -> CapHandle {
        let Reservation(()) = r;
        assert!(self.reserved > 0, "reserva de otra CSpace");
        self.reserved -= 1;
        self.place(cap)
    }

    /// Devuelve una reserva sin usarla.
    pub fn cancel(&mut self, r: Reservation) {
        let Reservation(()) = r;
        assert!(self.reserved > 0, "reserva de otra CSpace");
        self.reserved -= 1;
    }

    fn slot(&self, h: CapHandle) -> Result<&Slot<T>, Error> {
        // Fuera de rango, el índice pasa a ser el del slot 0, que siempre
        // está vacío: da `BadHandle`, como si no existiera.
        let s = &self.slots[index_nospec(h.slot() as usize, self.slots.len())];
        if !h.is_valid() || s.generation != h.generation() || s.cap.is_none() {
            return Err(Error::BadHandle);
        }
        Ok(s)
    }

    /// Busca una capacidad.
    pub fn get(&self, h: CapHandle) -> Result<&Cap<T>, Error> {
        Ok(self.slot(h)?.cap.as_ref().unwrap())
    }

    /// Busca una capacidad y exige derechos.
    pub fn get_with(&self, h: CapHandle, need: Rights) -> Result<&Cap<T>, Error> {
        let c = self.get(h)?;
        if !c.rights.contains(need) {
            return Err(Error::AccessDenied);
        }
        Ok(c)
    }

    /// Quita una capacidad, devolviéndola. El slot queda libre con una
    /// generación nueva, así el handle viejo deja de valer.
    pub fn remove(&mut self, h: CapHandle) -> Result<Cap<T>, Error> {
        self.slot(h)?;
        let i = index_nospec(h.slot() as usize, self.slots.len());
        let s = &mut self.slots[i];
        let cap = s.cap.take().unwrap();
        if retire(s) {
            self.free.push(h.slot());
        }
        self.live -= 1;
        Ok(cap)
    }

    /// Quita varias capacidades de forma atómica: o todas tienen `need` y se
    /// quitan, o no se toca nada. Se usa para transferirlas por IPC.
    pub fn remove_all(&mut self, hs: &[CapHandle], need: Rights) -> Result<Vec<Cap<T>>, Error> {
        for (i, h) in hs.iter().enumerate() {
            self.get_with(*h, need)?;
            if hs[..i].contains(h) {
                return Err(Error::InvalidArgs);
            }
        }
        let mut out = Vec::with_capacity(hs.len());
        for h in hs {
            out.push(self.remove(*h).unwrap());
        }
        Ok(out)
    }

    /// Recorre las capacidades vivas.
    pub fn caps(&self) -> impl Iterator<Item = &Cap<T>> {
        self.slots.iter().filter_map(|s| s.cap.as_ref())
    }

    /// Inserta varias de forma atómica (si no hay espacio, no inserta nada).
    pub fn insert_all(
        &mut self,
        caps: Vec<Cap<T>>,
    ) -> Result<Vec<CapHandle>, (Error, Vec<Cap<T>>)> {
        if !self.room_for(caps.len()) {
            return Err((Error::NoMemory, caps));
        }
        let mut hs = Vec::with_capacity(caps.len());
        for c in caps {
            hs.push(self.place(c));
        }
        Ok(hs)
    }

    /// Quita todo (al morir la cell). Devuelve los objetos para soltarlos.
    pub fn clear(&mut self) -> Vec<Cap<T>> {
        // Capacidad por cantidad de slots (no `live`): así no depende de que
        // `live` sea exacto, y Kani no razona sobre realocaciones.
        let mut out = Vec::with_capacity(self.slots.len());
        for (i, s) in self.slots.iter_mut().enumerate() {
            if let Some(c) = s.cap.take() {
                if retire(s) {
                    self.free.push(i as u32);
                }
                out.push(c);
            }
        }
        self.live = 0;
        out
    }
}

impl<T> CSpace<T> {
    /// Comprueba los invariantes internos (tests y pruebas de Kani). Escrito
    /// con bucles simples para que el verificador lo procese rápido.
    #[cfg(any(test, kani))]
    pub(crate) fn check_invariants(&self) {
        let n = self.slots.len();
        assert!(n >= 1 && self.slots[0].cap.is_none());
        assert!(n <= self.slot_limit as usize && self.slot_limit <= MAX_SLOTS);
        let mut live = 0;
        for s in &self.slots {
            assert!(s.generation <= GEN_LIMIT);
            if s.cap.is_some() {
                // Un slot retirado está vacío para siempre.
                assert!(s.generation < GEN_LIMIT);
                live += 1;
            }
        }
        assert!(live == self.live && self.live + self.reserved <= self.limit as usize);
        // Cada reserva tiene un slot asegurado.
        assert!(self.reserved <= self.available());
        for i in 0..self.free.len() {
            let f = self.free[i] as usize;
            assert!(f != 0 && f < n);
            assert!(self.slots[f].cap.is_none() && self.slots[f].generation < GEN_LIMIT);
            for &g in &self.free[..i] {
                assert!(g as usize != f);
            }
        }
    }
}

impl<T: Clone> CSpace<T> {
    /// Duplica con derechos atenuados (exige `DUPLICATE`).
    pub fn dup(&mut self, h: CapHandle, mask: Rights) -> Result<CapHandle, Error> {
        let c = self.get(h)?;
        let rights = c.rights.attenuate(mask).ok_or(Error::AccessDenied)?;
        let object = c.object.clone();
        self.insert(Cap { object, rights })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(o: u32, r: Rights) -> Cap<u32> {
        Cap {
            object: o,
            rights: r,
        }
    }

    #[test]
    fn insert_get_remove() {
        let mut cs = CSpace::new(16);
        let h = cs.insert(cap(7, Rights::SEND)).unwrap();
        assert_ne!(h.slot(), 0);
        assert_eq!(cs.get(h).unwrap().object, 7);
        assert_eq!(cs.remove(h).unwrap().object, 7);
        assert!(cs.get(h) == Err(Error::BadHandle));
        assert!(cs.is_empty());
    }

    #[test]
    fn stale_handle_rejected_after_reuse() {
        let mut cs = CSpace::new(16);
        let h1 = cs.insert(cap(1, Rights::SEND)).unwrap();
        cs.remove(h1).unwrap();
        let h2 = cs.insert(cap(2, Rights::SEND)).unwrap();
        assert_eq!(h1.slot(), h2.slot()); // slot reutilizado...
        assert_ne!(h1, h2); // ...con otra generación
        assert_eq!(cs.get(h1), Err(Error::BadHandle));
        assert_eq!(cs.get(h2).unwrap().object, 2);
        cs.check_invariants();
    }

    #[test]
    fn forged_handles() {
        let cs: CSpace<u32> = CSpace::new(16);
        assert_eq!(cs.get(CapHandle::INVALID), Err(Error::BadHandle));
        assert_eq!(cs.get(CapHandle(12345)), Err(Error::BadHandle));
        assert_eq!(cs.get(CapHandle(u32::MAX)), Err(Error::BadHandle));
    }

    #[test]
    fn rights_enforced_and_dup_attenuates() {
        let mut cs = CSpace::new(16);
        let h = cs
            .insert(cap(1, Rights::SEND | Rights::RECV | Rights::DUPLICATE))
            .unwrap();
        assert!(cs.get_with(h, Rights::SEND).is_ok());
        assert_eq!(cs.get_with(h, Rights::GRANT), Err(Error::AccessDenied));
        let d = cs.dup(h, Rights::SEND).unwrap();
        assert_eq!(cs.get(d).unwrap().rights, Rights::SEND);
        // La copia sin DUPLICATE no puede duplicarse.
        assert_eq!(cs.dup(d, Rights::ALL), Err(Error::AccessDenied));
    }

    #[test]
    fn limit() {
        let mut cs = CSpace::new(2);
        cs.insert(cap(1, Rights::NONE)).unwrap();
        cs.insert(cap(2, Rights::NONE)).unwrap();
        assert_eq!(cs.insert(cap(3, Rights::NONE)), Err(Error::NoMemory));
    }

    #[test]
    fn remove_all_is_atomic() {
        let mut cs = CSpace::new(16);
        let a = cs.insert(cap(1, Rights::GRANT)).unwrap();
        let b = cs.insert(cap(2, Rights::SEND)).unwrap(); // sin GRANT
        assert_eq!(
            cs.remove_all(&[a, b], Rights::GRANT),
            Err(Error::AccessDenied)
        );
        assert_eq!(cs.len(), 2); // no se tocó nada
        assert_eq!(
            cs.remove_all(&[a, a], Rights::GRANT),
            Err(Error::InvalidArgs)
        );
        let got = cs.remove_all(&[a], Rights::GRANT).unwrap();
        assert_eq!(got[0].object, 1);
        assert_eq!(cs.len(), 1);
    }

    #[test]
    fn insert_all_is_atomic() {
        let mut cs = CSpace::new(3);
        cs.insert(cap(0, Rights::NONE)).unwrap();
        let three = alloc::vec![
            cap(1, Rights::NONE),
            cap(2, Rights::NONE),
            cap(3, Rights::NONE)
        ];
        let (e, back) = cs.insert_all(three).unwrap_err();
        assert_eq!(e, Error::NoMemory);
        assert_eq!(back.len(), 3);
        assert_eq!(cs.len(), 1);
    }

    #[test]
    fn stale_handle_never_revives() {
        let mut cs = CSpace::new(4);
        let old = cs.insert(cap(1, Rights::SEND)).unwrap();
        cs.remove(old).unwrap();
        for _ in 0..GEN_LIMIT + 10 {
            let h = cs.insert(cap(2, Rights::SEND)).unwrap();
            assert_ne!(h, old);
            cs.remove(h).unwrap();
        }
        let h = cs.insert(cap(3, Rights::SEND)).unwrap();
        assert_ne!(h.slot(), old.slot()); // el slot 1 quedó retirado
        assert!(cs.get(old) == Err(Error::BadHandle));
        cs.check_invariants();
    }

    #[test]
    fn handle_space_is_bounded() {
        // limit 1 → 5 slots (el 0 no se usa): 4 slots de 4096 generaciones.
        let mut cs = CSpace::new(1);
        for _ in 0..4 * GEN_LIMIT {
            let h = cs.insert(cap(1, Rights::NONE)).unwrap();
            cs.remove(h).unwrap();
        }
        assert_eq!(cs.insert(cap(1, Rights::NONE)), Err(Error::NoMemory));
        let (e, back) = cs
            .insert_all(alloc::vec![cap(1, Rights::NONE)])
            .unwrap_err();
        assert_eq!((e, back.len()), (Error::NoMemory, 1));
    }

    #[test]
    fn reservation_keeps_its_place() {
        let mut cs = CSpace::new(2);
        cs.insert(cap(1, Rights::NONE)).unwrap();
        let r = cs.reserve().unwrap();
        // El único lugar que queda es de la reserva.
        assert_eq!(cs.insert(cap(2, Rights::NONE)), Err(Error::NoMemory));
        assert!(cs.reserve().is_err());
        let (e, _) = cs
            .insert_all(alloc::vec![cap(2, Rights::NONE)])
            .unwrap_err();
        assert_eq!(e, Error::NoMemory);
        let h = cs.insert_reserved(r, cap(3, Rights::NONE));
        assert_eq!(cs.get(h).unwrap().object, 3);
        cs.check_invariants();
        // Cancelar devuelve el lugar.
        let mut cs = CSpace::new(1);
        let r = cs.reserve().unwrap();
        cs.cancel(r);
        assert!(cs.insert(cap(1, Rights::NONE)).is_ok());
        cs.check_invariants();
    }

    #[test]
    fn reservation_counts_slots() {
        // limit 1 → 4 slots usables. Se gastan las generaciones de 3 y queda
        // uno: la reserva se lo queda aunque el límite de vivas lo permita.
        let mut cs = CSpace::new(1);
        for _ in 0..3 * GEN_LIMIT {
            let h = cs.insert(cap(1, Rights::NONE)).unwrap();
            cs.remove(h).unwrap();
        }
        let r = cs.reserve().unwrap();
        assert!(cs.reserve().is_err());
        let h = cs.insert_reserved(r, cap(2, Rights::NONE));
        cs.remove(h).unwrap();
        cs.check_invariants();
    }

    #[test]
    fn clear_invalidates_everything() {
        let mut cs = CSpace::new(16);
        let hs: Vec<_> = (0..5)
            .map(|i| cs.insert(cap(i, Rights::NONE)).unwrap())
            .collect();
        assert_eq!(cs.clear().len(), 5);
        for h in hs {
            assert!(cs.get(h) == Err(Error::BadHandle));
        }
    }
}

/// Pruebas de Kani: exploran *todos* los valores posibles de las entradas
/// (handles, derechos, límites, orden de operaciones) dentro de cotas chicas,
/// y comprueban que no hay panics, overflows ni índices fuera de rango, y que
/// se mantienen los invariantes. Correr con `cargo kani -p nsos-kcore`.
#[cfg(kani)]
mod proofs {
    use super::*;

    fn any_handle(issued: &[CapHandle]) -> CapHandle {
        if kani::any() && !issued.is_empty() {
            let i: usize = kani::any();
            kani::assume(i < issued.len());
            issued[i]
        } else {
            CapHandle(kani::any())
        }
    }

    fn any_cap() -> Cap<u8> {
        Cap {
            object: kani::any(),
            rights: Rights(kani::any()),
        }
    }

    /// Una CSpace cualquiera que cumpla los invariantes: 3 slots (más el 0)
    /// con generaciones y contenidos arbitrarios, incluidos slots retirados,
    /// lista libre en cualquier orden, y límites arbitrarios compatibles.
    /// Se arma directamente (con capacidad reservada, para que el verificador
    /// no tenga que razonar sobre realocaciones).
    fn any_state() -> (CSpace<u8>, Vec<CapHandle>) {
        const N: usize = 3;
        let mut slots = Vec::with_capacity(N + 3);
        let mut free = Vec::with_capacity(N + 3);
        let mut issued = Vec::with_capacity(N);
        slots.push(Slot {
            generation: 0,
            cap: None,
        });
        let mut live = 0;
        for i in 1..N {
            let generation: u32 = kani::any();
            kani::assume(generation <= GEN_LIMIT);
            let full = generation < GEN_LIMIT && kani::any();
            slots.push(Slot {
                generation,
                cap: if full { Some(any_cap()) } else { None },
            });
            if full {
                live += 1;
                issued.push(CapHandle::new(i as u32, generation).unwrap());
            } else if generation < GEN_LIMIT {
                free.push(i as u32);
            } else {
                // retirado: su último handle era de la generación anterior
                issued.push(CapHandle::new(i as u32, generation - 1).unwrap());
            }
        }
        if free.len() == 2 && kani::any() {
            free.swap(0, 1);
        }
        let limit: u32 = kani::any();
        kani::assume(limit <= 3);
        let extra: u32 = kani::any();
        kani::assume(extra <= 2);
        let reserved: usize = kani::any();
        kani::assume(reserved <= 2);
        let cs = CSpace {
            slots,
            free,
            limit,
            slot_limit: N as u32 + extra,
            live,
            reserved,
        };
        // Reservas compatibles: cuentan contra el límite y tienen slot.
        kani::assume(cs.live + cs.reserved <= cs.limit as usize);
        kani::assume(cs.reserved <= cs.available());
        cs.check_invariants();
        (cs, issued)
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn insert_any() {
        let (mut cs, _) = any_state();
        let c = any_cap();
        if let Ok(h) = cs.insert(c.clone()) {
            assert!(cs.get(h) == Ok(&c));
        }
        cs.check_invariants();
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn get_with_any() {
        let (cs, issued) = any_state();
        let need = Rights(kani::any());
        if let Ok(c) = cs.get_with(any_handle(&issued), need) {
            assert!(c.rights.contains(need));
        }
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn remove_any() {
        let (mut cs, issued) = any_state();
        let before = cs.len();
        let h = any_handle(&issued);
        if cs.remove(h).is_ok() {
            assert!(cs.get(h) == Err(Error::BadHandle));
            assert_eq!(cs.len(), before - 1);
        } else {
            assert_eq!(cs.len(), before);
        }
        cs.check_invariants();
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn dup_any() {
        let (mut cs, issued) = any_state();
        let h = any_handle(&issued);
        let src = cs.get(h).map(|c| c.rights);
        if let Ok(d) = cs.dup(h, Rights(kani::any())) {
            let r = cs.get(d).unwrap().rights;
            assert!(r.is_subset_of(src.unwrap()));
        }
        cs.check_invariants();
    }

    #[kani::proof]
    #[kani::unwind(4)]
    #[kani::solver(cadical)]
    fn remove_all_any() {
        let (mut cs, issued) = any_state();
        let before = cs.len();
        // Dos handles: cubre repetidos, mezclas válidos/ inválidos y atomicidad.
        let n = 2;
        let mut hs = Vec::with_capacity(2);
        for _ in 0..n {
            hs.push(any_handle(&issued));
        }
        let need = Rights(kani::any());
        match cs.remove_all(&hs, need) {
            Ok(got) => {
                assert_eq!(got.len(), n);
                assert!(got.iter().all(|c| c.rights.contains(need)));
                assert_eq!(cs.len(), before - n);
            }
            Err(_) => assert_eq!(cs.len(), before), // atómico
        }
        cs.check_invariants();
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn insert_all_any() {
        let (mut cs, _) = any_state();
        let before = cs.len();
        let n = 2;
        let mut caps = Vec::with_capacity(2);
        for _ in 0..n {
            caps.push(any_cap());
        }
        match cs.insert_all(caps) {
            Ok(hs) => {
                assert_eq!(hs.len(), n);
                assert_eq!(cs.len(), before + n);
            }
            Err((_, back)) => {
                assert_eq!(back.len(), n); // devuelve todo
                assert_eq!(cs.len(), before);
            }
        }
        cs.check_invariants();
    }

    #[kani::proof]
    #[kani::unwind(4)]
    #[kani::solver(cadical)]
    fn clear_any() {
        let (mut cs, issued) = any_state();
        let before = cs.len();
        let h = any_handle(&issued);
        assert_eq!(cs.clear().len(), before);
        assert!(cs.is_empty());
        assert!(cs.get(h).is_err());
        cs.check_invariants();
    }

    /// `reserve` desde cualquier CSpace (también con reservas pendientes):
    /// si la da, mantiene los invariantes, así que la reserva tiene lugar
    /// asegurado en el límite y en los slots.
    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn reserve_any() {
        let (mut cs, _) = any_state();
        let (live, reserved) = (cs.live, cs.reserved);
        match cs.reserve() {
            Ok(r) => {
                assert!(cs.reserved == reserved + 1 && cs.live == live);
                cs.check_invariants();
                cs.cancel(r);
                assert!(cs.reserved == reserved);
            }
            Err(e) => assert!(e == Error::NoMemory && cs.reserved == reserved),
        }
        cs.check_invariants();
    }

    /// Con una reserva pendiente, la operación `op` (insertar, insertar
    /// varias, quitar, duplicar o vaciar) deja lugar para ella:
    /// `insert_reserved` después no entra en pánico, el handle vale y nombra
    /// la capacidad insertada. Es lo que necesita `cell_spawn`. Una prueba
    /// por operación: juntas, CBMC se queda sin memoria.
    fn reserved_then(op: u8) {
        let (mut cs, issued) = any_state();
        let Ok(r) = cs.reserve() else { return };
        match op {
            0 => {
                let _ = cs.insert(any_cap());
            }
            1 => {
                let mut caps = Vec::with_capacity(2);
                caps.push(any_cap());
                caps.push(any_cap());
                let _ = cs.insert_all(caps);
            }
            2 => {
                let _ = cs.remove(any_handle(&issued));
            }
            3 => {
                let _ = cs.dup(any_handle(&issued), Rights(kani::any()));
            }
            _ => {
                let _ = cs.clear();
            }
        }
        let c = any_cap();
        let h = cs.insert_reserved(r, c.clone());
        assert!(cs.get(h) == Ok(&c));
        cs.check_invariants();
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn reserved_after_insert() {
        reserved_then(0);
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn reserved_after_insert_all() {
        reserved_then(1);
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn reserved_after_remove() {
        reserved_then(2);
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn reserved_after_dup() {
        reserved_then(3);
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn reserved_after_clear() {
        reserved_then(4);
    }

    /// `index_nospec` da exactamente `i` dentro de la tabla y `0` fuera,
    /// con cualquier `i` de 32 bits (un slot) y cualquier largo hasta 2^63.
    #[kani::proof]
    fn index_nospec_any() {
        let i: u32 = kani::any();
        let len: usize = kani::any();
        kani::assume(len < 1 << 63);
        let r = index_nospec(i as usize, len);
        assert!(r == if (i as usize) < len { i as usize } else { 0 });
    }

    /// Un handle cualquiera (incluso fabricado) nunca hace fallar a `get`.
    #[kani::proof]
    #[kani::unwind(4)]
    fn cspace_get_any_handle() {
        let mut cs: CSpace<u8> = CSpace::new(2);
        let _ = cs.insert(any_cap());
        let h = CapHandle(kani::any());
        let _ = cs.get(h);
        let _ = cs.get_with(h, Rights(kani::any()));
    }

    /// Slot con una generación cualquiera: quitar nunca hace que la
    /// generación dé la vuelta, y el handle viejo no vuelve a valer aunque se
    /// reuse el slot (o se use uno nuevo, si se retiró).
    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn cspace_generation_never_wraps() {
        let g: u32 = kani::any();
        kani::assume(g < GEN_LIMIT);
        let mut slots = Vec::with_capacity(4);
        slots.push(Slot {
            generation: 0,
            cap: None,
        });
        slots.push(Slot {
            generation: g,
            cap: Some(any_cap()),
        });
        let mut cs = CSpace {
            slots,
            free: Vec::with_capacity(4),
            limit: 2,
            slot_limit: 3,
            live: 1,
            reserved: 0,
        };
        cs.check_invariants();
        let old = CapHandle::new(1, g).unwrap();
        assert!(cs.remove(old).is_ok());
        cs.check_invariants();
        let h = cs.insert(any_cap()).unwrap();
        assert_ne!(h, old);
        assert!(cs.get(old) == Err(Error::BadHandle));
        cs.check_invariants();
    }

    /// Los límites extremos no desbordan.
    #[kani::proof]
    fn cspace_new_any_limit() {
        let cs: CSpace<u8> = CSpace::new(kani::any());
        assert!(cs.limit < MAX_SLOTS && cs.slot_limit <= MAX_SLOTS);
        cs.check_invariants();
    }
}
