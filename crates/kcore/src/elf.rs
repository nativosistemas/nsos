//! Validación de imágenes ELF64 de cells.
//!
//! El kernel nunca confía en el ELF: este módulo comprueba cabecera y
//! segmentos y devuelve una descripción ya validada que el cargador solo
//! tiene que copiar y mapear. Reglas:
//!
//! * ELF64, little-endian, de la arquitectura del kernel ([`MACHINE`]),
//!   ejecutable estático (`ET_EXEC`).
//! * Cada `PT_LOAD` vive en la mitad baja de usuario, por encima de
//!   [`USER_MIN`], alineado a página, con `filesz <= memsz` y datos dentro
//!   del archivo.
//! * Ningún segmento es escribible y ejecutable a la vez (W^X).
//! * Los segmentos no se solapan.
//! * El punto de entrada cae dentro de un segmento ejecutable.

use alloc::vec::Vec;

/// Dirección mínima de usuario (la página 0 y las primeras quedan sin mapear
/// para atrapar punteros nulos).
pub const USER_MIN: u64 = 0x10_0000;
/// Tope (excluido) del espacio de usuario.
pub const USER_MAX: u64 = 0x0000_7fff_ffff_0000;
/// Máximo de segmentos cargables aceptados.
pub const MAX_SEGMENTS: usize = 16;
const PAGE: u64 = 4096;

/// Arquitectura de las cells que acepta este kernel (`e_machine`):
/// `EM_AARCH64` en ARM64, `EM_X86_64` en el resto (incluidos los tests del
/// host).
#[cfg(target_arch = "aarch64")]
pub const MACHINE: u16 = 183;
/// Arquitectura de las cells que acepta este kernel (`e_machine`):
/// `EM_AARCH64` en ARM64, `EM_X86_64` en el resto (incluidos los tests del
/// host).
#[cfg(not(target_arch = "aarch64"))]
pub const MACHINE: u16 = 0x3E;

/// Error de validación.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    /// Archivo demasiado chico o tabla fuera del archivo.
    Truncated,
    /// No es ELF64 LE ejecutable de [`MACHINE`].
    BadHeader,
    /// Segmento fuera del rango de usuario o desalineado.
    BadSegment,
    /// Segmento W+X.
    WriteExec,
    /// Segmentos solapados.
    Overlap,
    /// Entrada fuera de un segmento ejecutable.
    BadEntry,
    /// Demasiados segmentos.
    TooManySegments,
}

/// Segmento cargable validado.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Dirección virtual (alineada a página).
    pub vaddr: u64,
    /// Tamaño en memoria.
    pub memsz: u64,
    /// Desplazamiento en el archivo.
    pub offset: u64,
    /// Bytes a copiar desde el archivo (el resto es cero).
    pub filesz: u64,
    /// Escribible.
    pub write: bool,
    /// Ejecutable.
    pub exec: bool,
}

/// Imagen validada.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Punto de entrada.
    pub entry: u64,
    /// Segmentos a cargar.
    pub segments: Vec<Segment>,
}

