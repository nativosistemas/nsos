//! Almacén de objetos de nsos: objetos con dueño, nombre y versión sobre un
//! dispositivo de bloques, con escritura copy-on-write.
//!
//! Lo usa la cell `almacen` (sobre el driver `virtblk`), pero no sabe nada de
//! cells ni de IPC: trabaja sobre cualquier [`BlockDev`], así que se prueba
//! en el host, incluso simulando caídas en cada escritura posible.
//!
//! ## Cómo escribe sin dejar datos a medias
//!
//! El disco tiene dos ranuras de superbloque (bloques 0 y 1). El estado
//! confirmado es el de la ranura válida con mayor secuencia. Escribir un
//! objeto ([`Store::put`]):
//! 1. pone los datos nuevos en bloques **libres** (que el estado confirmado
//!    no usa) y calcula su SHA-256;
//! 2. escribe la tabla nueva (todos los objetos) en otro bloque libre;
//! 3. `flush`;
//! 4. escribe el superbloque con secuencia + 1 en la **otra** ranura;
//! 5. `flush`. Recién ahí el objeto nuevo existe.
//!
//! Si la máquina se cae en cualquier punto, el estado confirmado sigue
//! intacto y al montar ([`Store::mount`]) se recupera el viejo o el nuevo,
//! nunca una mezcla: una ranura solo vale si su tabla y todos los datos
//! coinciden con sus hashes. El modelo y la demostración están en
//! `verification/lean/Nsos/Store.lean` (`Store.crash_safe`,
//! `Store.commit_good`, `Store.mount_good`); este código sigue sus pasos.
//!
//! ## Qué no hace (todavía)
//!
//! * Un objeto se escribe entero; no hay escrituras parciales.
//! * No borra objetos.
//! * Los hashes detectan daños y escrituras a medias, no a alguien que
//!   reescribe el disco a propósito (eso pide una clave: ver
//!   `docs/amenazas.md`).

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

pub mod alloc;
pub mod handles;
pub mod layout;
pub mod proto;

use alloc::Bitmap;
use layout::{
    blocks_for, classify, decode_super, decode_table, encode_super, encode_table, Entry, Name,
    SlotKind, Super, EMPTY_ENTRY, FIRST_DATA_BLOCK,
};
use nsos_link::sha256::{sha256, Sha256};

/// Tamaño de bloque del almacén.
pub const BLOCK: usize = 4096;
/// Objetos como máximo (los que entran en una tabla de un bloque).
pub const MAX_OBJECTS: usize = 30;
/// Largo máximo de un nombre.
pub const NAME_MAX: usize = 31;
/// Bloques de datos por objeto como máximo (64 KiB).
pub const MAX_OBJECT_BLOCKS: u32 = 16;
/// Bloques del disco que se usan como máximo (64 MiB).
pub const MAX_BLOCKS: u32 = 16384;

/// Dispositivo de bloques.
pub trait BlockDev {
    /// Cantidad de bloques de [`BLOCK`] bytes.
    fn blocks(&self) -> u32;
    fn read(&mut self, blk: u32, buf: &mut [u8; BLOCK]) -> Result<(), DevError>;
    fn write(&mut self, blk: u32, buf: &[u8; BLOCK]) -> Result<(), DevError>;
    /// Todo lo escrito antes queda en el medio permanente.
    fn flush(&mut self) -> Result<(), DevError>;
}

/// El dispositivo falló.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DevError;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// El dispositivo falló.
    Dev,
    /// El disco tiene otra cosa (no se toca).
    Foreign,
    /// El disco está vacío o dañado y no se permitió formatearlo.
    Unformatted,
    /// No hay bloques libres suficientes, o el disco es muy chico.
    NoSpace,
    /// El objeto supera [`MAX_OBJECT_BLOCKS`].
    TooBig,
    /// Ya hay [`MAX_OBJECTS`] objetos.
    TooManyObjects,
    /// No existe el objeto.
    NotFound,
    /// La versión esperada no es la actual (que va adentro; 0 = no existe).
    VersionMismatch(u64),
}

impl From<DevError> for Error {
    fn from(_: DevError) -> Error {
        Error::Dev
    }
}

/// Qué pasó al montar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MountInfo {
    /// El disco estaba vacío y se formateó.
    pub formatted: bool,
    /// La otra ranura tenía un superbloque de una escritura que no se
    /// completó, y se borró.
    pub cleaned: bool,
    pub slot: u32,
    pub seq: u64,
    pub objects: usize,
}

/// Un almacén montado.
pub struct Store<D: BlockDev> {
    dev: D,
    disk_blocks: u32,
    slot: u32,
    seq: u64,
    table: u32,
    entries: [Entry; MAX_OBJECTS],
    count: usize,
    /// La otra ranura pudo quedar con un superbloque de una escritura que
    /// falló: hay que borrarla antes de la próxima.
    stale_other: bool,
    buf: [u8; BLOCK],
}

/// Estado de una ranura válida.
struct View {
    seq: u64,
    table: u32,
    count: usize,
}

