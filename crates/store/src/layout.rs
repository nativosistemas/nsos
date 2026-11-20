//! Formato en disco: superbloques, tabla de objetos y entradas.
//!
//! Todos los enteros van en little-endian. Bloques de [`BLOCK`] bytes.
//!
//! ```text
//! superbloque (bloques 0 y 1):
//!   0..8   "NSOSALM1"          8..12  versión de formato (1)
//!   16..24 secuencia (u64)     24..28 bloque de la tabla (u32)
//!   32..64 SHA-256 de la tabla 64..96 SHA-256 de los bytes 0..64
//! tabla:
//!   0..8   "NSOSTAB1"          8..12  cantidad de entradas (u32)
//!   16..   entradas de 128 bytes
//! entrada:
//!   0      largo del nombre    1..32  nombre (resto a cero)
//!   32..36 dueño               36..40 largo de los datos en bytes
//!   40..48 versión             48..52 primer bloque de datos
//!   52..56 cantidad de bloques 56..88 SHA-256 de los datos
//! ```
//!
//! Los decodificadores no confían en nada: el disco puede tener cualquier
//! cosa (Kani: `decode_entry_any`, `decode_super_header_any`).

use crate::{BLOCK, MAX_OBJECTS, MAX_OBJECT_BLOCKS, NAME_MAX};
use nsos_link::sha256::sha256;

pub const SUPER_MAGIC: [u8; 8] = *b"NSOSALM1";
pub const TABLE_MAGIC: [u8; 8] = *b"NSOSTAB1";
pub const FORMAT_VERSION: u32 = 1;
pub const ENTRY_SIZE: usize = 128;
pub const TABLE_HDR: usize = 16;
const SUPER_BODY: usize = 64;

/// Primer bloque que puede tener tablas o datos (0 y 1 son las ranuras).
pub const FIRST_DATA_BLOCK: u32 = 2;

fn u32_at(b: &[u8], off: usize) -> u32 {
    let mut v = [0u8; 4];
    v.copy_from_slice(&b[off..off + 4]);
    u32::from_le_bytes(v)
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(v)
}

/// Nombre de un objeto: de 1 a [`NAME_MAX`] bytes, sin ceros.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Name {
    len: u8,
    bytes: [u8; NAME_MAX],
}

