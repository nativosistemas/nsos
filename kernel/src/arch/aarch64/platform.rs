//! La máquina, según el device tree que entrega Limine.
//!
//! [`discover`] corre antes de que el kernel instale sus propias tablas de
//! páginas (el blob está en el HHDM de Limine). Lee el árbol con el lector
//! verificado de `nsos_kcore::fdt`, y descarta toda región de dispositivo
//! que toque la RAM del mapa de memoria (`platform::mmio_allowed`): el
//! device tree es un dato de afuera y no puede hacer que el kernel mapee
//! como dispositivo memoria que usa él o una cell.

use crate::boot::limine::{self, MemKind, Memmap};
use crate::sync::IrqSpinLock;
use nsos_kcore::fdt::{Fdt, HEADER_LEN};
use nsos_kcore::platform::{mmio_allowed, Platform, Region};

/// Tamaño máximo de DTB que se acepta (los reales tienen decenas de KiB;
/// el de QEMU declara 1 MiB).
const MAX_DTB: usize = 2 << 20;
/// Regiones del mapa de memoria que se comparan (más: todo MMIO se rechaza).
const MAX_RAM: usize = 256;

static PLATFORM: IrqSpinLock<Option<Platform>> = IrqSpinLock::new(None);

/// La plataforma descubierta (después de [`discover`]).
pub fn get() -> Platform {
    PLATFORM.lock().expect("plataforma sin descubrir")
}

/// Lee el device tree y valida sus regiones contra el mapa de memoria.
pub fn discover(memmap: &Memmap) {
    let ptr = limine::DTB
        .response()
        .map(|r| r.ptr())
        .filter(|p| !p.is_null())
        .expect("Limine no entregó el device tree");
    // SAFETY: Limine entrega un DTB válido en memoria del bootloader,
    // mapeada en su HHDM (activo hasta `mm::init`); la cabecera mide al
    // menos `HEADER_LEN` bytes.
    let header = unsafe { core::slice::from_raw_parts(ptr, HEADER_LEN) };
    let total = Fdt::total_size(header).expect("el device tree no tiene la magia 0xd00dfeed");
    assert!(
        (HEADER_LEN..=MAX_DTB).contains(&total),
        "device tree de tamaño inválido ({} bytes)",
        total
    );
    // SAFETY: ídem; el blob entero mide `totalsize` bytes (acotado arriba).
    let blob = unsafe { core::slice::from_raw_parts(ptr, total) };
    let fdt = Fdt::new(blob).unwrap_or_else(|e| panic!("device tree inválido: {:?}", e));
    let mut p = Platform::from_fdt(&fdt);

    let mut ram = [Region { base: 0, len: 0 }; MAX_RAM];
    let mut n = 0;
    let mut too_many = false;
    for e in memmap.entries() {
        if e.kind() == MemKind::Reserved {
            continue;
        }
        match ram.get_mut(n) {
            Some(r) => {
                *r = Region {
                    base: e.base,
                    len: e.length,
                };
                n += 1;
            }
            None => too_many = true,
        }
    }
    let ok = |r: Option<Region>, what: &str| match r {
        Some(r) if !too_many && mmio_allowed(r, &ram[..n]) => Some(r),
        Some(r) => {
            crate::kprintln!(
                "dt: descarto {} en {:#x}+{:#x}: se solapa con la RAM",
                what,
                r.base,
                r.len
            );
            None
        }
        None => None,
    };
    p.uart = ok(p.uart, "el UART");
    p.gicd = ok(p.gicd, "el distribuidor del GIC");
    p.gicc = ok(p.gicc, "la interfaz de CPU del GIC");
    if let Some(mut pci) = p.pci {
        let io = pci.io.map(|w| Region {
            base: w.cpu,
            len: w.len,
        });
        if ok(io, "la ventana de E/S de PCI").is_none() {
            pci.io = None;
        }
        p.pci = ok(Some(pci.ecam), "la región ECAM de PCI").map(|_| pci);
    }
    *PLATFORM.lock() = Some(p);
}

/// Resumen de lo encontrado (va después de mapear el UART).
pub fn report() {
    let p = get();
    let r = |r: Option<Region>| r.map_or(0, |r| r.base);
    crate::kprintln!(
        "dt: PL011 en {:#x}, GICv2 en {:#x}/{:#x}, PCI ECAM {}, semilla del firmware de {} bytes",
        r(p.uart),
        r(p.gicd),
        r(p.gicc),
        if p.pci.is_some() { "sí" } else { "no" },
        p.rng_seed_len
    );
}