impl<D: BlockDev> Store<D> {
    /// Monta el almacén del disco. Si no hay ninguna ranura válida y el disco
    /// está vacío (o solo tiene superbloques nuestros a medias), lo formatea
    /// si `allow_format`. Un disco con otra cosa nunca se toca.
    pub fn mount(dev: D, allow_format: bool) -> Result<(Store<D>, MountInfo), Error> {
        let disk_blocks = dev.blocks().min(MAX_BLOCKS);
        let mut s = Store {
            dev,
            disk_blocks,
            slot: 0,
            seq: 0,
            table: 0,
            entries: [EMPTY_ENTRY; MAX_OBJECTS],
            count: 0,
            stale_other: false,
            buf: [0; BLOCK],
        };
        if disk_blocks < 4 {
            return Err(Error::NoSpace);
        }
        // Recuperación: la ranura válida de mayor secuencia (`Store.recover`).
        let mut other = [EMPTY_ENTRY; MAX_OBJECTS];
        let v0 = s.view(0, &mut other)?;
        if v0.is_some() {
            s.entries = other;
        }
        let v1 = s.view(1, &mut other)?;
        let chosen = match (v0, v1) {
            (Some(a), Some(b)) if a.seq < b.seq => Some((1, b, true)),
            (Some(a), _) => Some((0, a, false)),
            (None, Some(b)) => Some((1, b, true)),
            (None, None) => None,
        };
        let Some((slot, v, take_other)) = chosen else {
            return s.format_if_allowed(allow_format);
        };
        if take_other {
            s.entries = other;
        }
        s.slot = slot;
        s.seq = v.seq;
        s.table = v.table;
        s.count = v.count;
        // Limpieza (`Store.mount`): si la otra ranura tiene un superbloque
        // de secuencia igual o mayor (que entonces no valida), se borra.
        // Si no, una escritura posterior podría completar su árbol y
        // resucitar un estado que nunca se confirmó (`Store.stale_resurrects`).
        let o = 1 - slot;
        s.dev.read(o, &mut s.buf)?;
        let mut cleaned = false;
        if let Some(sb) = decode_super(&s.buf, disk_blocks) {
            if sb.seq >= s.seq {
                s.buf.fill(0);
                s.dev.write(o, &s.buf)?;
                s.dev.flush()?;
                cleaned = true;
            }
        }
        let info = MountInfo {
            formatted: false,
            cleaned,
            slot,
            seq: s.seq,
            objects: s.count,
        };
        Ok((s, info))
    }

    /// La ranura `i`, si valida entera: superbloque, tabla y datos de cada
    /// objeto con sus hashes (`Store.view` y `Store.TreeOk`).
    fn view(&mut self, i: u32, out: &mut [Entry; MAX_OBJECTS]) -> Result<Option<View>, Error> {
        self.dev.read(i, &mut self.buf)?;
        let Some(sb) = decode_super(&self.buf, self.disk_blocks) else {
            return Ok(None);
        };
        self.dev.read(sb.table, &mut self.buf)?;
        if sha256(&self.buf) != sb.table_hash {
            return Ok(None);
        }
        let Some(count) = decode_table(&self.buf, self.disk_blocks, out) else {
            return Ok(None);
        };
        for e in out[..count].iter() {
            if self.data_hash(e)? != e.hash {
                return Ok(None);
            }
        }
        Ok(Some(View {
            seq: sb.seq,
            table: sb.table,
            count,
        }))
    }

    fn data_hash(&mut self, e: &Entry) -> Result<[u8; 32], Error> {
        let mut h = Sha256::new();
        let mut left = e.len as usize;
        for i in 0..e.nblocks {
            self.dev.read(e.start + i, &mut self.buf)?;
            let n = left.min(BLOCK);
            h.update(&self.buf[..n]);
            left -= n;
        }
        Ok(h.finish())
    }

    fn format_if_allowed(mut self, allow: bool) -> Result<(Store<D>, MountInfo), Error> {
        for i in 0..2 {
            self.dev.read(i, &mut self.buf)?;
            if classify(&self.buf) == SlotKind::Foreign {
                return Err(Error::Foreign);
            }
        }
        if !allow {
            return Err(Error::Unformatted);
        }
        // `Store.format`: tabla vacía en 2, ranura 1 a cero, ranura 0 con la
        // secuencia 1.
        encode_table(&[], &mut self.buf);
        let table_hash = sha256(&self.buf);
        self.dev.write(FIRST_DATA_BLOCK, &self.buf)?;
        self.buf.fill(0);
        self.dev.write(1, &self.buf)?;
        self.dev.flush()?;
        let sb = Super {
            seq: 1,
            table: FIRST_DATA_BLOCK,
            table_hash,
        };
        encode_super(&sb, &mut self.buf);
        self.dev.write(0, &self.buf)?;
        self.dev.flush()?;
        self.slot = 0;
        self.seq = 1;
        self.table = FIRST_DATA_BLOCK;
        self.count = 0;
        let info = MountInfo {
            formatted: true,
            cleaned: false,
            slot: 0,
            seq: 1,
            objects: 0,
        };
        Ok((self, info))
    }

