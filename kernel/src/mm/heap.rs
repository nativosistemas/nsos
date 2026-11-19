//! Heap del kernel (`alloc`).
//!
//! Un bloque contiguo de marcos físicos, accedido por el HHDM, gestionado con
//! `linked_list_allocator`. El tamaño es fijo y acotado a propósito: el kernel
//! no debería necesitar mucha memoria dinámica (la de las cells se contabiliza
//! aparte, como objetos `Memory`).

use super::{phys_to_virt, pmm};
use crate::arch::cpu;
use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;

/// Tamaño del heap del kernel.
pub const HEAP_SIZE: usize = 8 * 1024 * 1024;

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

/// Reserva la memoria del heap y lo inicializa.
pub fn init() {
    let frames = HEAP_SIZE / 4096;
    let phys = pmm::alloc_contiguous(frames, 1).expect("sin memoria para el heap del kernel");
    // SAFETY: los marcos son nuestros (recién reservados), están mapeados
    // por el HHDM como RW y no se liberan nunca.
    unsafe {
        HEAP.0.lock().init(phys_to_virt(phys) as *mut u8, HEAP_SIZE);
    }
}

/// Bytes en uso y libres.
pub fn usage() -> (usize, usize) {
    cpu::without_interrupts(|| {
        let h = HEAP.0.lock();
        (h.used(), h.free())
    })
}
