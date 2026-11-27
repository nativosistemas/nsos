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
mod cell;
mod console;
mod irq;
mod ktest;
mod mm;
mod pci;
mod quota;
mod sched;
mod sync;
mod syscall;
mod trap;
mod usercopy;

use arch::cpu;
use boot::limine::{self, MemKind};
use core::panic::PanicInfo;

/// Marca que los tests de humo buscan en la salida serie.
const BOOT_OK: &str = "nsos: arranque OK";
/// Frecuencia del timer del scheduler.
pub const TIMER_HZ: u32 = 100;

/// Punto de entrada que invoca Limine (ver `ENTRY` en el linker script).
#[no_mangle]
extern "C" fn kmain() -> ! {
    let t_start = arch::time::counter();
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
    arch::configure(&has_flag);

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
    #[cfg(target_arch = "x86_64")]
    kprintln!("cpu: GDT, TSS e IDT cargadas");
    #[cfg(target_arch = "aarch64")]
    kprintln!("cpu: vectores de excepción instalados (EL1)");

    // Dispositivos según el device tree (ARM64), y memoria.
    arch::discover(memmap);
    mm::init(memmap, exec);
    arch::init_devices();
    pci::init();
    let st = mm::pmm::stats();
    // De acá salen las cuotas de memoria de todas las cells.
    quota::init(st.free_frames as u64);
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
    let yn = |b| if b { "sí" } else { "no (la CPU no lo tiene)" };
    let [(n1, p1), (n2, p2)] = arch::paging::protections();
    kprintln!("mm: {} {}, {} {}", n1, yn(p1), n2, yn(p2));

    // Interrupciones y scheduler.
    arch::time::init(TIMER_HZ);
    sched::init();
    cpu::enable_interrupts();

    let boot_us = arch::time::counter_ns(arch::time::counter() - t_start) / 1000;
    kprintln!(
        "arranque del kernel: {}.{:03} ms (desde kmain)",
        boot_us / 1000,
        boot_us % 1000
    );
    kprintln!("bench: arranque_kernel_us = {}", boot_us);
    kprintln!("{}", BOOT_OK);

    if has_flag("test-wx-fault") {
        // Test negativo: escribir en .rodata debe producir #PF (W^X activo)
        // y un pánico controlado, nunca una escritura silenciosa.
        static RO: u64 = 0;
        // SAFETY: deliberadamente inválido; se espera el fallo de página.
        unsafe { (&RO as *const u64 as *mut u64).write_volatile(1) };
        kprintln!("ERROR: se pudo escribir en .rodata");
    }

    if has_flag("test-smap-fault") {
        // Test negativo: con SMAP (PAN en ARM64), que el kernel lea una
        // página de usuario directamente (sin `usercopy`) debe producir un
        // fallo de página y un pánico.
        let v = ktest::map_user_test_page();
        kprintln!(
            "test-smap-fault: el kernel lee una página de usuario en {:#x}",
            v
        );
        // SAFETY: deliberadamente prohibido por SMAP; se espera el #PF.
        let x = unsafe { (v as *const u64).read_volatile() };
        kprintln!("ERROR: el kernel leyó memoria de usuario sin SMAP ({})", x);
        arch::qemu_exit(arch::QemuExit::Failure);
    }

    if has_flag("bench") {
        // Benchmarks: solo la cell `bench`, sin nada más compitiendo.
        let ok = ktest::run_bench();
        arch::qemu_exit(if ok {
            arch::QemuExit::Success
        } else {
            arch::QemuExit::Failure
        });
    }

    for (flag, origin) in [("migrar-origen", true), ("migrar-destino", false)] {
        if has_flag(flag) {
            // Una de las dos máquinas de la migración (las une virtio-net).
            let ok = ktest::run_migration(origin);
            arch::qemu_exit(if ok {
                arch::QemuExit::Success
            } else {
                arch::QemuExit::Failure
            });
        }
    }

    if let Some(mode) = cmdline
        .split_whitespace()
        .find_map(|a| a.strip_prefix("almacen="))
    {
        // Almacenamiento propio: driver virtio-blk, almacén y clientes
        // (ver docs/almacenamiento.md y tools/store-test.sh).
        let ok = ktest::run_store(mode);
        arch::qemu_exit(if ok {
            arch::QemuExit::Success
        } else {
            arch::QemuExit::Failure
        });
    }

    if has_flag("test-exit") {
        let ok = ktest::run() & ktest::run_cells();
        kprintln!(
            "sched: {} cambios de contexto, {} ticks",
            sched::switches(),
            arch::time::ticks()
        );
        arch::qemu_exit(if ok {
            arch::QemuExit::Success
        } else {
            arch::QemuExit::Failure
        });
    }

    // Demo: arrancar las cells de ejemplo.
    if let Err(e) = boot_demo_cells() {
        kprintln!("no se pudieron arrancar las cells: {}", e);
    }

    // Este contexto es ahora el hilo idle.
    loop {
        cpu::wait_for_interrupt();
    }
}

