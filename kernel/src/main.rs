//! nsos: microkernel experimental en Rust.
//!
//! Secuencia de arranque: consola serie → validación de Limine → GDT/IDT →
//! memoria (marcos, tablas de páginas propias, heap) → APIC y timer →
//! scheduler. Ver `docs/arquitectura.md` y `docs/hoja-de-ruta.md`.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

extern crate alloc;

mod arch;
mod boot;
mod console;
mod ktest;
mod mm;
mod sched;
mod sync;
mod trap;

use arch::{apic, cpu};
use boot::limine::{self, MemKind};
use core::panic::PanicInfo;

/// Marca que los tests de humo buscan en la salida serie.
const BOOT_OK: &str = "nsos: arranque OK";
/// Frecuencia del timer del scheduler.
const TIMER_HZ: u32 = 100;

/// Punto de entrada que invoca Limine (ver `ENTRY` en el linker script).
#[no_mangle]
extern "C" fn kmain() -> ! {
    let tsc_start = cpu::rdtsc();
    console::init();
    kprintln!();
    kprintln!(
        "nsos {} (ABI v{})",
        env!("CARGO_PKG_VERSION"),
        nsos_abi::ABI_VERSION
    );

    if !limine::BASE_REVISION.is_supported() {
        panic!(
            "el bootloader no soporta la revisión base {} del protocolo Limine",
            limine::REQUESTED_BASE_REVISION
        );
    }
    if let Some(info) = limine::BOOTLOADER_INFO.response() {
        kprintln!("bootloader: {} {}", info.name(), info.version());
    }
    let cmdline = limine::CMDLINE.response().map(|c| c.as_str()).unwrap_or("");
    let has_flag = |f: &str| cmdline.split_whitespace().any(|a| a == f);
    kprintln!("cmdline: \"{}\"", cmdline);

    let hhdm = limine::HHDM.response().expect("falta la respuesta HHDM");
    mm::set_hhdm(hhdm.offset);
    let memmap = limine::MEMMAP.response().expect("falta el mapa de memoria");
    let exec = limine::EXECUTABLE_ADDRESS
        .response()
        .expect("falta la dirección del ejecutable");

    if has_flag("memmap") {
        for e in memmap.entries() {
            kprintln!(
                "  mem [{:#014x} - {:#014x}) {:>8} KiB {:?}",
                e.base,
                e.base + e.length,
                e.length / 1024,
                e.kind()
            );
        }
    }

    // CPU: segmentos, TSS e interrupciones (aún deshabilitadas).
    arch::init_cpu();
    kprintln!("cpu: GDT, TSS e IDT cargadas");

    // Memoria.
    mm::init(memmap, exec);
    let st = mm::pmm::stats();
    let reclaimable: u64 = memmap
        .entries()
        .filter(|e| e.kind() == MemKind::BootloaderReclaimable)
        .map(|e| e.length)
        .sum();
    kprintln!(
        "mm: {} MiB libres de {} MiB gestionados (+{} KiB del bootloader sin reclamar); tablas de páginas propias, W^X",
        st.free_frames * 4 / 1024,
        st.total_frames * 4 / 1024,
        reclaimable / 1024
    );
    let (_, heap_free) = mm::heap::usage();
    kprintln!("mm: heap del kernel de {} KiB", heap_free / 1024);

    // Interrupciones y scheduler.
    apic::init(TIMER_HZ);
    kprintln!(
        "apic: timer a {} Hz, TSC ~{} MHz",
        TIMER_HZ,
        apic::tsc_hz() / 1_000_000
    );
    sched::init();
    cpu::enable_interrupts();

    let boot_us = (cpu::rdtsc() - tsc_start) * 1_000_000 / apic::tsc_hz().max(1);
    kprintln!(
        "arranque del kernel: {}.{:03} ms (desde kmain)",
        boot_us / 1000,
        boot_us % 1000
    );
    kprintln!("{}", BOOT_OK);

    if has_flag("test-wx-fault") {
        // Test negativo: escribir en .rodata debe producir #PF (W^X activo)
        // y un pánico controlado, nunca una escritura silenciosa.
        static RO: u64 = 0;
        // SAFETY: deliberadamente inválido; se espera el fallo de página.
        unsafe { (&RO as *const u64 as *mut u64).write_volatile(1) };
        kprintln!("ERROR: se pudo escribir en .rodata");
    }

    if has_flag("test-exit") {
        let ok = ktest::run();
        kprintln!(
            "sched: {} cambios de contexto, {} ticks",
            sched::switches(),
            apic::ticks()
        );
        arch::qemu_exit(if ok {
            arch::QemuExit::Success
        } else {
            arch::QemuExit::Failure
        });
    }

    // Este contexto es ahora el hilo idle.
    loop {
        cpu::wait_for_interrupt();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    cpu::disable_interrupts();
    console::_print_panic(format_args!("\n*** PANIC del kernel: {}\n", info));
    // En QEMU con isa-debug-exit esto termina la VM con código de fallo; en
    // otro entorno simplemente detiene la CPU.
    arch::qemu_exit(arch::QemuExit::Failure)
}
