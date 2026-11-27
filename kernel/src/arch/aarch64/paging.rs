//! Tablas de páginas ARM64: 4 niveles, granulo de 4 KiB, 48 bits.
//!
//! El recorrido es el mismo que en x86_64 (`nsos_kcore::paging`), con
//! entradas neutras: [`nsos_kcore::paging_arm64::Arm64`] traduce cada
//! lectura y escritura al formato ARM64. Que lo que ve el kernel es lo que
//! hace cumplir la MMU está demostrado con Kani en ese módulo.
//!
//! A diferencia de x86_64 hay dos raíces: `TTBR1_EL1` traduce la mitad alta
//! (kernel, HHDM, pilas de kernel) y es la misma siempre; `TTBR0_EL1`
//! traduce la mitad baja y cambia con cada cell. Las cells no copian nada
//! del kernel en sus tablas.
//!
//! # Invariantes (`unsafe`)
//! - Toda dirección física de tabla proviene del allocador de marcos (marco
//!   propio, de 4 KiB, puesto a cero) o de una entrada válida de una tabla
//!   válida; por eso convertirla a `&mut [u64; 512]` vía HHDM es correcto.
//! - Un `AddressSpace` es dueño exclusivo de sus tablas intermedias; el
//!   acceso concurrente lo serializa quien lo contenga (un lock).

use super::cpu::read_sysreg;
use crate::mm::{phys_to_virt, PhysAddr, VirtAddr};
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use nsos_kcore::paging::{self as pt, Tables};
pub use nsos_kcore::paging::{flags, MapError, PageSize, HUGE_PAGE_SIZE, PAGE_SIZE};
use nsos_kcore::paging_arm64::{self as arm, Arm64};

type Table = [u64; 512];

fn table_mut(phys: PhysAddr) -> &'static mut Table {
    // SAFETY: ver invariantes del módulo.
    unsafe { &mut *(phys_to_virt(phys) as *mut Table) }
}

/// Las tablas reales (descriptores ARM64) vistas a través del HHDM.
struct Hhdm;

impl Tables for Hhdm {
    fn read(&self, table: u64, i: usize) -> u64 {
        table_mut(table)[i]
    }
    fn write(&mut self, table: u64, i: usize, v: u64) {
        table_mut(table)[i] = v;
        // Que el recorrido de la MMU vea la escritura antes de usarla.
        // SAFETY: barreras sin efectos sobre la memoria del programa.
        unsafe { asm!("dsb ishst", "isb", options(nostack)) };
    }
}

fn view() -> Arm64<Hhdm> {
    Arm64(Hhdm)
}

/// Un espacio de direcciones (raíz de nivel 0).
pub struct AddressSpace {
    root: PhysAddr,
}

impl AddressSpace {
    /// Crea un espacio vacío. `alloc` devuelve marcos físicos a cero.
    pub fn new(alloc: &mut dyn FnMut() -> Option<PhysAddr>) -> Option<Self> {
        Some(Self { root: alloc()? })
    }

    /// Dirección física de la raíz.
    pub fn root(&self) -> PhysAddr {
        self.root
    }

    /// Raíz con la que corre la cell en EL0: la misma (sin KPTI).
    pub fn user_root(&self) -> PhysAddr {
        self.root()
    }

    /// Mapea una página `virt → phys` con `flags` neutros.
    pub fn map(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        size: PageSize,
        flags: u64,
        alloc: &mut dyn FnMut() -> Option<PhysAddr>,
    ) -> Result<(), MapError> {
        pt::map(&mut view(), self.root, virt, phys, size, flags, alloc)
    }

