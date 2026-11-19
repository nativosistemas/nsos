//! APIC local, PIC legado y calibración con el PIT.
//!
//! Se deshabilita el 8259 (todas sus líneas enmascaradas) y se usa el timer
//! del APIC local en modo periódico para el scheduler. La frecuencia del timer
//! (y de paso la del TSC) se mide contra el canal 2 del PIT.
//!
//! # Invariantes (`unsafe`)
//! - Los registros del APIC se acceden con lecturas/escrituras volátiles de
//!   32 bits dentro de la página MMIO mapeada sin caché por `mm::map_mmio`.
//! - Los puertos 0x20/0x21/0xA0/0xA1 (PIC) y 0x42/0x43/0x61 (PIT canal 2)
//!   solo los usa este módulo.

use super::cpu;
use super::port::Port;
use core::sync::atomic::{AtomicU64, Ordering};

/// Vector del timer del APIC.
pub const TIMER_VECTOR: u8 = 32;
/// Vector de interrupción espuria.
pub const SPURIOUS_VECTOR: u8 = 0xFF;

const REG_ID: usize = 0x20;
const REG_EOI: usize = 0xB0;
const REG_SVR: usize = 0xF0;
const REG_LVT_TIMER: usize = 0x320;
const REG_TIMER_INIT: usize = 0x380;
const REG_TIMER_CURRENT: usize = 0x390;
const REG_TIMER_DIV: usize = 0x3E0;

static BASE: AtomicU64 = AtomicU64::new(0);
static TSC_HZ: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

fn read(reg: usize) -> u32 {
    // SAFETY: ver invariantes del módulo.
    unsafe { ((BASE.load(Ordering::Relaxed) as usize + reg) as *const u32).read_volatile() }
}

fn write(reg: usize, v: u32) {
    // SAFETY: ver invariantes del módulo.
    unsafe { ((BASE.load(Ordering::Relaxed) as usize + reg) as *mut u32).write_volatile(v) }
}

fn disable_pic() {
    // SAFETY: puertos del PIC 8259, exclusivos de este módulo.
    let (cmd1, data1, cmd2, data2) = unsafe {
        (
            Port::<u8>::new(0x20),
            Port::<u8>::new(0x21),
            Port::<u8>::new(0xA0),
            Port::<u8>::new(0xA1),
        )
    };
    // Remapear a 0x20..0x2F por si llega una IRQ espuria, y enmascarar todo.
    cmd1.write(0x11);
    cmd2.write(0x11);
    data1.write(0x20);
    data2.write(0x28);
    data1.write(4);
    data2.write(2);
    data1.write(1);
    data2.write(1);
    data1.write(0xFF);
    data2.write(0xFF);
}

/// Espera ~`ms` milisegundos con el canal 2 del PIT (máx. ~54 ms).
fn pit_wait_ms(ms: u32) {
    // SAFETY: puertos del PIT canal 2 y del controlador 0x61, exclusivos.
    let (ctrl, ch2, gate) = unsafe {
        (
            Port::<u8>::new(0x43),
            Port::<u8>::new(0x42),
            Port::<u8>::new(0x61),
        )
    };
    let count = (1_193_182u32 * ms / 1000).min(0xFFFF) as u16;
    gate.write((gate.read() & 0xFD) | 0x01); // gate alto, parlante apagado
    ctrl.write(0b1011_0000); // canal 2, lo/hi, modo 0
    ch2.write(count as u8);
    ch2.write((count >> 8) as u8);
    let g = gate.read() & 0xFE;
    gate.write(g); // reiniciar la cuenta
    gate.write(g | 1);
    while gate.read() & 0x20 == 0 {
        core::hint::spin_loop();
    }
}

/// Inicializa el APIC local del BSP y arranca el timer a `hz` interrupciones
/// por segundo.
pub fn init(hz: u32) {
    disable_pic();
    // SAFETY: IA32_APIC_BASE existe en toda CPU x86_64.
    let base_phys = unsafe { cpu::rdmsr(cpu::msr::IA32_APIC_BASE) } & 0xFFFF_F000;
    let virt = crate::mm::map_mmio(base_phys, 4096);
    BASE.store(virt, Ordering::Relaxed);

    write(REG_SVR, 0x100 | SPURIOUS_VECTOR as u32); // habilitar APIC

    // Calibración: cuántos ticks del timer (div 16) y del TSC hay en 10 ms.
    write(REG_TIMER_DIV, 0x3);
    write(REG_LVT_TIMER, 1 << 16); // enmascarado
    write(REG_TIMER_INIT, u32::MAX);
    let t0 = cpu::rdtsc();
    pit_wait_ms(10);
    let elapsed = u32::MAX - read(REG_TIMER_CURRENT);
    let tsc = cpu::rdtsc() - t0;
    TSC_HZ.store(tsc * 100, Ordering::Relaxed);

    let per_tick = ((elapsed as u64 * 100) / hz as u64).max(1) as u32;
    write(REG_LVT_TIMER, TIMER_VECTOR as u32 | (1 << 17)); // periódico
    write(REG_TIMER_INIT, per_tick);
}

/// Fin de interrupción.
pub fn eoi() {
    write(REG_EOI, 0);
}

/// ID del APIC local.
#[allow(dead_code)]
pub fn id() -> u32 {
    read(REG_ID) >> 24
}

/// Llamado por el handler del timer.
pub fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Interrupciones de timer desde el arranque.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Frecuencia medida del TSC (0 si aún no se calibró).
pub fn tsc_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}
