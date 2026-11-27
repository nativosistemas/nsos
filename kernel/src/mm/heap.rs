//! Heap del kernel (`alloc`).
//!
//! Un bloque contiguo de marcos físicos, accedido por el HHDM, gestionado con
//! `linked_list_allocator`. Se dimensiona una vez al arrancar: una novena
//! parte de la memoria libre (al menos [`MIN_HEAP`]), así hay
//! `crate::quota::KMEM_PER_FRAME` bytes por cada marco que queda para las
//! cells. Lo que el kernel gasta por una cell se cobra a su cuota de memoria
//! del kernel (`crate::quota`), así que ninguna cell lo agota.

use super::{phys_to_virt, pmm};
use crate::arch::cpu;
use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;

/// Tamaño mínimo del heap del kernel.
pub const MIN_HEAP: usize = 8 * 1024 * 1024;

/// Tamaño del heap, fijado en [`init`].
static SIZE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// `LockedHeap` con interrupciones deshabilitadas durante cada operación,
/// para que un handler nunca encuentre el lock tomado por el hilo que
/// interrumpió.
struct IrqSafeHeap(LockedHeap);

// SAFETY: delega en `LockedHeap`, que cumple el contrato de `GlobalAlloc`;
// solo agregamos la sección sin interrupciones.
unsafe impl GlobalAlloc for IrqSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: mismo contrato que el llamador.
        cpu::without_interrupts(|| unsafe { self.0.alloc(layout) })
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: mismo contrato que el llamador.
        cpu::without_interrupts(|| unsafe { self.0.dealloc(ptr, layout) })
    }
}

#[global_allocator]
static HEAP: IrqSafeHeap = IrqSafeHeap(LockedHeap::empty());

/// Reserva la memoria del heap (una novena parte de los marcos libres, al
/// menos [`MIN_HEAP`]) y lo inicializa.
pub fn init() {
    let free = pmm::stats().free_frames;
    let frames = (free / 9).max(MIN_HEAP / 4096);
    let size = frames * 4096;
    let phys = pmm::alloc_contiguous(frames, 1).expect("sin memoria para el heap del kernel");
    // SAFETY: los marcos son nuestros (recién reservados), están mapeados
    // por el HHDM como RW y no se liberan nunca.
    unsafe {
        HEAP.0.lock().init(phys_to_virt(phys) as *mut u8, size);
    }
    SIZE.store(size, core::sync::atomic::Ordering::Relaxed);
}

/// Tamaño del heap en bytes.
pub fn size() -> usize {
    SIZE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Bytes en uso y libres.
pub fn usage() -> (usize, usize) {
    cpu::without_interrupts(|| {
        let h = HEAP.0.lock();
        (h.used(), h.free())
    })
}