    /// Mapea un rango usando bloques de 2 MiB donde sea posible.
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
        let phys = pt::unmap(&mut view(), self.root, virt)?;
        // SAFETY: invalidar una entrada del TLB (de todos los ASID) no
        // afecta a la memoria del programa.
        unsafe {
            asm!(
                "dsb ishst",
                "tlbi vaae1is, {}",
                "dsb ish",
                "isb",
                in(reg) (virt >> 12) & ((1 << 44) - 1),
                options(nostack),
            )
        };
        Some(phys)
    }

    /// Traduce una dirección virtual a física.
    pub fn translate(&self, virt: VirtAddr) -> Option<PhysAddr> {
        pt::translate(&view(), self.root, virt)
    }

    /// Traduce y devuelve los permisos efectivos (neutros).
    pub fn translate_with_flags(&self, virt: VirtAddr) -> Option<(PhysAddr, u64)> {
        pt::translate_with_flags(&view(), self.root, virt)
    }

    /// En ARM64 la mitad alta tiene su propia raíz (`TTBR1_EL1`), común a
    /// todos los espacios: no hace falta preparar nada.
    pub fn prealloc_kernel_half(
        &mut self,
        _alloc: &mut dyn FnMut() -> Option<PhysAddr>,
    ) -> Result<(), MapError> {
        Ok(())
    }

    /// Crea un espacio de cell vacío (va en `TTBR0_EL1`; el kernel está en
    /// `TTBR1_EL1` y no se copia).
    pub fn new_user(
        _kernel: &AddressSpace,
        alloc: &mut dyn FnMut() -> Option<PhysAddr>,
        _free: &mut dyn FnMut(PhysAddr),
    ) -> Option<Self> {
        Self::new(alloc)
    }

    /// Libera las tablas de la mitad baja y la raíz (no las páginas hoja,
    /// que pertenecen a la cell y se liberan aparte).
    pub fn destroy_user(self, free: &mut dyn FnMut(PhysAddr)) {
        fn walk(table: PhysAddr, level: u32, free: &mut dyn FnMut(PhysAddr)) {
            if level > 1 {
                for &d in table_mut(table).iter() {
                    let e = arm::decode(d);
                    if e & flags::PRESENT != 0 && e & flags::HUGE == 0 {
                        walk(e & pt::ADDR_MASK, level - 1, free);
                    }
                }
            }
            free(table);
        }
        for &d in table_mut(self.root)[..pt::KERNEL_HALF].iter() {
            let e = arm::decode(d);
            if e & flags::PRESENT != 0 {
                walk(e & pt::ADDR_MASK, 3, free);
            }
        }
        free(self.root);
    }

    /// Activa este espacio como el del kernel (`TTBR1_EL1`).
    ///
    /// # Safety
    /// El espacio debe mapear el código, la pila y los datos en uso.
    pub unsafe fn activate(&self) {
        // SAFETY: delegada al llamador.
        unsafe {
            asm!(
                "dsb ish",
                "msr ttbr1_el1, {}",
                "isb",
                "tlbi vmalle1",
                "dsb nsh",
                "isb",
                in(reg) self.root,
                options(nostack),
            )
        };
    }
}

/// Raíz del espacio del kernel (para distinguirla de la de una cell).
static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);
/// Tabla vacía para `TTBR0_EL1` mientras corre un hilo de kernel.
static EMPTY_ROOT: AtomicU64 = AtomicU64::new(0);

/// Cambia la mitad baja a la de la cell con raíz `root`; con la raíz del
/// kernel deja la mitad baja vacía.
///
/// # Safety
/// `root` debe ser la raíz del espacio del kernel o de una cell viva.
pub unsafe fn switch_to(root: PhysAddr) {
    if root == 0 {
        return;
    }
    let ttbr0 = if root == KERNEL_ROOT.load(Ordering::Relaxed) {
        EMPTY_ROOT.load(Ordering::Relaxed)
    } else {
        root
    };
    if read_sysreg!("ttbr0_el1") & pt::ADDR_MASK != ttbr0 {
        // Sin ASID: se invalida el TLB local entero en cada cambio.
        // SAFETY: la mitad alta (kernel) no depende de TTBR0; delegada al
        // llamador la validez de la mitad baja.
        unsafe {
            asm!(
                "msr ttbr0_el1, {}",
                "isb",
                "tlbi vmalle1",
                "dsb nsh",
                "isb",
                in(reg) ttbr0,
                options(nostack),
            )
        };
    }
}

/// Bits de `TCR_EL1`.
mod tcr {
    pub const T0SZ: u64 = 0x3F;
    pub const T1SZ: u64 = 0x3F << 16;
    pub const TG0: u64 = 0b11 << 14;
    pub const TG1: u64 = 0b11 << 30;
    pub const TG1_4K: u64 = 0b10 << 30;
    pub const EPD0: u64 = 1 << 7;
    pub const EPD1: u64 = 1 << 23;
    pub const A1: u64 = 1 << 22;
    /// HPD0, HPD1 y HWU059..HWU162 (bits 41..=50).
    pub const HPD_HWU: u64 = 0x3FF << 41;
}