impl Name {
    /// `None` si está vacío, es muy largo o tiene un byte cero.
    pub fn new(s: &[u8]) -> Option<Name> {
        if s.is_empty() || s.len() > NAME_MAX || s.contains(&0) {
            return None;
        }
        let mut bytes = [0u8; NAME_MAX];
        bytes[..s.len()].copy_from_slice(s);
        Some(Name {
            len: s.len() as u8,
            bytes,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

/// Superbloque (lo que dice una ranura).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Super {
    pub seq: u64,
    pub table: u32,
    pub table_hash: [u8; 32],
}

pub fn encode_super(s: &Super, out: &mut [u8; BLOCK]) {
    out.fill(0);
    out[0..8].copy_from_slice(&SUPER_MAGIC);
    out[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    out[16..24].copy_from_slice(&s.seq.to_le_bytes());
    out[24..28].copy_from_slice(&s.table.to_le_bytes());
    out[32..64].copy_from_slice(&s.table_hash);
    let h = sha256(&out[..SUPER_BODY]);
    out[SUPER_BODY..SUPER_BODY + 32].copy_from_slice(&h);
}

/// Campos de un superbloque, sin comprobar su hash (lo hace
/// [`decode_super`]). Exige la marca, la versión y que la tabla no sea una
/// ranura ni quede fuera del disco.
pub fn decode_super_header(b: &[u8; BLOCK], disk_blocks: u32) -> Option<Super> {
    if b[0..8] != SUPER_MAGIC || u32_at(b, 8) != FORMAT_VERSION {
        return None;
    }
    let table = u32_at(b, 24);
    if table < FIRST_DATA_BLOCK || table >= disk_blocks {
        return None;
    }
    let mut table_hash = [0u8; 32];
    table_hash.copy_from_slice(&b[32..64]);
    Some(Super {
        seq: u64_at(b, 16),
        table,
        table_hash,
    })
}

/// Superbloque si pasa su propio hash. Si no, la ranura es "basura" (en el
/// modelo de Lean, `Block.junk`).
pub fn decode_super(b: &[u8; BLOCK], disk_blocks: u32) -> Option<Super> {
    let s = decode_super_header(b, disk_blocks)?;
    if sha256(&b[..SUPER_BODY])[..] != b[SUPER_BODY..SUPER_BODY + 32] {
        return None;
    }
    Some(s)
}

/// Qué hay en una ranura que no tiene un superbloque válido.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotKind {
    /// Todo a cero: disco nuevo.
    Zero,
    /// Empieza con nuestra marca (un superbloque escrito a medias).
    Ours,
    /// Otra cosa: el disco no es nuestro (FAT, una tabla de particiones...).
    Foreign,
}

pub fn classify(b: &[u8; BLOCK]) -> SlotKind {
    if b.iter().all(|&x| x == 0) {
        SlotKind::Zero
    } else if b[0..8] == SUPER_MAGIC {
        SlotKind::Ours
    } else {
        SlotKind::Foreign
    }
}

/// Entrada de la tabla: un objeto.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    pub owner: u32,
    pub name: Name,
    pub version: u64,
    /// Largo de los datos en bytes.
    pub len: u32,
    /// Primer bloque de datos (0 si el objeto está vacío).
    pub start: u32,
    pub nblocks: u32,
    pub hash: [u8; 32],
}

pub const EMPTY_ENTRY: Entry = Entry {
    owner: 0,
    name: Name {
        len: 0,
        bytes: [0; NAME_MAX],
    },
    version: 0,
    len: 0,
    start: 0,
    nblocks: 0,
    hash: [0; 32],
};

/// Bloques que ocupan `len` bytes.
pub fn blocks_for(len: u32) -> u32 {
    len.div_ceil(BLOCK as u32)
}

pub fn encode_entry(e: &Entry, out: &mut [u8]) {
    out[..ENTRY_SIZE].fill(0);
    out[0] = e.name.len;
    out[1..1 + NAME_MAX].copy_from_slice(&e.name.bytes);
    out[32..36].copy_from_slice(&e.owner.to_le_bytes());
    out[36..40].copy_from_slice(&e.len.to_le_bytes());
    out[40..48].copy_from_slice(&e.version.to_le_bytes());
    out[48..52].copy_from_slice(&e.start.to_le_bytes());
    out[52..56].copy_from_slice(&e.nblocks.to_le_bytes());
    out[56..88].copy_from_slice(&e.hash);
}

/// Decodifica una entrada y comprueba que sea coherente: nombre válido,
/// tamaño acotado, cantidad de bloques justa y datos dentro del disco y
/// fuera de las ranuras de superbloque.
pub fn decode_entry(b: &[u8], disk_blocks: u32) -> Option<Entry> {
    if b.len() < ENTRY_SIZE {
        return None;
    }
    let n = b[0] as usize;
    if n == 0
        || n > NAME_MAX
        || b[1..1 + n].contains(&0)
        || b[1 + n..1 + NAME_MAX].iter().any(|&x| x != 0)
    {
        return None;
    }
    let mut bytes = [0u8; NAME_MAX];
    bytes.copy_from_slice(&b[1..1 + NAME_MAX]);
    let len = u32_at(b, 36);
    let start = u32_at(b, 48);
    let nblocks = u32_at(b, 52);
    if len > MAX_OBJECT_BLOCKS * BLOCK as u32 || nblocks != blocks_for(len) {
        return None;
    }
    if nblocks == 0 {
        if start != 0 {
            return None;
        }
    } else if start < FIRST_DATA_BLOCK || start.checked_add(nblocks)? > disk_blocks {
        return None;
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&b[56..88]);
    Some(Entry {
        owner: u32_at(b, 32),
        name: Name {
            len: n as u8,
            bytes,
        },
        version: u64_at(b, 40),
        len,
        start,
        nblocks,
        hash,
    })
}

pub fn encode_table(entries: &[Entry], out: &mut [u8; BLOCK]) {
    out.fill(0);
    out[0..8].copy_from_slice(&TABLE_MAGIC);
    out[8..12].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    for (i, e) in entries.iter().enumerate() {
        let off = TABLE_HDR + i * ENTRY_SIZE;
        encode_entry(e, &mut out[off..off + ENTRY_SIZE]);
    }
}

/// Decodifica una tabla en `out`; devuelve cuántas entradas tiene. Rechaza
/// dos entradas con el mismo dueño y nombre.
pub fn decode_table(
    b: &[u8; BLOCK],
    disk_blocks: u32,
    out: &mut [Entry; MAX_OBJECTS],
) -> Option<usize> {
    if b[0..8] != TABLE_MAGIC {
        return None;
    }
    let count = u32_at(b, 8) as usize;
    if count > MAX_OBJECTS {
        return None;
    }
    for i in 0..count {
        let off = TABLE_HDR + i * ENTRY_SIZE;
        let e = decode_entry(&b[off..off + ENTRY_SIZE], disk_blocks)?;
        if out[..i]
            .iter()
            .any(|o| o.owner == e.owner && o.name == e.name)
        {
            return None;
        }
        out[i] = e;
    }
    Some(count)
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Cualquier entrada de 128 bytes y cualquier tamaño de disco: sin
    /// panics ni overflows; si se acepta, los datos quedan dentro del disco
    /// y fuera de las ranuras, y codificarla da los mismos bytes
    /// (canónica, salvo los bytes reservados).
    #[kani::proof]
    #[kani::unwind(90)]
    fn decode_entry_any() {
        let b: [u8; ENTRY_SIZE] = kani::any();
        let disk: u32 = kani::any();
        if let Some(e) = decode_entry(&b, disk) {
            assert!(e.len <= MAX_OBJECT_BLOCKS * BLOCK as u32);
            assert!(e.nblocks == blocks_for(e.len));
            if e.nblocks > 0 {
                assert!(e.start >= FIRST_DATA_BLOCK);
                assert!(e.start as u64 + e.nblocks as u64 <= disk as u64);
            }
            assert!(Name::new(e.name.as_bytes()) == Some(e.name));
            let mut again = [0u8; ENTRY_SIZE];
            encode_entry(&e, &mut again);
            assert!(again[..88] == b[..88]);
        }
    }
}
