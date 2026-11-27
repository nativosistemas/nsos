//! Código dependiente de ARM64 (AArch64, EL1).
//!
//! Plataforma: la que describe el device tree (GICv2, PL011, generic
//! timer y, si hay, PCI con ECAM), arrancada por Limine sobre UEFI. Hoy se
//! prueba en QEMU `virt`. Ver `docs/arm64.md`.

pub mod cpu;
pub mod gic;
pub mod paging;
pub mod platform;
pub mod serial;
pub mod trap;

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicBool, Ordering};

pub use trap::{initial_frame, TrapFrame, FAULT_EXEC, FAULT_PAGE};

// Punto de entrada (ver `ENTRY` en el linker script): el kernel corre con
// `SP_EL1` (`SPSel` = 1) sobre la pila que dejó Limine.
global_asm!(
    r#"
.section .text
.global nsos_entry
nsos_entry:
    mov x9, sp
    msr spsel, #1
    mov sp, x9
    b kmain
"#
);

/// Inicialización temprana de la CPU: vectores de excepción, FP/SIMD
/// deshabilitado (kernel y cells son `softfloat`) y timers fuera del
/// alcance de EL0. Las interrupciones siguen enmascaradas.
pub fn init_cpu() {
    trap::init();
    // SAFETY: CPACR_EL1.FPEN = 0 hace que cualquier instrucción FP/SIMD
    // falle (nadie las usa); CNTKCTL_EL1 = 0 impide que EL0 lea o programe
    // los timers (una cell no puede frenar el del scheduler).
    unsafe {
        asm!(
            "msr cpacr_el1, xzr",
            "msr cntkctl_el1, xzr",
            "isb",
            options(nomem, nostack)
        )
    };
}

/// Lee el device tree (antes de `mm::init`: el blob está en el HHDM de
/// Limine).
pub fn discover(memmap: &crate::boot::limine::Memmap) {
    platform::discover(memmap);
}

/// Mapea los dispositivos que el HHDM no cubre (UART, GIC), con las
/// direcciones del device tree. Va después de `mm::init`; hasta acá la
/// consola guardaba el texto en un búfer.
pub fn init_devices() {
    let p = platform::get();
    if let Some(uart) = p.uart {
        serial::set_base(crate::mm::map_mmio(uart.base, uart.len));
    }
    platform::report();
    let (Some(d), Some(c)) = (p.gicd, p.gicc) else {
        panic!("el device tree no describe un GICv2 utilizable");
    };
    gic::init(d, c);
}

/// Líneas de interrupción para drivers: SPI del GIC.
pub mod irq {
    use super::gic;

    /// ¿Se puede entregar a un driver?
    pub fn valid(id: u32) -> bool {
        gic::valid_spi(id)
    }

    /// Por nivel, hacia el núcleo 0, habilitada.
    pub fn route(id: u32) {
        gic::route_spi(id)
    }

    /// Enmascarar.
    pub fn mask(id: u32) {
        gic::disable(id)
    }

    /// Desenmascarar.
    pub fn unmask(id: u32) {
        gic::enable(id)
    }
}

/// Semilla para el nonce del enlace entre máquinas. Una cell en EL0 no
/// puede saber si la CPU tiene `RNDR` (y la Cortex-A76 no lo tiene), así
/// que el kernel la deriva de la semilla que deja el firmware en el device
/// tree (`/chosen/rng-seed`, nueva en cada arranque) y del contador:
/// `HMAC-SHA256(semilla, "nsos-nonce" ‖ contador)`. Sin semilla del
/// firmware devuelve ceros y la cell se niega a usarla.
pub fn nonce_seed() -> [u8; 16] {
    let p = platform::get();
    let mut out = [0; 16];
    if p.rng_seed_len == 0 {
        return out;
    }
    let mac = nsos_link::sha256::hmac(
        &p.rng_seed[..p.rng_seed_len],
        &[b"nsos-nonce", &cpu::counter().to_le_bytes()],
    );
    out.copy_from_slice(&mac[..16]);
    out
}

/// Cede la CPU al scheduler con una excepción síncrona (`svc` desde EL1).
pub fn yield_now() {
    // SAFETY: la excepción guarda y restaura el contexto completo.
    unsafe { asm!("svc #1", options(nomem)) };
}

/// Breakpoint de software (`brk`); el kernel lo cuenta y sigue.
pub fn breakpoint() {
    // SAFETY: `brk` genera una excepción que el handler atiende y retorna.
    unsafe { asm!("brk #0", options(nomem)) };
}

/// En ARM64 no hay TSS: la pila de kernel de un hilo de cell es la que
/// tiene `SP_EL1` al volver a EL0 (ver `trap`).
pub fn set_kernel_stack(_top: u64) {}