/// Bits de `SCTLR_EL1`.
mod sctlr {
    /// EL0 puede tocar `DAIF` (enmascarar interrupciones).
    pub const UMA: u64 = 1 << 9;
    /// Lo escribible no se ejecuta (W^X por hardware).
    pub const WXN: u64 = 1 << 19;
    /// En 0: `PSTATE.PAN` se pone al entrar a EL1 por una excepción.
    pub const SPAN: u64 = 1 << 23;
}

/// Activa el espacio del kernel por primera vez: atributos de memoria,
/// control de traducción y W^X por hardware.
///
/// # Safety
/// El espacio debe mapear el código, los datos y la pila en uso.
pub unsafe fn install(space: &AddressSpace) {
    let t = read_sysreg!("tcr_el1");
    assert!(
        t & tcr::T0SZ == 16
            && t & tcr::T1SZ == 16 << 16
            && t & tcr::TG0 == 0
            && t & tcr::TG1 == tcr::TG1_4K,
        "TCR_EL1 inesperado ({t:#x}): se esperan 48 bits y granulo de 4 KiB"
    );
    // Permisos jerárquicos activos (APTable/XNTable) y bits 59..62 de las
    // hojas ignorados (sin PBHA): la codificación de `paging_arm64` lo exige.
    // ASID de TTBR0 y las dos mitades habilitadas.
    let t = t & !(tcr::HPD_HWU | tcr::A1 | tcr::EPD0 | tcr::EPD1);
    let s = read_sysreg!("sctlr_el1");
    let s = (s | sctlr::WXN) & !(sctlr::UMA | sctlr::SPAN);
    let empty = crate::mm::pmm::alloc_frame().expect("sin marcos para la tabla vacía");
    EMPTY_ROOT.store(empty, Ordering::Relaxed);
    KERNEL_ROOT.store(space.root(), Ordering::Relaxed);
    // SAFETY: MAIR solo agrega el índice de dispositivo (el 0 sigue siendo
    // memoria normal write-back, como lo dejó Limine); TCR conserva tamaño y
    // granulo; las nuevas tablas mapean lo que está en uso (delegado al
    // llamador) y la mitad baja queda vacía (el kernel no la usa).
    unsafe {
        asm!(
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "isb",
            "msr ttbr0_el1, {empty}",
            mair = in(reg) arm::MAIR,
            tcr = in(reg) t,
            empty = in(reg) empty,
            options(nostack),
        );
        space.activate();
        asm!(
            "msr sctlr_el1, {}",
            "isb",
            "tlbi vmalle1",
            "dsb nsh",
            "isb",
            in(reg) s,
            options(nostack),
        );
    }
}

static PAN: AtomicBool = AtomicBool::new(false);

/// PAN (Privileged Access Never, ARMv8.1): el kernel no puede leer ni
/// escribir páginas de cells (como SMAP). Con `SCTLR_EL1.SPAN` = 0 se
/// vuelve a poner en cada excepción. Que no las ejecute (como SMEP) no
/// depende de la CPU: toda página de cell lleva PXN.
pub fn harden() {
    let pan = (read_sysreg!("id_aa64mmfr1_el1") >> 20) & 0xF != 0;
    if pan {
        // SAFETY: `msr pan, #1` (codificado a mano para no depender de las
        // extensiones del ensamblador) solo restringe al kernel, que no toca
        // páginas de cells.
        unsafe { asm!(".inst 0xd500419f", options(nomem, nostack)) };
    }
    PAN.store(pan, Ordering::Relaxed);
}

/// ¿Está activo PAN?
pub fn pan_enabled() -> bool {
    PAN.load(Ordering::Relaxed)
}

/// Protecciones contra el kernel tocando páginas de cells: (no ejecutar,
/// no leer ni escribir).
pub fn protections() -> [(&'static str, bool); 2] {
    [("PXN", true), ("PAN", pan_enabled())]
}

/// Sin KPTI en ARM64: nada que preparar (los núcleos que usamos no son
/// vulnerables a Meltdown).
pub fn prepare_user_half(_kernel: &AddressSpace, _alloc: &mut dyn FnMut() -> Option<PhysAddr>) {}
