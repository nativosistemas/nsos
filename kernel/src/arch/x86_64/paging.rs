//! Tablas de páginas x86_64 de 4 niveles.
//!
//! Las tablas se acceden a través del mapeo directo de memoria física (HHDM),
//! así que este módulo no necesita mapeos recursivos.
//!
//! # Invariantes (`unsafe`)
//! - Toda dirección física de tabla proviene del allocador de marcos (marco
//!   propio, de 4 KiB, puesto a cero) o de una entrada presente de una tabla
//!   válida; por eso convertirla a `&mut [u64; 512]` vía HHDM es correcto.
//! - Un `AddressSpace` es dueño exclusivo de sus tablas intermedias; el
//!   acceso concurrente lo serializa quien lo contenga (un lock).

use crate::mm::{phys_to_virt, PhysAddr, VirtAddr};
use nsos_kcore::paging::{self as pt, Tables, ADDR_MASK};
pub use nsos_kcore::paging::{flags, MapError, PageSize, HUGE_PAGE_SIZE, PAGE_SIZE};

type Table = [u64; 512];

fn table_mut(phys: PhysAddr) -> &'static mut Table {
    // SAFETY: ver invariantes del módulo.
    unsafe { &mut *(phys_to_virt(phys) as *mut Table) }
}

/// Las tablas vistas a través del HHDM.
struct Hhdm;

impl Tables for Hhdm {
    fn read(&self, table: u64, i: usize) -> u64 {
        table_mut(table)[i]
    }
    fn write(&mut self, table: u64, i: usize, v: u64) {
        table_mut(table)[i] = v;
    }
}

/// Un espacio de direcciones (raíz PML4).
pub struct AddressSpace {
    pml4: PhysAddr,
}

impl AddressSpace {
    /// Crea un espacio vacío. `alloc` devuelve marcos físicos a cero.
    pub fn new(alloc: &mut dyn FnMut() -> Option<PhysAddr>) -> Option<Self> {
        Some(Self { pml4: alloc()? })
    }

    /// Dirección física de la raíz (valor para CR3).
    pub fn root(&self) -> PhysAddr {
        self.pml4
    }

    /// Mapea una página `virt → phys` con `flags` (PRESENT se agrega solo).
    pub fn map(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        size: PageSize,
        flags: u64,
        alloc: &mut dyn FnMut() -> Option<PhysAddr>,
    ) -> Result<(), MapError> {
        pt::map(&mut Hhdm, self.pml4, virt, phys, size, flags, alloc)
    }

    /// Mapea un rango usando páginas de 2 MiB donde sea posible.
    pub fn map_range(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        len: u64,
        flags: u64,
        alloc: &mut dyn FnMut() -> Option<PhysAddr>,
    ) -> Result<(), MapError> {
        let mut off = 0;
        while off < len {
            let (v, p) = (virt + off, phys + off);
            let size = if v % HUGE_PAGE_SIZE == 0
                && p % HUGE_PAGE_SIZE == 0
                && len - off >= HUGE_PAGE_SIZE
            {
                PageSize::Size2M
            } else {
                PageSize::Size4K
            };
            match self.map(v, p, size, flags, alloc) {
                // Regiones contiguas del mapa de memoria pueden compartir una
                // página de borde: no es un error mapearla dos veces igual.
                Err(MapError::AlreadyMapped) if self.translate(v) == Some(p) => {}
                Err(MapError::HugePageConflict) if self.translate(v) == Some(p) => {}
                r => r?,
            }
            off += size.bytes();
        }
        Ok(())
    }

    /// Quita el mapeo de una página de 4 KiB. Devuelve la física que había.
    pub fn unmap(&mut self, virt: VirtAddr) -> Option<PhysAddr> {
        let phys = pt::unmap(&mut Hhdm, self.pml4, virt)?;
        super::cpu::invlpg(virt);
        Some(phys)
    }

    /// Traduce una dirección virtual a física.
    pub fn translate(&self, virt: VirtAddr) -> Option<PhysAddr> {
        pt::translate(&Hhdm, self.pml4, virt)
    }

    /// Traduce y devuelve los permisos *efectivos* (USER y WRITABLE solo si
    /// están en todos los niveles; NX si está en alguno).
    pub fn translate_with_flags(&self, virt: VirtAddr) -> Option<(PhysAddr, u64)> {
        pt::translate_with_flags(&Hhdm, self.pml4, virt)
    }

    /// Crea las tablas de nivel 3 de toda la mitad alta (entradas 256..512),
    /// para que cualquier mapeo posterior del kernel sea visible en todos los
    /// espacios de cells que copian estas entradas.
    pub fn prealloc_kernel_half(
        &mut self,
        alloc: &mut dyn FnMut() -> Option<PhysAddr>,
    ) -> Result<(), MapError> {
        let pml4 = table_mut(self.pml4);
        for e in pml4[pt::KERNEL_HALF..].iter_mut() {
            if *e & flags::PRESENT == 0 {
                *e = alloc().ok_or(MapError::OutOfFrames)? | flags::PRESENT | flags::WRITABLE;
            }
        }
        Ok(())
    }

    /// Crea un espacio de cell: mitad baja vacía, mitad alta compartida con
    /// `kernel` (mismas tablas de nivel 3, sin el bit USER).
    pub fn new_user(
        kernel: &AddressSpace,
        alloc: &mut dyn FnMut() -> Option<PhysAddr>,
    ) -> Option<Self> {
        let s = Self { pml4: alloc()? };
        let src = table_mut(kernel.pml4);
        let dst = table_mut(s.pml4);
        dst[pt::KERNEL_HALF..].copy_from_slice(&src[pt::KERNEL_HALF..]);
        Some(s)
    }

    /// Libera las tablas de la mitad baja y el PML4 (no las páginas hoja,
    /// que pertenecen a la cell y se liberan aparte).
    pub fn destroy_user(self, free: &mut dyn FnMut(PhysAddr)) {
        fn walk(table: PhysAddr, level: u32, free: &mut dyn FnMut(PhysAddr)) {
            if level > 1 {
                for &e in table_mut(table).iter() {
                    if e & flags::PRESENT != 0 && e & flags::HUGE == 0 {
                        walk(e & ADDR_MASK, level - 1, free);
                    }
                }
            }
            free(table);
        }
        let pml4 = table_mut(self.pml4);
        for &e in pml4[..pt::KERNEL_HALF].iter() {
            if e & flags::PRESENT != 0 {
                walk(e & ADDR_MASK, 3, free);
            }
        }
        free(self.pml4);
    }

    /// Activa este espacio en la CPU actual.
    ///
    /// # Safety
    /// El espacio debe mapear el código, la pila y los datos en uso.
    pub unsafe fn activate(&self) {
        // SAFETY: delegada al llamador.
        unsafe { super::cpu::write_cr3(self.pml4) };
    }
}