/// Busca un módulo de Limine cuyo path termina en `/name`.
pub fn module(name: &str) -> Option<&'static [u8]> {
    limine::MODULES
        .response()?
        .iter()
        .find(|m| m.path().rsplit('/').next() == Some(name))
        .map(|m| m.bytes())
}

/// Arranca `ping` y `pong` conectados por un canal, y la cell `intruso`.
pub fn boot_demo_cells() -> Result<(), &'static str> {
    use nsos_kcore::cspace::Cap;
    let (a, b) = cell::channel_create(nsos_contract::protocols::echo::ID);
    let cap = |e| Cap {
        object: cell::KObject::Channel(e),
        rights: cell::ENDPOINT_RIGHTS,
    };
    let pong = module("pong").ok_or("falta el módulo pong")?;
    let ping = module("ping").ok_or("falta el módulo ping")?;
    let intruso = module("intruso").ok_or("falta el módulo intruso")?;
    let report = |e: cell::SpawnError| {
        kprintln!("cell::spawn: {:?}", e);
        "una cell no arrancó"
    };
    cell::spawn("pong", pong, alloc::vec![cap(b)]).map_err(report)?;
    cell::spawn("ping", ping, alloc::vec![cap(a)]).map_err(report)?;
    cell::spawn("intruso", intruso, alloc::vec![]).map_err(report)?;
    let caos = module("caos").ok_or("falta el módulo caos")?;
    cell::spawn("caos", caos, alloc::vec![]).map_err(report)?;
    let espera = module("espera").ok_or("falta el módulo espera")?;
    cell::spawn("espera", espera, alloc::vec![]).map_err(report)?;
    let desmapeo = module("desmapeo").ok_or("falta el módulo desmapeo")?;
    cell::spawn("desmapeo", desmapeo, alloc::vec![]).map_err(report)?;
    let sellada = module("sellada").ok_or("falta el módulo sellada")?;
    cell::spawn("sellada", sellada, alloc::vec![]).map_err(report)?;
    let glotona = module("glotona").ok_or("falta el módulo glotona")?;
    cell::spawn_with_quota("glotona", glotona, alloc::vec![], GLOTONA_QUOTA).map_err(report)?;
    boot_supervisor(report)?;
    boot_virtio_console(report)
}

/// Capacidad a la imagen del módulo `name` (para que una cell cree otras).
pub fn image(name: &str) -> Result<cell::KCap, &'static str> {
    let bytes = module(name).ok_or("falta un módulo de cell")?;
    Ok(nsos_kcore::cspace::Cap {
        object: cell::KObject::Image(alloc::sync::Arc::new(cell::Image {
            name: alloc::string::String::from(name),
            bytes,
        })),
        rights: cell::IMAGE_RIGHTS,
    })
}

