//! Allocador de marcos físicos del kernel.
//!
//! Envuelve el `FrameBitmap` de `nsos-kcore` (lógica pura, testeada en el
//! host) con un lock y lo inicializa desde el mapa de memoria de Limine.

use super::{phys_to_virt, PhysAddr};
use crate::boot::limine::{MemKind, Memmap};
use crate::sync::IrqSpinLock;
use nsos_kcore::frame_bitmap::{FrameBitmap, FRAME_SIZE};

static PMM: IrqSpinLock<Option<FrameBitmap<'static>>> = IrqSpinLock::new(None);

/// Estadísticas del allocador.
#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub total_frames: usize,
    pub free_frames: usize,
}

/// Inicializa el allocador. Coloca el bitmap en la primera región utilizable
/// con espacio suficiente.
pub fn init(memmap: &Memmap) {
    let top = memmap
        .entries()
        .filter(|e| matches!(e.kind(), MemKind::Usable | MemKind::BootloaderReclaimable))
        .map(|e| e.base + e.length)
        .max()
        .expect("no hay memoria utilizable");
    let nframes = (top / FRAME_SIZE) as usize;
    let words = FrameBitmap::words_for(nframes);
    let bytes = (words * 8) as u64;

    let host = memmap
        .entries()
        .find(|e| e.kind() == MemKind::Usable && e.length >= bytes && e.base >= 0x10_0000)
        .expect("no hay región para el bitmap de marcos");

    // SAFETY: `host` es memoria Usable según el bootloader, nadie más la usa
    // todavía, está mapeada por el HHDM y la reservamos en el bitmap justo
    // abajo, así que nunca se entregará como marco.
    let storage: &'static mut [u64] =
        unsafe { core::slice::from_raw_parts_mut(phys_to_virt(host.base) as *mut u64, words) };

    let mut bm = FrameBitmap::new(storage, nframes);
    for e in memmap.entries() {
        if e.kind() == MemKind::Usable {
            bm.release_region(e.base, e.length);
        }
    }
    bm.reserve_region(host.base, bytes);
    bm.reserve_region(0, FRAME_SIZE); // el marco 0 nunca se entrega
    *PMM.lock() = Some(bm);
}

/// Devuelve al allocador la memoria del bootloader. Solo es seguro cuando ya
/// no se usa ninguna estructura de Limine (ni su pila ni sus tablas).
#[allow(dead_code)]
pub fn reclaim_bootloader(memmap: &Memmap) {
    let mut g = PMM.lock();
    let bm = g.as_mut().expect("pmm sin inicializar");
    for e in memmap.entries() {
        if e.kind() == MemKind::BootloaderReclaimable {
            bm.release_region(e.base, e.length);
        }
    }
}

/// Reserva un marco puesto a cero.
pub fn alloc_frame() -> Option<PhysAddr> {
    let f = PMM.lock().as_mut()?.alloc()?;
    let phys = f as u64 * FRAME_SIZE;
    zero(phys, 1);
    Some(phys)
}

/// Reserva `n` marcos contiguos (sin poner a cero).
pub fn alloc_contiguous(n: usize, align_frames: usize) -> Option<PhysAddr> {
    let f = PMM.lock().as_mut()?.alloc_contiguous(n, align_frames)?;
    Some(f as u64 * FRAME_SIZE)
}

/// Libera un marco.
pub fn free_frame(phys: PhysAddr) {
    let r = PMM
        .lock()
        .as_mut()
        .expect("pmm sin inicializar")
        .free_frame((phys / FRAME_SIZE) as usize);
    if let Err(e) = r {
        panic!("free_frame({phys:#x}): {e:?}");
    }
}

/// Estadísticas actuales.
pub fn stats() -> Stats {
    let g = PMM.lock();
    let bm = g.as_ref().expect("pmm sin inicializar");
    Stats {
        total_frames: bm.total(),
        free_frames: bm.free_count(),
    }
}

fn zero(phys: PhysAddr, frames: usize) {
    // SAFETY: el marco acaba de salir del allocador: es nuestro y está
    // mapeado por el HHDM.
    unsafe {
        core::ptr::write_bytes(
            phys_to_virt(phys) as *mut u8,
            0,
            frames * FRAME_SIZE as usize,
        )
    };
}
