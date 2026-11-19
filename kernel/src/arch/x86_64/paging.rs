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

/// Tamaño de página base.
pub const PAGE_SIZE: u64 = 4096;
/// Tamaño de página grande.
pub const HUGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;

/// Bits de una entrada de tabla de páginas.
pub mod flags {
    pub const PRESENT: u64 = 1 << 0;
    pub const WRITABLE: u64 = 1 << 1;
    pub const USER: u64 = 1 << 2;
    pub const WRITE_THROUGH: u64 = 1 << 3;
    pub const NO_CACHE: u64 = 1 << 4;
    pub const HUGE: u64 = 1 << 7;
    pub const GLOBAL: u64 = 1 << 8;
    pub const NO_EXECUTE: u64 = 1 << 63;
}

const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

/// Errores al mapear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// No hay marcos para tablas intermedias.
    OutOfFrames,
    /// La dirección ya estaba mapeada.
    AlreadyMapped,
    /// Dirección no alineada al tamaño de página pedido.
    Misaligned,
    /// Choca con una página grande existente.
    HugePageConflict,
}

/// Tamaño de una página a mapear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSize {
    Size4K,
    Size2M,
}

impl PageSize {
    pub const fn bytes(self) -> u64 {
        match self {
            PageSize::Size4K => PAGE_SIZE,
            PageSize::Size2M => HUGE_PAGE_SIZE,
        }
    }
}

type Table = [u64; 512];

fn table_mut(phys: PhysAddr) -> &'static mut Table {
    // SAFETY: ver invariantes del módulo.
    unsafe { &mut *(phys_to_virt(phys) as *mut Table) }
}

fn index(virt: VirtAddr, level: u32) -> usize {
    ((virt >> (12 + 9 * (level - 1))) & 0x1ff) as usize
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
        if !virt.is_multiple_of(size.bytes()) || !phys.is_multiple_of(size.bytes()) {
            return Err(MapError::Misaligned);
        }
        let leaf_level = match size {
            PageSize::Size4K => 1,
            PageSize::Size2M => 2,
        };
        let inter = flags::PRESENT | flags::WRITABLE | (flags & flags::USER);
        let mut table = table_mut(self.pml4);
        for level in (leaf_level + 1..=4).rev() {
            let e = &mut table[index(virt, level)];
            if *e & flags::PRESENT == 0 {
                let f = alloc().ok_or(MapError::OutOfFrames)?;
                *e = f | inter;
            } else if *e & flags::HUGE != 0 {
                return Err(MapError::HugePageConflict);
            } else {
                *e |= inter;
            }
            table = table_mut(*e & ADDR_MASK);
        }
        let e = &mut table[index(virt, leaf_level)];
        if *e & flags::PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        let huge = if size == PageSize::Size2M {
            flags::HUGE
        } else {
            0
        };
        *e = phys | flags | flags::PRESENT | huge;
        Ok(())
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
        let mut table = table_mut(self.pml4);
        for level in (2..=4).rev() {
            let e = table[index(virt, level)];
            if e & flags::PRESENT == 0 || e & flags::HUGE != 0 {
                return None;
            }
            table = table_mut(e & ADDR_MASK);
        }
        let e = &mut table[index(virt, 1)];
        if *e & flags::PRESENT == 0 {
            return None;
        }
        let phys = *e & ADDR_MASK;
        *e = 0;
        super::cpu::invlpg(virt);
        Some(phys)
    }

    /// Traduce una dirección virtual a física.
    pub fn translate(&self, virt: VirtAddr) -> Option<PhysAddr> {
        let mut table = table_mut(self.pml4);
        for level in (1..=4).rev() {
            let e = table[index(virt, level)];
            if e & flags::PRESENT == 0 {
                return None;
            }
            if level == 1 || e & flags::HUGE != 0 {
                let page = match level {
                    1 => PAGE_SIZE,
                    2 => HUGE_PAGE_SIZE,
                    _ => 1 << 30,
                };
                let base = e & ADDR_MASK & !(page - 1);
                return Some(base + (virt & (page - 1)));
            }
            table = table_mut(e & ADDR_MASK);
        }
        None
    }

    /// Traduce y devuelve los permisos *efectivos* (USER y WRITABLE solo si
    /// están en todos los niveles; NX si está en alguno).
    pub fn translate_with_flags(&self, virt: VirtAddr) -> Option<(PhysAddr, u64)> {
        let mut table = table_mut(self.pml4);
        let mut eff = flags::USER | flags::WRITABLE;
        let mut nx = 0;
        for level in (1..=4).rev() {
            let e = table[index(virt, level)];
            if e & flags::PRESENT == 0 {
                return None;
            }
            eff &= e;
            nx |= e & flags::NO_EXECUTE;
            if level == 1 || e & flags::HUGE != 0 {
                let page = match level {
                    1 => PAGE_SIZE,
                    2 => HUGE_PAGE_SIZE,
                    _ => 1 << 30,
                };
                let base = e & ADDR_MASK & !(page - 1);
                return Some((base + (virt & (page - 1)), eff | nx | flags::PRESENT));
            }
            table = table_mut(e & ADDR_MASK);
        }
        None
    }

    /// Crea las tablas de nivel 3 de toda la mitad alta (entradas 256..512),
    /// para que cualquier mapeo posterior del kernel sea visible en todos los
    /// espacios de cells que copian estas entradas.
    pub fn prealloc_kernel_half(
        &mut self,
        alloc: &mut dyn FnMut() -> Option<PhysAddr>,
    ) -> Result<(), MapError> {
        let pml4 = table_mut(self.pml4);
        for e in pml4[256..].iter_mut() {
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
        dst[256..].copy_from_slice(&src[256..]);
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
        for &e in pml4[..256].iter() {
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
