//! Gestión de memoria: marcos físicos, tablas de páginas del kernel y heap.

pub mod heap;
pub mod pmm;

use crate::arch::cpu;
use crate::arch::paging::{flags, AddressSpace, PAGE_SIZE};
use crate::boot::limine::{ExecutableAddress, MemKind, Memmap};
use crate::sync::IrqSpinLock;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Dirección física.
pub type PhysAddr = u64;
/// Dirección virtual.
pub type VirtAddr = u64;

static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);
static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);

/// CR3 del espacio del kernel.
pub fn kernel_root() -> PhysAddr {
    KERNEL_ROOT.load(Ordering::Relaxed)
}

/// Espacio de direcciones del kernel. Su mitad alta (entradas 256..512 del
/// PML4) se compartirá con todas las cells (hito 4).
pub static KERNEL_SPACE: IrqSpinLock<Option<AddressSpace>> = IrqSpinLock::new(None);

/// Convierte física → virtual a través del mapeo directo.
pub fn phys_to_virt(p: PhysAddr) -> VirtAddr {
    p + HHDM_OFFSET.load(Ordering::Relaxed)
}

/// Fija el desplazamiento del HHDM (una vez, al arrancar).
pub fn set_hhdm(offset: u64) {
    HHDM_OFFSET.store(offset, Ordering::Relaxed);
}

extern "C" {
    static __kernel_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
}

fn sym(s: &u8) -> u64 {
    s as *const u8 as u64
}

const fn align_up(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}

/// Inicializa marcos físicos, construye las tablas de páginas propias del
/// kernel (W^X por sección, HHDM sin ejecución) y el heap.
pub fn init(memmap: &Memmap, exec: &ExecutableAddress) {
    pmm::init(memmap);

    let mut alloc = pmm::alloc_frame;
    let mut space = AddressSpace::new(&mut alloc).expect("sin marcos para el PML4");
    let hhdm = HHDM_OFFSET.load(Ordering::Relaxed);

    // 1. Mapeo directo de la RAM (y regiones que el firmware nos deja leer).
    for e in memmap.entries() {
        let map = matches!(
            e.kind(),
            MemKind::Usable
                | MemKind::BootloaderReclaimable
                | MemKind::KernelAndModules
                | MemKind::AcpiReclaimable
                | MemKind::AcpiNvs
                | MemKind::Framebuffer
        );
        if !map {
            continue;
        }
        let base = e.base & !(PAGE_SIZE - 1);
        let len = align_up(e.base + e.length, PAGE_SIZE) - base;
        space
            .map_range(
                hhdm + base,
                base,
                len,
                flags::WRITABLE | flags::NO_EXECUTE | flags::GLOBAL,
                &mut alloc,
            )
            .expect("no se pudo mapear el HHDM");
    }

    // 2. El kernel, con permisos por sección.
    // SAFETY: símbolos definidos por el linker script; solo usamos su dirección.
    let (k_start, text_end, ro_start, ro_end, data_start, k_end) = unsafe {
        (
            sym(&__kernel_start),
            sym(&__text_end),
            sym(&__rodata_start),
            sym(&__rodata_end),
            sym(&__data_start),
            sym(&__kernel_end),
        )
    };
    let v2p = |v: u64| v - exec.virtual_base + exec.physical_base;
    let mut map_section = |start: u64, end: u64, f: u64| {
        let (s, e) = (start & !(PAGE_SIZE - 1), align_up(end, PAGE_SIZE));
        let mut v = s;
        while v < e {
            space
                .map(
                    v,
                    v2p(v),
                    crate::arch::paging::PageSize::Size4K,
                    f | flags::GLOBAL,
                    &mut alloc,
                )
                .expect("no se pudo mapear el kernel");
            v += PAGE_SIZE;
        }
    };
    map_section(k_start, text_end, 0); // R-X
    map_section(ro_start, ro_end, flags::NO_EXECUTE); // R--
    map_section(data_start, k_end, flags::WRITABLE | flags::NO_EXECUTE); // RW-

    // 3. Mitad alta completa, compartida con todas las cells.
    space
        .prealloc_kernel_half(&mut alloc)
        .expect("sin marcos para la mitad alta");

    // 4. Activar: NX y protección de escritura en anillo 0.
    // SAFETY: EFER.NXE y CR0.WP solo endurecen permisos; el nuevo espacio
    // mapea el código del kernel (sección text), sus datos y la pila actual
    // (que vive en memoria del bootloader, cubierta por el HHDM).
    unsafe {
        cpu::wrmsr(
            cpu::msr::IA32_EFER,
            cpu::rdmsr(cpu::msr::IA32_EFER) | (1 << 11),
        );
        cpu::write_cr0(cpu::read_cr0() | (1 << 16));
        space.activate();
    }
    KERNEL_ROOT.store(space.root(), Ordering::Relaxed);
    *KERNEL_SPACE.lock() = Some(space);
    enable_smep_smap();

    heap::init();
}

static SMEP: AtomicBool = AtomicBool::new(false);
static SMAP: AtomicBool = AtomicBool::new(false);

/// SMEP: el kernel no puede ejecutar páginas de usuario. SMAP: no puede
/// leerlas ni escribirlas (salvo con `RFLAGS.AC`, que el kernel nunca pone).
/// El kernel ya no las toca por diseño (`usercopy` copia por el HHDM); con
/// SMAP la CPU lo garantiza. Se activan si la CPU los tiene.
fn enable_smep_smap() {
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

/// ¿Están activos SMEP y SMAP?
pub fn smep_smap() -> (bool, bool) {
    (SMEP.load(Ordering::Relaxed), SMAP.load(Ordering::Relaxed))
}

/// Mapea un rango MMIO en el HHDM del kernel sin caché y devuelve su
/// dirección virtual.
pub fn map_mmio(phys: PhysAddr, len: u64) -> VirtAddr {
    let base = phys & !(PAGE_SIZE - 1);
    let len = align_up(phys + len, PAGE_SIZE) - base;
    let virt = phys_to_virt(base);
    let mut g = KERNEL_SPACE.lock();
    let space = g.as_mut().expect("mm sin inicializar");
    let mut alloc = pmm::alloc_frame;
    let mut off = 0;
    while off < len {
        if space.translate(virt + off).is_none() {
            space
                .map(
                    virt + off,
                    base + off,
                    crate::arch::paging::PageSize::Size4K,
                    flags::WRITABLE | flags::NO_EXECUTE | flags::NO_CACHE | flags::WRITE_THROUGH,
                    &mut alloc,
                )
                .expect("no se pudo mapear MMIO");
        }
        off += PAGE_SIZE;
    }
    phys_to_virt(phys)
}
