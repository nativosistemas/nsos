//! Controlador de interrupciones GICv2 (QEMU `virt` con `gic-version=2`;
//! la Raspberry Pi 4 y la 5 también usan un GIC-400, que es GICv2).

use core::sync::atomic::{AtomicU64, Ordering};
use nsos_kcore::platform::Region;

const GICD_CTLR: u64 = 0x000;
const GICD_ISENABLER: u64 = 0x100;
const GICD_IPRIORITYR: u64 = 0x400;
const GICC_CTLR: u64 = 0x000;
const GICC_PMR: u64 = 0x004;
const GICC_IAR: u64 = 0x00C;
const GICC_EOIR: u64 = 0x010;

/// INTID que devuelve `ack` cuando no hay nada pendiente.
pub const SPURIOUS: u32 = 1023;

static GICD: AtomicU64 = AtomicU64::new(0);
static GICC: AtomicU64 = AtomicU64::new(0);

fn write(base: &AtomicU64, off: u64, v: u32) {
    // SAFETY: `base` es un bloque del GIC mapeado como dispositivo por
    // `init`; `off` es un registro de 32 bits dentro del bloque.
    unsafe { ((base.load(Ordering::Relaxed) + off) as *mut u32).write_volatile(v) }
}

fn read(base: &AtomicU64, off: u64) -> u32 {
    // SAFETY: ídem `write`.
    unsafe { ((base.load(Ordering::Relaxed) + off) as *const u32).read_volatile() }
}

/// Mapea el GIC (distribuidor e interfaz de CPU, del device tree) y lo
/// habilita. Las interrupciones siguen enmascaradas en la CPU hasta que el
/// kernel las habilite.
pub fn init(gicd: Region, gicc: Region) {
    // Los registros que se usan están en los primeros 4 KiB de cada bloque
    // (el device tree de la Pi declara 4 KiB para el distribuidor).
    GICD.store(
        crate::mm::map_mmio(gicd.base, gicd.len.max(0x1000)),
        Ordering::Relaxed,
    );
    GICC.store(
        crate::mm::map_mmio(gicc.base, gicc.len.max(0x1000)),
        Ordering::Relaxed,
    );
    write(&GICD, GICD_CTLR, 1);
    write(&GICC, GICC_PMR, 0xFF);
    write(&GICC, GICC_CTLR, 1);
}

/// Habilita la interrupción `id` (SGI/PPI/SPI) con prioridad media.
pub fn enable(id: u32) {
    let prio = GICD_IPRIORITYR + (id as u64 & !3);
    let shift = (id % 4) * 8;
    let v = read(&GICD, prio) & !(0xFF << shift);
    write(&GICD, prio, v | (0x80 << shift));
    write(&GICD, GICD_ISENABLER + 4 * (id as u64 / 32), 1 << (id % 32));
}

/// Reconoce la interrupción pendiente de mayor prioridad.
pub fn ack() -> u32 {
    read(&GICC, GICC_IAR) & 0x3FF
}

/// Fin de interrupción.
pub fn eoi(id: u32) {
    write(&GICC, GICC_EOIR, id);
}
