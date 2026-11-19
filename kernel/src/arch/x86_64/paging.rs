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

/// Cambia al espacio de direcciones con raíz `root` si no es el activo.
///
/// # Safety
/// `root` debe ser la raíz del espacio del kernel o de una cell viva: todos
/// mapean la mitad alta completa (kernel, HHDM, pilas de kernel).
pub unsafe fn switch_to(root: PhysAddr) {
    if root != 0 && super::cpu::read_cr3() & !0xFFF != root {
        // SAFETY: delegada al llamador.
        unsafe { super::cpu::write_cr3(root) };
    }
}

/// Activa el espacio del kernel por primera vez: NX y protección de
/// escritura en anillo 0 (CR0.WP).
///
/// # Safety
/// El espacio debe mapear el código, los datos y la pila en uso.
pub unsafe fn install(space: &AddressSpace) {
    use super::cpu;
    // SAFETY: EFER.NXE y CR0.WP solo endurecen permisos; el espacio mapea
    // lo que está en uso (delegado al llamador).
    unsafe {
        cpu::wrmsr(
            cpu::msr::IA32_EFER,
            cpu::rdmsr(cpu::msr::IA32_EFER) | (1 << 11),
        );
        cpu::write_cr0(cpu::read_cr0() | (1 << 16));
        space.activate();
    }
}

static SMEP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static SMAP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// SMEP: el kernel no puede ejecutar páginas de usuario. SMAP: no puede
/// leerlas ni escribirlas (salvo con `RFLAGS.AC`, que el kernel nunca pone).
/// El kernel ya no las toca por diseño (`usercopy` copia por el HHDM); con
/// SMAP la CPU lo garantiza. Se activan si la CPU los tiene.
pub fn harden() {
    use super::cpu;
    use core::sync::atomic::Ordering;
    let (_, ebx, _, _) = cpu::cpuid(7, 0);
    let smep = ebx & (1 << 7) != 0;
    let smap = ebx & (1 << 20) != 0;
    let mut cr4 = cpu::read_cr4();
    if smep {
        cr4 |= 1 << 20;
    }
    if smap {
        cr4 |= 1 << 21;
    }
    // SAFETY: solo se activan bits que CPUID reporta; ninguno afecta al
    // kernel, que no ejecuta ni toca páginas de usuario.
    unsafe { cpu::write_cr4(cr4) };
    SMEP.store(smep, Ordering::Relaxed);
    SMAP.store(smap, Ordering::Relaxed);
}

/// Protecciones de la CPU contra el kernel tocando páginas de cells:
/// (no ejecutar, no leer ni escribir) = (SMEP, SMAP).
pub fn protections() -> [(&'static str, bool); 2] {
    use core::sync::atomic::Ordering;
    [
        ("SMEP", SMEP.load(Ordering::Relaxed)),
        ("SMAP", SMAP.load(Ordering::Relaxed)),
    ]
}