/// Raíces de la cell que va a correr: en ARM64 no hay KPTI (los núcleos
/// que usamos no son vulnerables a Meltdown).
pub fn set_cell_roots(_kernel: u64, _user: u64) {}

/// Deshabilita interrupciones y detiene la CPU para siempre.
pub fn halt_forever() -> ! {
    loop {
        cpu::disable_interrupts();
        cpu::wait_for_interrupt();
    }
}

static SEMIHOSTING: AtomicBool = AtomicBool::new(false);

/// Ajustes de la línea de comandos: `semihosting` habilita la salida de
/// QEMU por semihosting (en hardware real la instrucción no existe).
pub fn configure(has_flag: &dyn Fn(&str) -> bool) {
    SEMIHOSTING.store(has_flag("semihosting"), Ordering::Relaxed);
}

/// Código de salida de QEMU (los mismos estados que en x86_64).
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum QemuExit {
    /// QEMU termina con estado 33.
    Success = 33,
    /// QEMU termina con estado 35.
    Failure = 35,
}

/// Termina QEMU por semihosting (`SYS_EXIT`) si se arrancó con la bandera
/// `semihosting`; si no, detiene la CPU.
pub fn qemu_exit(code: QemuExit) -> ! {
    // Lo que quedó en el búfer de la consola sale antes de apagar.
    crate::console::flush();
    if SEMIHOSTING.load(Ordering::Relaxed) {
        // Si el kernel termina antes de mapear el UART (por ejemplo, un
        // device tree inválido), el texto guardado sale por semihosting.
        crate::console::with_early_text(|t| {
            for c in t {
                // SAFETY: con semihosting habilitado, `SYS_WRITEC` (`x0` =
                // 3) escribe el byte al que apunta `x1` en la consola del
                // host; no toca otra memoria.
                unsafe {
                    asm!("hlt #0xf000", in("x0") 0x03u64, in("x1") c as *const u8, options(nostack))
                };
            }
        });
        // ADP_Stopped_ApplicationExit y el estado.
        let block: [u64; 2] = [0x20026, code as u64];
        // SAFETY: con semihosting habilitado en QEMU, `hlt #0xf000` con
        // `x0` = SYS_EXIT termina la máquina virtual; `x1` apunta a un
        // bloque de dos palabras válido.
        unsafe {
            asm!(
                "hlt #0xf000",
                in("x0") 0x18u64,
                in("x1") block.as_ptr(),
                options(nostack)
            )
        };
    }
    halt_forever()
}

/// Timer del scheduler (generic timer virtual) y reloj.
pub mod time {
    use super::{cpu, gic};
    use core::sync::atomic::{AtomicU64, Ordering};

    /// INTID del timer virtual (PPI 11).
    pub const TIMER_INTID: u32 = 27;

    static TICKS: AtomicU64 = AtomicU64::new(0);
    static INTERVAL: AtomicU64 = AtomicU64::new(0);

    fn arm(interval: u64) {
        // SAFETY: programar el timer virtual de EL1 solo genera la próxima
        // interrupción `TIMER_INTID`.
        unsafe {
            core::arch::asm!(
                "msr cntv_tval_el0, {}",
                "msr cntv_ctl_el0, {}",
                "isb",
                in(reg) interval,
                in(reg) 1u64,
                options(nomem, nostack),
            )
        };
    }

    /// Arranca el timer del scheduler a `hz` interrupciones por segundo.
    pub fn init(hz: u32) {
        let interval = (cpu::counter_hz() / hz as u64).max(1);
        INTERVAL.store(interval, Ordering::Relaxed);
        gic::enable(TIMER_INTID);
        arm(interval);
        crate::kprintln!(
            "timer: generic timer a {} Hz, contador de {} MHz",
            hz,
            cpu::counter_hz() / 1_000_000
        );
    }

    /// Llamado por el handler del timer: cuenta el tick y lo vuelve a armar.
    pub fn on_tick() {
        TICKS.fetch_add(1, Ordering::Relaxed);
        arm(INTERVAL.load(Ordering::Relaxed));
    }

    /// Interrupciones de timer desde el arranque.
    pub fn ticks() -> u64 {
        TICKS.load(Ordering::Relaxed)
    }

    /// Contador libre (virtual) del generic timer.
    pub fn counter() -> u64 {
        cpu::counter()
    }

    /// Convierte una diferencia de [`counter`] a nanosegundos.
    pub fn counter_ns(c: u64) -> u64 {
        ((c as u128 * 1_000_000_000) / cpu::counter_hz().max(1) as u128) as u64
    }

    /// Reloj monótono en nanosegundos.
    pub fn now_ns() -> u64 {
        counter_ns(counter())
    }
}
