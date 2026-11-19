//! Validación de imágenes ELF64 de cells.
//!
//! El kernel nunca confía en el ELF: este módulo comprueba cabecera y
//! segmentos y devuelve una descripción ya validada que el cargador solo
//! tiene que copiar y mapear. Reglas:
//!
//! * ELF64, little-endian, x86_64, ejecutable estático (`ET_EXEC`).
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

/// Error de validación.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    /// Archivo demasiado chico o tabla fuera del archivo.
    Truncated,
    /// No es ELF64 LE x86_64 ejecutable.
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
        b.get(o..o + 2)
            .ok_or(ElfError::Truncated)?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, ElfError> {
    Ok(u32::from_le_bytes(
        b.get(o..o + 4)
            .ok_or(ElfError::Truncated)?
            .try_into()
            .unwrap(),
    ))
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, ElfError> {
    Ok(u64::from_le_bytes(
        b.get(o..o + 8)
            .ok_or(ElfError::Truncated)?
            .try_into()
            .unwrap(),
    ))
}

/// Valida un ELF y describe cómo cargarlo.
pub fn parse(b: &[u8]) -> Result<Image, ElfError> {
    if b.len() < 64 {
        return Err(ElfError::Truncated);
    }
    // e_ident: magia, clase 64, little-endian, versión 1
    if b[0..4] != *b"\x7fELF" || b[4] != 2 || b[5] != 1 || b[6] != 1 {
        return Err(ElfError::BadHeader);
    }
    let e_type = u16_at(b, 16)?;
    let e_machine = u16_at(b, 18)?;
    if e_type != 2 || e_machine != 0x3E {
        return Err(ElfError::BadHeader);
    }
    let entry = u64_at(b, 24)?;
    let phoff = u64_at(b, 32)? as usize;
    let phentsize = u16_at(b, 54)? as usize;
    let phnum = u16_at(b, 56)? as usize;
    if phentsize < 56 {
        return Err(ElfError::BadHeader);
    }
    let table_end = phnum
        .checked_mul(phentsize)
        .and_then(|n| n.checked_add(phoff))
        .ok_or(ElfError::Truncated)?;
    if table_end > b.len() {
        return Err(ElfError::Truncated);
    }

    let mut segments: Vec<Segment> = Vec::new();
    for i in 0..phnum {
        let o = phoff + i * phentsize;
        if u32_at(b, o)? != 1 {
            continue; // solo PT_LOAD
        }
        let flags = u32_at(b, o + 4)?;
        let offset = u64_at(b, o + 8)?;
        let vaddr = u64_at(b, o + 16)?;
        let filesz = u64_at(b, o + 32)?;
        let memsz = u64_at(b, o + 40)?;
        if memsz == 0 {
            continue;
        }
        if segments.len() == MAX_SEGMENTS {
            return Err(ElfError::TooManySegments);
        }
        let end = vaddr.checked_add(memsz).ok_or(ElfError::BadSegment)?;
        // El desplazamiento dentro de la página debe coincidir en archivo y
        // memoria; normalizamos ambos a inicio de página.
        if vaddr % PAGE != offset % PAGE || filesz > memsz || vaddr < USER_MIN || end > USER_MAX {
            return Err(ElfError::BadSegment);
        }
        let file_end = offset.checked_add(filesz).ok_or(ElfError::Truncated)?;
        if file_end > b.len() as u64 {
            return Err(ElfError::Truncated);
        }
        let write = flags & 2 != 0;
        let exec = flags & 1 != 0;
        if write && exec {
            return Err(ElfError::WriteExec);
        }
        let lead = vaddr % PAGE;
        let seg = Segment {
            vaddr: vaddr - lead,
            memsz: (memsz + lead).div_ceil(PAGE) * PAGE,
            offset: offset - lead,
            filesz: filesz + lead,
            write,
            exec,
        };
        for s in &segments {
            if seg.vaddr < s.vaddr + s.memsz && s.vaddr < seg.vaddr + seg.memsz {
                return Err(ElfError::Overlap);
            }
        }
        segments.push(seg);
    }

    if !segments
        .iter()
        .any(|s| s.exec && entry >= s.vaddr && entry < s.vaddr + s.memsz)
    {
        return Err(ElfError::BadEntry);
    }
    Ok(Image { entry, segments })
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
        for _ in 0..5000 {
            let mut b = base.clone();
            for _ in 0..8 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let pos = (seed as usize) % 200;
                b[pos] = (seed >> 32) as u8;
            }
            if let Ok(img) = parse(&b) {
                for s in img.segments {
                    assert!(!(s.write && s.exec));
                    assert!(s.vaddr >= USER_MIN && s.vaddr + s.memsz <= USER_MAX);
                }
            }
        }
    }
}