/// Cuota de `glotona` (4 MiB): la prueba negativa de las cuotas.
pub const GLOTONA_QUOTA: u64 = 1024;

/// Cuota de `init` (16 MiB): de ella salen las de las cells que crea.
const INIT_QUOTA: u64 = 4096;

/// Arranca `init`, el supervisor, con las imágenes de las cells que vigila.
fn boot_supervisor(report: impl Fn(cell::SpawnError) -> &'static str) -> Result<(), &'static str> {
    let init = module("init").ok_or("falta el módulo init")?;
    let caps = alloc::vec![
        image("fragil")?,
        image("dormilona")?,
        image("contador-v1")?,
        image("contador-v2")?,
        image("cliente")?,
        image("contador-roto")?,
    ];
    cell::spawn_with_quota("init", init, caps, INIT_QUOTA).map_err(report)?;
    Ok(())
}

/// virtio-console (PCI legacy, `1af4:1003`).
pub const VIRTIO_CONSOLE: (u16, u16) = (0x1AF4, 0x1003);

/// ¿Hay una consola virtio (y por lo tanto arrancó `virtcon`)?
pub fn virtio_console_present() -> bool {
    pci::find(VIRTIO_CONSOLE.0, VIRTIO_CONSOLE.1).is_some()
}

/// Si hay una consola virtio, arranca su driver (`virtcon`) en user-space
/// y una cell cliente (`hola`) conectada a él por un canal. El kernel solo
/// descubre el dispositivo y entrega sus recursos como capacidades.
fn boot_virtio_console(
    report: impl Fn(cell::SpawnError) -> &'static str,
) -> Result<(), &'static str> {
    use nsos_kcore::cspace::Cap;
    let Some(dev) = pci::find(VIRTIO_CONSOLE.0, VIRTIO_CONSOLE.1) else {
        kprintln!("pci: no hay virtio-console; no arranco virtcon");
        return Ok(());
    };
    let (base, len) = dev.io_bar(0).ok_or("virtio-console sin BAR0 de E/S")?;
    dev.enable_io_and_dma();
    kprintln!(
        "pci: virtio-console en {:02x}:{:02x}.{} (puertos {:#x}+{:#x})",
        dev.bus,
        dev.dev,
        dev.func,
        base,
        len
    );
    let ports = Cap {
        object: cell::KObject::IoPorts(alloc::sync::Arc::new(cell::PortRange { base, len })),
        rights: cell::PORT_RIGHTS,
    };
    let dma = |pages| {
        cell::MemObj::new(pages, quota::root())
            .map(|m| Cap {
                object: cell::KObject::Memory(m),
                rights: cell::DMA_RIGHTS,
            })
            .ok_or("sin memoria contigua para DMA")
    };
    let ring = dma(8)?;
    let buf = dma(1)?;
    let (a, b) = cell::channel_create(nsos_contract::protocols::console::ID);
    let chan = |e| Cap {
        object: cell::KObject::Channel(e),
        rights: cell::ENDPOINT_RIGHTS,
    };
    let virtcon = module("virtcon").ok_or("falta el módulo virtcon")?;
    let hola = module("hola").ok_or("falta el módulo hola")?;
    let mut caps = alloc::vec![ports, ring, buf, chan(a)];
    caps.extend(dev.irq_cap()); // slot 5, si la línea se pudo entregar
    cell::spawn("virtcon", virtcon, caps).map_err(&report)?;
    cell::spawn("hola", hola, alloc::vec![chan(b)]).map_err(&report)?;
    Ok(())
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    cpu::disable_interrupts();
    console::_print_panic(format_args!("\n*** PANIC del kernel: {}\n", info));
    // En QEMU con isa-debug-exit esto termina la VM con código de fallo; en
    // otro entorno simplemente detiene la CPU.
    arch::qemu_exit(arch::QemuExit::Failure)
}