    /// Objetos confirmados.
    pub fn entries(&self) -> &[Entry] {
        &self.entries[..self.count]
    }

    /// Secuencia del estado confirmado.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    fn index(&self, owner: u32, name: &Name) -> Option<usize> {
        self.entries()
            .iter()
            .position(|e| e.owner == owner && e.name == *name)
    }

    /// El objeto `name` de `owner`.
    pub fn get(&self, owner: u32, name: &Name) -> Option<Entry> {
        self.index(owner, name).map(|i| self.entries[i])
    }

    /// Lee el bloque `i` de los datos de `e` (lo que pasa de `e.len` viene
    /// a cero).
    pub fn read_block(&mut self, e: &Entry, i: u32, out: &mut [u8; BLOCK]) -> Result<(), Error> {
        if i >= e.nblocks {
            return Err(Error::NotFound);
        }
        self.dev.read(e.start + i, out)?;
        Ok(())
    }

    /// Escribe el objeto `name` de `owner` con `len` bytes; `fill(i, buf)`
    /// llena el bloque `i`. Si `expected` es `Some(v)`, solo escribe si la
    /// versión actual es `v` (0 = el objeto no existe). Devuelve la versión
    /// nueva. Si falla, el estado confirmado no cambia.
    pub fn put(
        &mut self,
        owner: u32,
        name: &Name,
        len: usize,
        expected: Option<u64>,
        mut fill: impl FnMut(u32, &mut [u8; BLOCK]) -> Result<(), Error>,
    ) -> Result<u64, Error> {
        if len > MAX_OBJECT_BLOCKS as usize * BLOCK {
            return Err(Error::TooBig);
        }
        let len = len as u32;
        let idx = self.index(owner, name);
        let current = idx.map(|i| self.entries[i].version).unwrap_or(0);
        if let Some(v) = expected {
            if v != current {
                return Err(Error::VersionMismatch(current));
            }
        }
        if idx.is_none() && self.count == MAX_OBJECTS {
            return Err(Error::TooManyObjects);
        }
        let other = 1 - self.slot;
        if self.stale_other {
            self.buf.fill(0);
            self.dev.write(other, &self.buf)?;
            self.dev.flush()?;
            self.stale_other = false;
        }
        // Bloques libres respecto del estado confirmado (`Store.Fresh`).
        let mut used = Bitmap::<{ MAX_BLOCKS as usize / 64 }>::new();
        used.mark(0, FIRST_DATA_BLOCK);
        used.mark(self.table, 1);
        for e in self.entries() {
            used.mark(e.start, e.nblocks);
        }
        let nblocks = blocks_for(len);
        let start = used
            .find_run(nblocks, self.disk_blocks)
            .ok_or(Error::NoSpace)?;
        used.mark(start, nblocks);
        let table = used.find_run(1, self.disk_blocks).ok_or(Error::NoSpace)?;

        // Desde aquí la otra ranura puede cambiar.
        self.stale_other = true;
        // 1. Datos.
        let mut h = Sha256::new();
        let mut left = len as usize;
        for i in 0..nblocks {
            self.buf.fill(0);
            fill(i, &mut self.buf)?;
            let n = left.min(BLOCK);
            self.buf[n..].fill(0);
            h.update(&self.buf[..n]);
            left -= n;
            self.dev.write(start + i, &self.buf)?;
        }
        let entry = Entry {
            owner,
            name: *name,
            version: current + 1,
            len,
            start: if nblocks == 0 { 0 } else { start },
            nblocks,
            hash: h.finish(),
        };
        // 2. Tabla: el objeto nuevo primero y los demás como estaban
        // (`Store.put`).
        let mut next = [EMPTY_ENTRY; MAX_OBJECTS];
        next[0] = entry;
        let mut n = 1;
        for (j, e) in self.entries().iter().enumerate() {
            if Some(j) != idx {
                next[n] = *e;
                n += 1;
            }
        }
        encode_table(&next[..n], &mut self.buf);
        let table_hash = sha256(&self.buf);
        self.dev.write(table, &self.buf)?;
        // 3. Que los datos y la tabla lleguen antes que el superbloque.
        self.dev.flush()?;
        // 4. Superbloque en la otra ranura, con secuencia + 1.
        let sb = Super {
            seq: self.seq + 1,
            table,
            table_hash,
        };
        encode_super(&sb, &mut self.buf);
        self.dev.write(other, &self.buf)?;
        self.dev.flush()?;
        // 5. Confirmado.
        self.slot = other;
        self.seq += 1;
        self.table = table;
        self.entries = next;
        self.count = n;
        self.stale_other = false;
        Ok(entry.version)
    }

    /// El dispositivo (la cell lo usa para su prueba de caídas).
    pub fn dev_mut(&mut self) -> &mut D {
        &mut self.dev
    }

    /// Devuelve el dispositivo (para los tests).
    pub fn into_dev(self) -> D {
        self.dev
    }
}

#[cfg(test)]
mod tests;
