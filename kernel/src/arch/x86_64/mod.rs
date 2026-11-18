//! Código dependiente de x86_64.

pub mod apic;
pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod paging;
pub mod port;
pub mod serial;

use core::arch::asm;
use port::Port;

/// Inicialización temprana de la CPU: GDT/TSS e IDT. Las interrupciones
/// siguen deshabilitadas.
pub fn init_cpu() {
    gdt::init();
    idt::init();
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
