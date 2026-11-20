//! Código dependiente de x86_64.

pub mod apic;
pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod paging;
pub mod port;
pub mod serial;
pub mod syscall;
pub mod trap;

use core::arch::asm;
use port::Port;

pub use idt::{initial_frame, TrapFrame};
pub use syscall::set_kernel_stack;

/// Código de excepción de un fallo de página (`ExitStatus::Faulted`).
pub const FAULT_PAGE: u64 = 14;

/// Dispositivos que hay que mapear después de `mm::init` (ninguno: COM1 es
/// un puerto de E/S y el APIC lo mapea `time::init`).
pub fn init_devices() {}

/// Semilla para el nonce del enlace: en x86_64 la cell usa `rdrand`, así
/// que no hace falta.
pub fn nonce_seed() -> [u8; 16] {
    [0; 16]
}

/// En x86_64 no hay device tree: los dispositivos están en direcciones
/// fijas o en PCI.
pub fn discover(_memmap: &crate::boot::limine::Memmap) {}

/// Ajustes que dependen de la línea de comandos (nada en x86_64).
pub fn configure(_has_flag: &dyn Fn(&str) -> bool) {}

/// Breakpoint de software (`int3`); el kernel lo cuenta y sigue.
pub fn breakpoint() {
    // SAFETY: int3 genera #BP, que el handler atiende y retorna.
    unsafe { asm!("int3") };
}

/// Interrupciones y reloj.
pub mod time {
    use super::{apic, cpu};

    /// Arranca el timer del scheduler a `hz` interrupciones por segundo.
    pub fn init(hz: u32) {
        apic::init(hz);
        crate::kprintln!(
            "apic: timer a {} Hz, TSC ~{} MHz",
            hz,
            apic::tsc_hz() / 1_000_000
        );
    }

    /// Interrupciones de timer desde el arranque.
    pub fn ticks() -> u64 {
        apic::ticks()
    }

    /// Contador libre de la CPU (TSC); utilizable antes de `init`.
    pub fn counter() -> u64 {
        cpu::rdtsc()
    }

    /// Convierte una diferencia de [`counter`] a nanosegundos (después de
    /// `init`, que calibra el TSC).
    pub fn counter_ns(c: u64) -> u64 {
        let hz = apic::tsc_hz().max(1);
        ((c as u128 * 1_000_000_000) / hz as u128) as u64
    }

    /// Reloj monótono en nanosegundos.
    pub fn now_ns() -> u64 {
        counter_ns(counter())
    }
}

/// Inicialización temprana de la CPU: GDT/TSS e IDT. Las interrupciones
/// siguen deshabilitadas.
pub fn init_cpu() {
    gdt::init();
    idt::init();
    syscall::init();
}

/// Cede la CPU al scheduler mediante una interrupción de software.
pub fn yield_now() {
    // SAFETY: el vector YIELD tiene stub e entrada en la IDT; el handler
    // guarda y restaura el contexto completo.
    unsafe { asm!("int {v}", v = const idt::YIELD_VECTOR, options(nomem)) };
}

/// Deshabilita interrupciones y detiene la CPU para siempre.
pub fn halt_forever() -> ! {
    loop {
        // SAFETY: `cli; hlt` no accede a memoria; solo detiene este núcleo.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

/// Código de salida para el dispositivo `isa-debug-exit` de QEMU.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum QemuExit {
    /// QEMU termina con estado `(0x10 << 1) | 1 = 33`.
    Success = 0x10,
    /// QEMU termina con estado `(0x11 << 1) | 1 = 35`.
    Failure = 0x11,
}

/// Termina QEMU si se lanzó con `-device isa-debug-exit,iobase=0xf4`.
/// En hardware real (o sin el dispositivo) no hace nada y se detiene.
pub fn qemu_exit(code: QemuExit) -> ! {
    // SAFETY: 0xF4 es el puerto que configuramos para isa-debug-exit en los
    // scripts de QEMU; en un PC sin ese dispositivo el puerto no está mapeado
    // y la escritura se ignora.
    let p: Port<u32> = unsafe { Port::new(0xF4) };
    p.write(code as u32);
    halt_forever()
}