fn u16_at(b: &[u8], o: usize) -> Result<u16, ElfError> {
    Ok(u16::from_le_bytes(
        b.get(o..o.checked_add(2).ok_or(ElfError::Truncated)?)
            .ok_or(ElfError::Truncated)?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, ElfError> {
    Ok(u32::from_le_bytes(
        b.get(o..o.checked_add(4).ok_or(ElfError::Truncated)?)
            .ok_or(ElfError::Truncated)?
            .try_into()
            .unwrap(),
    ))
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, ElfError> {
    Ok(u64::from_le_bytes(
        b.get(o..o.checked_add(8).ok_or(ElfError::Truncated)?)
            .ok_or(ElfError::Truncated)?
            .try_into()
            .unwrap(),
    ))
}

/// Cabecera validada: dónde está la tabla de cabeceras de programa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    entry: u64,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

/// Valida la cabecera. Si la acepta, la tabla de cabeceras de programa
/// entra entera en el archivo y cada entrada tiene al menos 56 bytes, así
/// que leer los campos de cualquier entrada nunca se sale de `b`.
fn header(b: &[u8]) -> Result<Header, ElfError> {
    if b.len() < 64 {
        return Err(ElfError::Truncated);
    }
    // e_ident: magia, clase 64, little-endian, versión 1
    if b[0..4] != *b"\x7fELF" || b[4] != 2 || b[5] != 1 || b[6] != 1 {
        return Err(ElfError::BadHeader);
    }
    let e_type = u16_at(b, 16)?;
    let e_machine = u16_at(b, 18)?;
    if e_type != 2 || e_machine != MACHINE {
        return Err(ElfError::BadHeader);
    }
    let entry = u64_at(b, 24)?;
    let phoff = u64_at(b, 32)?;
    let phentsize = u16_at(b, 54)? as usize;
    let phnum = u16_at(b, 56)? as usize;
    if phentsize < 56 {
        return Err(ElfError::BadHeader);
    }
    table(phoff, phentsize, phnum, b.len() as u64)?;
    Ok(Header {
        entry,
        phoff: phoff as usize,
        phentsize,
        phnum,
    })
}

/// La tabla de cabeceras de programa entra entera en un archivo de
/// `file_len` bytes. Se cuenta en 64 bits: en un objetivo de 32 bits,
/// convertir `phoff` antes a `usize` podría truncarlo.
fn table(phoff: u64, phentsize: usize, phnum: usize, file_len: u64) -> Result<(), ElfError> {
    let table_end = (phnum as u64)
        .checked_mul(phentsize as u64)
        .and_then(|n| n.checked_add(phoff))
        .ok_or(ElfError::Truncated)?;
    if table_end > file_len {
        return Err(ElfError::Truncated);
    }
    Ok(())
}

/// Valida un `PT_LOAD` (sus campos tal como vienen en el archivo) y lo
/// normaliza a páginas enteras. `Ok(None)` si está vacío (`memsz == 0`).
fn segment(
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
    file_len: u64,
) -> Result<Option<Segment>, ElfError> {
    if memsz == 0 {
        return Ok(None);
    }
    let end = vaddr.checked_add(memsz).ok_or(ElfError::BadSegment)?;
    // El desplazamiento dentro de la página debe coincidir en archivo y
    // memoria; normalizamos ambos a inicio de página.
    if vaddr % PAGE != offset % PAGE || filesz > memsz || vaddr < USER_MIN || end > USER_MAX {
        return Err(ElfError::BadSegment);
    }
    let file_end = offset.checked_add(filesz).ok_or(ElfError::Truncated)?;
    if file_end > file_len {
        return Err(ElfError::Truncated);
    }
    let write = flags & 2 != 0;
    let exec = flags & 1 != 0;
    if write && exec {
        return Err(ElfError::WriteExec);
    }
    let lead = vaddr % PAGE;
    Ok(Some(Segment {
        vaddr: vaddr - lead,
        memsz: (memsz + lead).div_ceil(PAGE) * PAGE,
        offset: offset - lead,
        filesz: filesz + lead,
        write,
        exec,
    }))
}

/// Agrega `seg` si no se solapa con ninguno de los anteriores.
fn push_disjoint(segments: &mut Vec<Segment>, seg: Segment) -> Result<(), ElfError> {
    if segments.len() == MAX_SEGMENTS {
        return Err(ElfError::TooManySegments);
    }
    if overlaps_any(segments, &seg) {
        return Err(ElfError::Overlap);
    }
    segments.push(seg);
    Ok(())
}

/// `seg` comparte alguna dirección con alguno de `segments`.
fn overlaps_any(segments: &[Segment], seg: &Segment) -> bool {
    segments
        .iter()
        .any(|s| seg.vaddr < s.vaddr + s.memsz && s.vaddr < seg.vaddr + seg.memsz)
}

/// El punto de entrada cae dentro de un segmento ejecutable.
fn check_entry(segments: &[Segment], entry: u64) -> Result<(), ElfError> {
    if segments
        .iter()
        .any(|s| s.exec && entry >= s.vaddr && entry < s.vaddr + s.memsz)
    {
        Ok(())
    } else {
        Err(ElfError::BadEntry)
    }
}

/// Valida un ELF y describe cómo cargarlo.
pub fn parse(b: &[u8]) -> Result<Image, ElfError> {
    let h = header(b)?;
    let mut segments: Vec<Segment> = Vec::new();
    for i in 0..h.phnum {
        // `header` ya garantiza que la tabla entra en el archivo; igual se
        // cuenta con aritmética comprobada para que leer nunca entre en
        // pánico, sea cual sea la cabecera.
        let e = i
            .checked_mul(h.phentsize)
            .and_then(|n| n.checked_add(h.phoff))
            .and_then(|o| b.get(o..))
            .ok_or(ElfError::Truncated)?;
        if u32_at(e, 0)? != 1 {
            continue; // solo PT_LOAD
        }
        let seg = segment(
            u32_at(e, 4)?,
            u64_at(e, 8)?,
            u64_at(e, 16)?,
            u64_at(e, 32)?,
            u64_at(e, 40)?,
            b.len() as u64,
        )?;
        if let Some(seg) = seg {
            push_disjoint(&mut segments, seg)?;
        }
    }
    check_entry(&segments, h.entry)?;
    Ok(Image {
        entry: h.entry,
        segments,
    })
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Un segmento cualquiera de los que puede devolver `segment`: en páginas
    /// enteras y dentro del espacio de usuario.
    fn any_valid_segment() -> Segment {
        let s = Segment {
            vaddr: kani::any(),
            memsz: kani::any(),
            offset: kani::any(),
            filesz: kani::any(),
            write: kani::any(),
            exec: kani::any(),
        };
        kani::assume(s.vaddr % PAGE == 0 && s.memsz % PAGE == 0 && s.memsz > 0);
        kani::assume(s.vaddr >= USER_MIN && s.vaddr <= USER_MAX && s.memsz <= USER_MAX - s.vaddr);
        s
    }

    fn disjoint(a: &Segment, b: &Segment) -> bool {
        a.vaddr + a.memsz <= b.vaddr || b.vaddr + b.memsz <= a.vaddr
    }

    /// Con cualquier cabecera y cualquier largo de archivo: sin pánicos ni
    /// desbordes.
    #[kani::proof]
    #[kani::unwind(5)]
    fn header_any() {
        let bytes: [u8; 64] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= bytes.len());
        let _ = header(&bytes[..len]);
    }

    /// Con cualesquiera campos de un `PT_LOAD` y cualquier largo de archivo:
    /// sin pánicos ni desbordes; si se acepta, el segmento cumple lo que el
    /// cargador (`spawn` en `kernel/src/cell.rs`) da por cierto para mapearlo
    /// y copiarlo sin más controles.
    #[kani::proof]
    fn segment_any() {
        let (flags, offset, vaddr, filesz, memsz, file_len): (u32, u64, u64, u64, u64, u64) = (
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
        );
        let Ok(Some(s)) = segment(flags, offset, vaddr, filesz, memsz, file_len) else {
            return;
        };
        // Páginas enteras: ninguna página del segmento es de otro.
        assert!(s.vaddr % PAGE == 0 && s.memsz % PAGE == 0 && s.memsz > 0);
        assert!(s.offset % PAGE == 0);
        // Dentro del espacio de usuario, sin la página nula ni la mitad del
        // kernel, y sin desbordar al sumar.
        assert!(s.vaddr >= USER_MIN);
        assert!(s.vaddr.checked_add(s.memsz).is_some_and(|e| e <= USER_MAX));
        // Lo que se copia sale del archivo y entra en el segmento.
        assert!(s.filesz <= s.memsz);
        assert!(s
            .offset
            .checked_add(s.filesz)
            .is_some_and(|e| e <= file_len));
        // W^X, y los permisos son los que pidió el ELF.
        assert!(!(s.write && s.exec));
        assert!(s.write == (flags & 2 != 0) && s.exec == (flags & 1 != 0));
        // La normalización no mueve nada: cubre lo que pidió el ELF y cada
        // byte del archivo cae en la dirección que pidió.
        assert!(s.vaddr <= vaddr && vaddr + memsz <= s.vaddr + s.memsz);
        assert!(vaddr - s.vaddr == offset - s.offset);
        assert!(s.offset + s.filesz == offset + filesz);
    }

    /// Con cualquier lista de hasta dos segmentos válidos y cualquier
    /// segmento válido nuevo: si `overlaps_any` dice que no se solapa, es
    /// disjunto de cada uno. `push_disjoint` solo agrega con esa respuesta,
    /// así que, por inducción, los segmentos de toda imagen aceptada son
    /// disjuntos: ninguna página se mapea dos veces.
    #[kani::proof]
    #[kani::unwind(4)]
    fn overlaps_any_sound() {
        let segments = [any_valid_segment(), any_valid_segment()];
        let n: usize = kani::any();
        kani::assume(n <= 2);
        let c = any_valid_segment();
        if !overlaps_any(&segments[..n], &c) {
            let i: usize = kani::any();
            kani::assume(i < n);
            assert!(disjoint(&segments[i], &c));
        }
    }

    /// Si `check_entry` acepta, la entrada cae en un segmento ejecutable.
    #[kani::proof]
    #[kani::unwind(4)]
    fn check_entry_any() {
        let segments = [any_valid_segment(), any_valid_segment()];
        let n: usize = kani::any();
        kani::assume(n <= 2);
        let entry: u64 = kani::any();
        if check_entry(&segments[..n], entry).is_ok() {
            let i: usize = kani::any();
            kani::assume(i < n);
            let s = segments[i];
            kani::cover!(s.exec && s.vaddr <= entry && entry < s.vaddr + s.memsz);
            assert!(segments[..n]
                .iter()
                .any(|s| s.exec && s.vaddr <= entry && entry < s.vaddr + s.memsz));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Construye un ELF mínimo con los PT_LOAD dados: (vaddr, offset, filesz, memsz, flags).
    fn build(entry: u64, segs: &[(u64, u64, u64, u64, u32)], total: usize) -> Vec<u8> {
        let mut b = vec![0u8; total.max(64 + 56 * segs.len())];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[6] = 1;
        b[16..18].copy_from_slice(&2u16.to_le_bytes());
        b[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        b[24..32].copy_from_slice(&entry.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&(segs.len() as u16).to_le_bytes());
        for (i, &(v, off, fs, ms, fl)) in segs.iter().enumerate() {
            let o = 64 + i * 56;
            b[o..o + 4].copy_from_slice(&1u32.to_le_bytes());
            b[o + 4..o + 8].copy_from_slice(&fl.to_le_bytes());
            b[o + 8..o + 16].copy_from_slice(&off.to_le_bytes());
            b[o + 16..o + 24].copy_from_slice(&v.to_le_bytes());
            b[o + 32..o + 40].copy_from_slice(&fs.to_le_bytes());
            b[o + 40..o + 48].copy_from_slice(&ms.to_le_bytes());
        }
        b
    }

    const RX: u32 = 5;
    const RW: u32 = 6;

    #[test]
    fn valid_image() {
        let b = build(
            0x40_1000,
            &[
                (0x40_1000, 0x1000, 0x100, 0x100, RX),
                (0x40_2000, 0x2000, 0x10, 0x3000, RW),
            ],
            0x3000,
        );
        let img = parse(&b).unwrap();
        assert_eq!(img.entry, 0x40_1000);
        assert_eq!(img.segments.len(), 2);
        assert_eq!(img.segments[1].memsz, 0x3000);
        assert!(img.segments[1].write && !img.segments[1].exec);
    }

    #[test]
    fn unaligned_segment_is_normalized() {
        let b = build(0x40_1010, &[(0x40_1010, 0x1010, 0x20, 0x20, RX)], 0x2000);
        let s = parse(&b).unwrap().segments[0];
        assert_eq!(
            (s.vaddr, s.offset, s.filesz, s.memsz),
            (0x40_1000, 0x1000, 0x30, 0x1000)
        );
    }

    #[test]
    fn rejects_bad_header() {
        let mut b = build(0x40_1000, &[(0x40_1000, 0x1000, 1, 1, RX)], 0x2000);
        b[18] = 0xB7; // AArch64
        assert_eq!(parse(&b), Err(ElfError::BadHeader));
        assert_eq!(parse(&b[..10]), Err(ElfError::Truncated));
    }

    #[test]
    fn rejects_kernel_addresses_and_null_page() {
        let b = build(
            0xffff_ffff_8000_0000,
            &[(0xffff_ffff_8000_0000, 0x1000, 1, 1, RX)],
            0x2000,
        );
        assert_eq!(parse(&b), Err(ElfError::BadSegment));
        let b = build(0x1000, &[(0x1000, 0x1000, 1, 1, RX)], 0x2000);
        assert_eq!(parse(&b), Err(ElfError::BadSegment));
    }

    #[test]
    fn rejects_wx_overlap_entry_and_truncation() {
        let b = build(0x40_1000, &[(0x40_1000, 0x1000, 1, 1, 7)], 0x2000);
        assert_eq!(parse(&b), Err(ElfError::WriteExec));
        let b = build(
            0x40_1000,
            &[
                (0x40_1000, 0x1000, 1, 0x2000, RX),
                (0x40_2000, 0x1000, 1, 1, RW),
            ],
            0x2000,
        );
        assert_eq!(parse(&b), Err(ElfError::Overlap));
        let b = build(0x50_0000, &[(0x40_1000, 0x1000, 1, 1, RX)], 0x2000);
        assert_eq!(parse(&b), Err(ElfError::BadEntry));
        let b = build(
            0x40_1000,
            &[(0x40_1000, 0x1000, 0x5000, 0x5000, RX)],
            0x2000,
        );
        assert_eq!(parse(&b), Err(ElfError::Truncated));
        // tabla de program headers fuera del archivo
        let mut b = build(0x40_1000, &[(0x40_1000, 0x1000, 1, 1, RX)], 0x2000);
        b[56..58].copy_from_slice(&1000u16.to_le_bytes());
        assert_eq!(parse(&b), Err(ElfError::Truncated));
    }

    /// Ninguna entrada arbitraria hace entrar en pánico al parser.
    #[test]
    fn pseudo_fuzz_never_panics() {
        let base = build(
            0x40_1000,
            &[
                (0x40_1000, 0x1000, 0x100, 0x100, RX),
                (0x40_2000, 0x2000, 0x10, 0x3000, RW),
            ],
            0x3000,
        );
        let mut seed = 0xDEADBEEFu64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // Cabecera ELF (64) + 2 cabeceras de programa (2 × 56).
        let headers = 64 + 2 * 56;
        let mut accepted = 0;
        for i in 0..50_000 {
            let mut b = base.clone();
            for _ in 0..1 + i % 8 {
                let r = next();
                let pos = (r as usize) % headers;
                b[pos] = (r >> 32) as u8;
            }
            if let Ok(img) = parse(&b) {
                accepted += 1;
                // Lo que el parser acepta cumple todas las reglas de carga.
                for (k, s) in img.segments.iter().enumerate() {
                    assert!(!(s.write && s.exec), "segmento W+X");
                    assert!(s.vaddr >= USER_MIN, "segmento bajo USER_MIN");
                    assert!(
                        s.vaddr.checked_add(s.memsz).is_some_and(|e| e <= USER_MAX),
                        "segmento fuera del espacio de usuario"
                    );
                    assert!(s.filesz <= s.memsz);
                    assert!(
                        (s.offset as usize)
                            .checked_add(s.filesz as usize)
                            .is_some_and(|e| e <= b.len()),
                        "segmento fuera del archivo"
                    );
                    for t in &img.segments[k + 1..] {
                        let disjoint = s.vaddr + s.memsz <= t.vaddr || t.vaddr + t.memsz <= s.vaddr;
                        assert!(disjoint, "segmentos solapados");
                    }
                }
                assert!(
                    img.segments
                        .iter()
                        .any(|s| s.exec && s.vaddr <= img.entry && img.entry < s.vaddr + s.memsz),
                    "entrada fuera de un segmento ejecutable"
                );
            }
        }
        assert!(accepted > 0);
    }
}
