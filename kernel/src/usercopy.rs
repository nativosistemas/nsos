//! Copias entre memoria de una cell y el kernel.
//!
//! Nunca se desreferencia un puntero de usuario directamente: cada página se
//! traduce con las tablas de la cell, se exige el bit USER (y WRITABLE para
//! escribir) en todos los niveles, y se copia a través del HHDM. Así una
//! cell no puede hacer que el kernel lea o escriba memoria del kernel, ni
//! provocar un fallo de página dentro del kernel.
//!
//! Spectre v1: primero se comprueban los permisos de todas las páginas y
//! después de una barrera de especulación se copia, así una predicción
//! equivocada de un control no copia (ni siquiera de forma especulativa)
//! una página que no es de la cell.

use crate::arch::paging::{flags, PAGE_SIZE};
use crate::cell::Cell;
use crate::mm::phys_to_virt;
use nsos_abi::Error;

/// Tope (excluido) de direcciones de usuario.
pub const USER_END: u64 = 0x0000_8000_0000_0000;

fn check_range(addr: u64, len: usize) -> Result<(), Error> {
    let end = addr.checked_add(len as u64).ok_or(Error::InvalidArgs)?;
    if end > USER_END {
        return Err(Error::InvalidArgs);
    }
    Ok(())
}

fn for_each_page(
    cell: &Cell,
    addr: u64,
    len: usize,
    write: bool,
    mut f: impl FnMut(u64 /*kvirt*/, usize /*off*/, usize /*n*/),
) -> Result<(), Error> {
    check_range(addr, len)?;
    let need = flags::USER | if write { flags::WRITABLE } else { 0 };
    cell.with_space(|space| {
        let mut done = 0usize;
        while done < len {
            let va = addr + done as u64;
            let (pa, fl) = space.translate_with_flags(va).ok_or(Error::InvalidArgs)?;
            if fl & need != need {
                return Err(Error::InvalidArgs);
            }
            let n = ((PAGE_SIZE - (va % PAGE_SIZE)) as usize).min(len - done);
            f(phys_to_virt(pa), done, n);
            done += n;
        }
        Ok(())
    })
    .unwrap_or(Err(Error::InvalidArgs))
}

/// Copia `out.len()` bytes desde la cell.
pub fn copy_from_user(cell: &Cell, addr: u64, out: &mut [u8]) -> Result<(), Error> {
    // Validar todo antes de copiar nada.
    for_each_page(cell, addr, out.len(), false, |_, _, _| {})?;
    crate::arch::cpu::speculation_barrier();
    for_each_page(cell, addr, out.len(), false, |k, off, n| {
        // SAFETY: `k` es la dirección HHDM de una página de usuario presente;
        // `out[off..off+n]` está en rango.
        unsafe { core::ptr::copy_nonoverlapping(k as *const u8, out[off..].as_mut_ptr(), n) };
    })
}

/// Copia `data` hacia la cell.
pub fn copy_to_user(cell: &Cell, addr: u64, data: &[u8]) -> Result<(), Error> {
    for_each_page(cell, addr, data.len(), true, |_, _, _| {})?;
    crate::arch::cpu::speculation_barrier();
    for_each_page(cell, addr, data.len(), true, |k, off, n| {
        // SAFETY: página de usuario presente y escribible, vía HHDM.
        unsafe { core::ptr::copy_nonoverlapping(data[off..].as_ptr(), k as *mut u8, n) };
    })
}
