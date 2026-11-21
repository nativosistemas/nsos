//! IOAPIC: enruta las líneas INTx de PCI a vectores del APIC local.
//!
//! QEMU (`q35` y `pc`) tiene un solo IOAPIC en `0xFEC00000` (la dirección
//! estándar; el kernel no lee ACPI). Las líneas de PCI llegan por el pin
//! igual a la `Interrupt Line` que escribió el firmware, por nivel y activas
//! en alto. Al arrancar se enmascaran todos los pines; una línea solo se
//! habilita cuando se entrega a un driver (`irq::claim`).
//!
//! # Invariantes (`unsafe`)
//! - Los registros (`IOREGSEL` en +0, `IOWIN` en +0x10) se acceden con
//!   escrituras/lecturas volátiles de 32 bits en la página MMIO mapeada sin
//!   caché por `mm::map_mmio`, y siempre bajo `LOCK` (selección + acceso).

use crate::sync::IrqSpinLock;
use core::sync::atomic::{AtomicU64, Ordering};

const PHYS: u64 = 0xFEC0_0000;
const IOREGSEL: u64 = 0x00;
const IOWIN: u64 = 0x10;
const REG_VER: u32 = 0x01;
const REG_REDTBL: u32 = 0x10;

/// Primer vector de las líneas: el pin `p` usa `VECTOR_BASE + p`.
pub const VECTOR_BASE: u8 = 48;

static BASE: AtomicU64 = AtomicU64::new(0);
static PINS: AtomicU64 = AtomicU64::new(0);
static LOCK: IrqSpinLock<()> = IrqSpinLock::new(());

fn read(reg: u32) -> u32 {
    let b = BASE.load(Ordering::Relaxed);
    // SAFETY: ver invariantes del módulo.
    unsafe {
        ((b + IOREGSEL) as *mut u32).write_volatile(reg);
        ((b + IOWIN) as *const u32).read_volatile()
    }
}

fn write(reg: u32, v: u32) {
    let b = BASE.load(Ordering::Relaxed);
    // SAFETY: ver invariantes del módulo.
    unsafe {
        ((b + IOREGSEL) as *mut u32).write_volatile(reg);
        ((b + IOWIN) as *mut u32).write_volatile(v);
    }
}

/// Mapea el IOAPIC y enmascara todos sus pines.
pub fn init() {
    BASE.store(crate::mm::map_mmio(PHYS, 0x1000), Ordering::Relaxed);
    let _g = LOCK.lock();
    let pins = ((read(REG_VER) >> 16) & 0xFF) + 1;
    PINS.store(pins as u64, Ordering::Relaxed);
    for p in 0..pins {
        let (lo, hi) = nsos_kcore::irq::ioapic_entry(VECTOR_BASE + p as u8, 0, true);
        write(REG_REDTBL + 2 * p, lo);
        write(REG_REDTBL + 2 * p + 1, hi);
    }
}

/// ¿Es un pin que el kernel puede entregar? (El 0 y el 2 son del timer
/// legado; no se entregan.)
pub fn valid(pin: u32) -> bool {
    (pin as u64) < PINS.load(Ordering::Relaxed) && pin != 0 && pin != 2
}

/// Programa el pin hacia el APIC del BSP, por nivel, habilitado.
pub fn route(pin: u32) {
    let _g = LOCK.lock();
    let (lo, hi) = nsos_kcore::irq::ioapic_entry(VECTOR_BASE + pin as u8, 0, false);
    write(REG_REDTBL + 2 * pin + 1, hi);
    write(REG_REDTBL + 2 * pin, lo);
}

fn set_mask(pin: u32, masked: bool) {
    let _g = LOCK.lock();
    let reg = REG_REDTBL + 2 * pin;
    let lo = read(reg) & !(1 << 16);
    write(reg, if masked { lo | 1 << 16 } else { lo });
}

/// Enmascara el pin.
pub fn mask(pin: u32) {
    set_mask(pin, true);
}

/// Desenmascara el pin.
pub fn unmask(pin: u32) {
    set_mask(pin, false);
}
