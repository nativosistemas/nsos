//! nsos: microkernel experimental en Rust.
//!
//! Hito 1: arrancar con Limine en QEMU x86_64, inicializar el puerto serie e
//! informar lo que entregó el bootloader. Ver `docs/hoja-de-ruta.md`.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod arch;
mod boot;
mod console;
mod sync;

use boot::limine::{self, MemKind};
use core::panic::PanicInfo;

/// Marca que los tests de humo buscan en la salida serie.
const BOOT_OK: &str = "nsos: arranque OK";

/// Punto de entrada que invoca Limine (ver `ENTRY` en el linker script).
#[no_mangle]
extern "C" fn kmain() -> ! {
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
    kprintln!("cmdline: \"{}\"", cmdline);

    let hhdm = limine::HHDM
        .response()
        .expect("falta la respuesta HHDM")
        .offset;
    kprintln!("hhdm offset: {:#x}", hhdm);

    let memmap = limine::MEMMAP.response().expect("falta el mapa de memoria");
    let mut usable = 0u64;
    let mut reclaimable = 0u64;
    for e in memmap.entries() {
        kprintln!(
            "  mem [{:#014x} - {:#014x}) {:>8} KiB {:?}",
            e.base,
            e.base + e.length,
            e.length / 1024,
            e.kind()
        );
        match e.kind() {
            MemKind::Usable => usable += e.length,
            MemKind::BootloaderReclaimable => reclaimable += e.length,
            _ => {}
        }
    }
    kprintln!(
        "memoria utilizable: {} MiB (+{} MiB recuperable del bootloader)",
        usable / (1024 * 1024),
        reclaimable / (1024 * 1024)
    );

    kprintln!("{}", BOOT_OK);

    if cmdline.split_whitespace().any(|a| a == "test-exit") {
        arch::qemu_exit(arch::QemuExit::Success);
    }
    arch::halt_forever()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    console::_print_panic(format_args!("\n*** PANIC del kernel: {}\n", info));
    // En QEMU con isa-debug-exit esto termina la VM con código de fallo; en
    // otro entorno simplemente detiene la CPU.
    arch::qemu_exit(arch::QemuExit::Failure)
}
