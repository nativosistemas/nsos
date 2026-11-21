//! Tests que corren dentro del kernel, en QEMU, al arrancar con `test-exit`.
//!
//! Complementan los tests del host: aquí se prueba lo que depende del
//! hardware (paginación, interrupciones, cambio de contexto).

use crate::arch::paging::{flags, PageSize};
use crate::arch::{cpu, time};
use crate::mm::{self, phys_to_virt, pmm};
use crate::{kprint, kprintln, sched, trap};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

type Test = (&'static str, fn() -> Result<(), &'static str>);

const TESTS: &[Test] = &[
    ("heap: Vec, Box y String", heap_basic),
    ("marcos: reservar y liberar", frames),
    ("paginación: mapear, traducir y desmapear", paging),
    ("paginación: W^X en el kernel", kernel_wx),
    ("excepciones: un breakpoint vuelve", breakpoint),
    ("timer: llegan ticks", timer_ticks),
    ("scheduler: apropiación entre 3 hilos", preemption),
    ("scheduler: yield alterna hilos", yield_alternates),
    ("scheduler: 50 hilos terminan y se liberan", many_threads),
    ("ipc: extremo en su propia cola se libera", ipc_self_loop),
    ("ipc: ciclo de dos canales se libera", ipc_two_cycle),
    ("ipc: extremo alcanzable no se libera", ipc_reachable_kept),
];

/// Ejecuta todos los tests. Devuelve `true` si pasaron todos.
pub fn run() -> bool {
    kprintln!("ktest: {} tests", TESTS.len());
    let mut failed = 0;
    for (name, t) in TESTS {
        kprint!("  {} ... ", name);
        match t() {
            Ok(()) => kprintln!("ok"),
            Err(e) => {
                kprintln!("FALLO: {}", e);
                failed += 1;
            }
        }
    }
    kprintln!("ktest: {} ok, {} fallos", TESTS.len() - failed, failed);
    failed == 0
}

fn check(cond: bool, msg: &'static str) -> Result<(), &'static str> {
    if cond {
        Ok(())
    } else {
        Err(msg)
    }
}

fn heap_basic() -> Result<(), &'static str> {
    let (used0, _) = mm::heap::usage();
    {
        let v: Vec<u64> = (0..10_000).collect();
        check(v.iter().sum::<u64>() == 49_995_000, "suma incorrecta")?;
        let b = Box::new([7u8; 4096]);
        check(b.iter().all(|&x| x == 7), "Box corrupto")?;
        let mut s = String::new();
        for i in 0..100 {
            s += alloc::format!("{i},").as_str();
        }
        check(s.starts_with("0,1,2,"), "String incorrecto")?;
    }
    let (used1, _) = mm::heap::usage();
    check(used1 == used0, "fuga de memoria en el heap")
}

fn frames() -> Result<(), &'static str> {
    let before = pmm::stats().free_frames;
    let mut got = Vec::new();
    for _ in 0..32 {
        got.push(pmm::alloc_frame().ok_or("sin marcos")?);
    }
    let mut sorted = got.clone();
    sorted.sort();
    sorted.dedup();
    check(sorted.len() == 32, "marcos repetidos")?;
    check(
        got.iter().all(|&p| p % 4096 == 0 && p != 0),
        "marco mal alineado o nulo",
    )?;
    // Puestos a cero.
    // SAFETY: el marco es nuestro y está en el HHDM.
    let zeroed = unsafe { core::slice::from_raw_parts(phys_to_virt(got[0]) as *const u64, 512) };
    check(zeroed.iter().all(|&w| w == 0), "marco no puesto a cero")?;
    for p in got {
        pmm::free_frame(p);
    }
    check(
        pmm::stats().free_frames == before,
        "no se recuperaron los marcos",
    )
}

const TEST_VADDR: u64 = 0xffff_a000_0000_0000;

/// Para el test negativo de SMAP/PAN: mapea una página *de usuario* (bit
/// USER) en la mitad baja activa y devuelve su dirección.
pub fn map_user_test_page() -> u64 {
    const USER_VADDR: u64 = 0x7000_0000;
    let phys = pmm::alloc_frame().expect("sin marcos");
    let mut alloc = pmm::alloc_frame;
    let f = flags::USER | flags::NO_EXECUTE;
    let mut g = mm::KERNEL_SPACE.lock();
    let kernel = g.as_mut().expect("mm sin inicializar");
    if cfg!(target_arch = "x86_64") {
        // La mitad baja del espacio del kernel es la activa.
        kernel
            .map(USER_VADDR, phys, PageSize::Size4K, f, &mut alloc)
            .expect("map falló");
    } else {
        // La mitad baja tiene su propia raíz: un espacio de cell de prueba
        // (que se pierde: el test termina en pánico).
        let mut user =
            crate::arch::paging::AddressSpace::new_user(kernel, &mut alloc).expect("sin marcos");
        user.map(USER_VADDR, phys, PageSize::Size4K, f, &mut alloc)
            .expect("map falló");
        // SAFETY: el espacio nuevo es una mitad baja válida; el kernel no
        // depende de la mitad baja.
        unsafe { crate::arch::paging::switch_to(user.root()) };
        // `user` no libera sus tablas al salir de alcance: quedan en uso.
    }
    USER_VADDR
}

fn paging() -> Result<(), &'static str> {
    let phys = pmm::alloc_frame().ok_or("sin marcos")?;
    {
        let mut g = mm::KERNEL_SPACE.lock();
        let space = g.as_mut().ok_or("mm sin inicializar")?;
        let mut alloc = pmm::alloc_frame;
        space
            .map(
                TEST_VADDR,
                phys,
                PageSize::Size4K,
                flags::WRITABLE | flags::NO_EXECUTE,
                &mut alloc,
            )
            .map_err(|_| "map falló")?;
        check(
            space.translate(TEST_VADDR + 0x123) == Some(phys + 0x123),
            "translate incorrecto",
        )?;
        check(
            space
                .map(TEST_VADDR, phys, PageSize::Size4K, 0, &mut alloc)
                .is_err(),
            "permitió mapear dos veces",
        )?;
    }
    // Escribir por la dirección nueva y leer por el alias del HHDM.
    // SAFETY: TEST_VADDR acaba de mapearse RW sobre un marco nuestro.
    unsafe { (TEST_VADDR as *mut u64).write_volatile(0xC0FFEE) };
    // SAFETY: mismo marco, alias en el HHDM.
    let v = unsafe { (phys_to_virt(phys) as *const u64).read_volatile() };
    check(v == 0xC0FFEE, "los alias no coinciden")?;
    let mut g = mm::KERNEL_SPACE.lock();
    let space = g.as_mut().ok_or("mm sin inicializar")?;
    check(space.unmap(TEST_VADDR) == Some(phys), "unmap incorrecto")?;
    check(space.translate(TEST_VADDR).is_none(), "sigue mapeado")?;
    drop(g);
    pmm::free_frame(phys);
    Ok(())
}

/// Permisos efectivos de una dirección en el espacio del kernel, como los
/// ve la MMU (en ARM64, a través del codec verificado con Kani).
fn leaf_flags(virt: u64) -> Option<u64> {
    let g = mm::KERNEL_SPACE.lock();
    g.as_ref()?.translate_with_flags(virt).map(|(_, f)| f)
}

fn kernel_wx() -> Result<(), &'static str> {
    static DATA: AtomicU64 = AtomicU64::new(0);
    static RODATA: [u8; 4] = *b"nsos";
    let text = leaf_flags(kernel_wx as *const () as u64).ok_or("text sin mapear")?;
    let ro = leaf_flags(RODATA.as_ptr() as u64).ok_or("rodata sin mapear")?;
    let data = leaf_flags(&DATA as *const _ as u64).ok_or("data sin mapear")?;
    let boxed = Box::new(0u8);
    let heap = leaf_flags(&*boxed as *const u8 as u64).ok_or("heap sin mapear")?;
    let w = |e: u64| e & flags::WRITABLE != 0;
    let x = |e: u64| e & flags::NO_EXECUTE == 0;
    check(x(text) && !w(text), "text debería ser R-X")?;
    check(!x(ro) && !w(ro), "rodata debería ser R--")?;
    check(!x(data) && w(data), "data debería ser RW-")?;
    check(!x(heap) && w(heap), "heap debería ser RW-")
}

fn breakpoint() -> Result<(), &'static str> {
    let before = trap::breakpoints();
    crate::arch::breakpoint();
    check(
        trap::breakpoints() == before + 1,
        "no se contó el breakpoint",
    )
}

fn wait_ticks(n: u64) {
    let target = time::ticks() + n;
    while time::ticks() < target {
        cpu::wait_for_interrupt();
    }
}

fn timer_ticks() -> Result<(), &'static str> {
    let t0 = time::ticks();
    wait_ticks(3);
    check(time::ticks() >= t0 + 3, "no avanzan los ticks")
}

fn preemption() -> Result<(), &'static str> {
    // Tres hilos que nunca ceden la CPU: si los tres avanzan y se alternan,
    // el timer los está apropiando.
    let counters: Arc<[AtomicU64; 3]> = Arc::new(Default::default());
    let last = Arc::new(AtomicU64::new(u64::MAX));
    let alternations = Arc::new(AtomicU64::new(0));
    let end = time::ticks() + 15;
    for i in 0..3u64 {
        let (c, l, a) = (counters.clone(), last.clone(), alternations.clone());
        sched::spawn("busy", move || {
            while time::ticks() < end {
                c[i as usize].fetch_add(1, Ordering::Relaxed);
                if l.swap(i, Ordering::Relaxed) != i {
                    a.fetch_add(1, Ordering::Relaxed);
                }
                core::hint::spin_loop();
            }
        });
    }
    sched::wait_all();
    check(
        counters.iter().all(|c| c.load(Ordering::Relaxed) > 0),
        "algún hilo no corrió",
    )?;
    check(
        alternations.load(Ordering::Relaxed) >= 6,
        "no hubo apropiación",
    )
}

fn yield_alternates() -> Result<(), &'static str> {
    let log = Arc::new(crate::sync::IrqSpinLock::new(Vec::new()));
    for id in 0..2u8 {
        let log = log.clone();
        sched::spawn("ping", move || {
            for _ in 0..50 {
                log.lock().push(id);
                crate::arch::yield_now();
            }
        });
    }
    sched::wait_all();
    let log = log.lock();
    check(log.len() == 100, "faltan entradas")?;
    let changes = log.windows(2).filter(|w| w[0] != w[1]).count();
    check(changes >= 90, "yield no alterna los hilos")
}

fn many_threads() -> Result<(), &'static str> {
    let done = Arc::new(AtomicU64::new(0));
    // Calentar: el primer lote crea estructuras (colas) que se quedan.
    sched::spawn("warm", || {});
    sched::wait_all();
    let (used0, _) = mm::heap::usage();
    for _ in 0..50 {
        let d = done.clone();
        sched::spawn("short", move || {
            d.fetch_add(1, Ordering::Relaxed);
        });
    }
    sched::wait_all();
    check(done.load(Ordering::Relaxed) == 50, "no corrieron todos")?;
    // Forzar un cambio más para liberar el último zombie.
    crate::arch::yield_now();
    let (used1, _) = mm::heap::usage();
    // Tolerancia: crecimiento de las colas internas del scheduler.
    check(
        used1 <= used0 + 4096,
        "las pilas de los hilos muertos no se liberaron",
    )
}

// ------------------------------------------------- recolector de IPC

/// Encola en `via` (hacia su peer) un mensaje que lleva `cargo`.
fn ipc_send(
    via: &Arc<crate::cell::Endpoint>,
    cargo: &Arc<crate::cell::Endpoint>,
) -> Result<(), &'static str> {
    use crate::cell::{KObject, QueuedMsg, ENDPOINT_RIGHTS};
    use nsos_kcore::cspace::Cap;
    let cap = Cap {
        object: KObject::Channel(cargo.clone()),
        rights: ENDPOINT_RIGHTS,
    };
    via.send_with(false, || {
        Ok(QueuedMsg::new(
            Default::default(),
            [0; nsos_abi::msg::MAX_DATA],
            alloc::vec![cap],
        ))
    })
    .map_err(|_| "no se pudo encolar")
}

fn ipc_self_loop() -> Result<(), &'static str> {
    let (a, b) = crate::cell::channel_create();
    ipc_send(&a, &b)?; // b viaja en la cola de b
    let (wa, wb) = (Arc::downgrade(&a), Arc::downgrade(&b));
    drop((a, b));
    check(wa.upgrade().is_none(), "el lado 0 sigue vivo")?;
    check(wb.upgrade().is_some(), "el ciclo no se formó")?;
    crate::cell::ipc_gc();
    check(
        wb.upgrade().is_none(),
        "el extremo en su propia cola no se liberó",
    )
}

fn ipc_two_cycle() -> Result<(), &'static str> {
    let (a0, a1) = crate::cell::channel_create();
    let (b0, b1) = crate::cell::channel_create();
    ipc_send(&a0, &b1)?; // b1 viaja hacia a1
    ipc_send(&b0, &a1)?; // a1 viaja hacia b1
    let (wa, wb) = (Arc::downgrade(&a1), Arc::downgrade(&b1));
    drop((a0, a1, b0, b1));
    check(
        wa.upgrade().is_some() && wb.upgrade().is_some(),
        "el ciclo no se formó",
    )?;
    crate::cell::ipc_gc();
    check(
        wa.upgrade().is_none() && wb.upgrade().is_none(),
        "el ciclo no se liberó",
    )
}

fn ipc_reachable_kept() -> Result<(), &'static str> {
    let (a0, a1) = crate::cell::channel_create();
    let (b0, b1) = crate::cell::channel_create();
    ipc_send(&a0, &b1)?; // b1 viaja hacia a1, que seguimos teniendo
    let wb = Arc::downgrade(&b1);
    drop((a0, b0, b1));
    crate::cell::ipc_gc();
    check(wb.upgrade().is_some(), "se liberó un extremo alcanzable")?;
    drop(a1); // ahora sí: su cola se descarta y b1 muere
    check(wb.upgrade().is_none(), "b1 no se liberó al cerrar a1")
}

/// Test de integración: tres cells en anillo 3. `ping` y `pong` hablan por
/// IPC (con transferencia de capacidades); `intruso` intenta violar el
/// aislamiento y termina por fallo de página. El kernel debe seguir vivo.
/// Modo benchmark: arranca solo la cell `bench` y espera que termine con 0.
pub fn run_bench() -> bool {
    let caps =
        (|| Ok::<_, &'static str>(alloc::vec![crate::image("pong")?, crate::image("fragil")?]))();
    let (Some(bench), Ok(caps)) = (crate::module("bench"), caps) else {
        kprintln!("bench: faltan módulos");
        return false;
    };
    if let Err(e) = crate::cell::spawn("bench", bench, caps) {
        kprintln!("bench: no arrancó: {:?}", e);
        return false;
    }
    sched::wait_all();
    let ok = crate::cell::EXITS
        .lock()
        .iter()
        .any(|(n, s)| n == "bench" && *s == crate::cell::ExitStatus::Exited(0));
    kprintln!("bench: {}", if ok { "terminó con 0" } else { "FALLO" });
    ok
}

/// virtio-net (PCI legacy, `1af4:1000`): la red que une las dos máquinas de
/// la migración.
const VIRTIO_NET: (u16, u16) = (0x1AF4, 0x1000);

/// Una de las dos máquinas de la migración. El kernel descubre la placa de
/// red y entrega sus recursos al driver `virtnet`; al `enlace` le da un canal
/// con el driver y, en un `Memory` de solo lectura, la clave compartida (el
/// módulo `enlace.key`), el rol de esta máquina y una semilla para su nonce
/// (`arch::nonce_seed`). Espera a que todas las
/// cells terminen y exige que terminen con 0.
pub fn run_migration(origin: bool) -> bool {
    use crate::cell::{self, KObject};
    use nsos_abi::Rights;
    use nsos_kcore::cspace::Cap;
    let (sup, images, cells): (&str, [&str; 2], [&str; 5]) = if origin {
        (
            "origen",
            ["contador-v1", "cliente"],
            ["virtnet", "enlace", "origen", "contador-v1", "cliente"],
        )
    } else {
        (
            "destino",
            ["contador-roto", "contador-v2"],
            [
                "virtnet",
                "enlace",
                "destino",
                "contador-roto",
                "contador-v2",
            ],
        )
    };
    kprintln!("migración: esta máquina es el {}", sup);
    let spawn = || -> Result<(), &'static str> {
        let chan = |e| Cap {
            object: KObject::Channel(e),
            rights: cell::ENDPOINT_RIGHTS,
        };
        let dev = crate::pci::find(VIRTIO_NET.0, VIRTIO_NET.1).ok_or("no hay virtio-net")?;
        let (base, len) = dev.io_bar(0).ok_or("virtio-net sin BAR0 de E/S")?;
        dev.enable_io_and_dma();
        kprintln!("pci: virtio-net en puertos {:#x}+{:#x}", base, len);
        let ports = Cap {
            object: KObject::IoPorts(alloc::sync::Arc::new(cell::PortRange { base, len })),
            rights: cell::PORT_RIGHTS,
        };
        let dma = |pages| {
            cell::MemObj::new(pages)
                .map(|m| Cap {
                    object: KObject::Memory(m),
                    rights: cell::DMA_RIGHTS,
                })
                .ok_or("sin memoria contigua para DMA")
        };
        let key = crate::module("enlace.key").ok_or("falta la clave del enlace (enlace.key)")?;
        if key.len() < 32 {
            return Err("la clave del enlace tiene menos de 32 bytes");
        }
        let mut config = [0u8; 49];
        config[..32].copy_from_slice(&key[..32]);
        config[32] = if origin { 0 } else { 1 };
        config[33..49].copy_from_slice(&crate::arch::nonce_seed());
        let config = Cap {
            object: KObject::Memory(cell::MemObj::from_bytes(&config).ok_or("sin memoria")?),
            rights: Rights::READ,
        };
        let (net_a, net_b) = cell::channel_create();
        let (ctl_a, ctl_b) = cell::channel_create();
        let virtnet = crate::module("virtnet").ok_or("falta el módulo virtnet")?;
        let enlace = crate::module("enlace").ok_or("falta el módulo enlace")?;
        let sup_img = crate::module(sup).ok_or("falta el supervisor")?;
        let caps = alloc::vec![
            chan(ctl_b),
            crate::image(images[0])?,
            crate::image(images[1])?
        ];
        let mut net_caps = alloc::vec![ports, dma(8)?, dma(5)?, chan(net_a)];
        net_caps.extend(dev.irq_cap()); // slot 5, si la línea se pudo entregar
        cell::spawn("virtnet", virtnet, net_caps).map_err(|_| "virtnet no arrancó")?;
        cell::spawn(
            "enlace",
            enlace,
            alloc::vec![chan(net_b), config, chan(ctl_a)],
        )
        .map_err(|_| "enlace no arrancó")?;
        cell::spawn(sup, sup_img, caps).map_err(|_| "el supervisor no arrancó")?;
        Ok(())
    };
    // Todas juntas: `virtnet` y `enlace` sondean cediendo la CPU, y este hilo
    // (el idle) no vuelve a correr mientras haya otro listo.
    if let Err(e) = cpu::without_interrupts(spawn) {
        kprintln!("migración: {}", e);
        return false;
    }
    sched::wait_all();
    let exits = crate::cell::EXITS.lock().clone();
    let mut ok = true;
    for name in cells {
        let st = exits.iter().find(|(n, _)| n == name).map(|(_, s)| *s);
        let good = st == Some(crate::cell::ExitStatus::Exited(0));
        kprintln!(
            "  {} terminó con 0 ... {}",
            name,
            if good { "ok" } else { "FALLO" }
        );
        ok &= good;
    }
    kprintln!("migración: {}", if ok { "OK" } else { "FALLO" });
    ok
}

/// virtio-blk (PCI legacy, `1af4:1001`): el disco del almacén.
const VIRTIO_BLK: (u16, u16) = (0x1AF4, 0x1001);

/// Almacenamiento: el kernel descubre el disco virtio y entrega sus
/// recursos al driver `virtblk`. El búfer de datos es un solo objeto
/// `Memory` con dos capacidades: la del driver (con `CONTROL`, para DMA) y
/// la del `almacen` (sin `CONTROL`). Al `almacen` le da además un canal con
/// el driver y las raíces de los dueños 1 (`escritor`) y 2 (`curioso`).
///
/// `mode` (de `almacen=...` en la línea de comandos): `escribir:N`,
/// `verificar`, `caer:N` (caída simulada antes del superbloque de la
/// escritura N), `sin-fin` (escribe hasta que se mate a QEMU) o `ajeno`
/// (el disco tiene otra cosa: el almacén no lo toca). Espera a que las
/// cells terminen y comprueba cómo terminó cada una.
pub fn run_store(mode: &str) -> bool {
    use crate::cell::{self, ExitStatus::Exited, KObject};
    use nsos_abi::Rights;
    use nsos_kcore::cspace::Cap;
    let (tag, n) = match mode.split_once(':') {
        Some((m, n)) => (m, n.parse::<u8>().unwrap_or(0)),
        None => (mode, 0),
    };
    let code = match tag {
        "escribir" => b'e',
        "verificar" => b'v',
        "caer" => b'c',
        "sin-fin" => b's',
        "ajeno" => b'a',
        _ => {
            kprintln!("almacen: modo desconocido \"{}\"", mode);
            return false;
        }
    };
    // Qué tiene que pasar: (cell, estado de salida).
    let expect: &[(&str, i32)] = match code {
        b'e' | b'v' => &[
            ("virtblk", 0),
            ("almacen", 0),
            ("escritor", 0),
            ("curioso", 0),
        ],
        b'c' => &[("virtblk", 0), ("almacen", 7), ("escritor", 4)],
        b'a' => &[("virtblk", 0), ("almacen", 5), ("escritor", 4)],
        _ => &[],
    };
    let with_curioso = matches!(code, b'e' | b'v');
    kprintln!("almacen: modo {}", mode);
    let spawn = || -> Result<(), &'static str> {
        let chan = |e| Cap {
            object: KObject::Channel(e),
            rights: cell::ENDPOINT_RIGHTS,
        };
        let dev = crate::pci::find(VIRTIO_BLK.0, VIRTIO_BLK.1).ok_or("no hay virtio-blk")?;
        let (base, len) = dev.io_bar(0).ok_or("virtio-blk sin BAR0 de E/S")?;
        dev.enable_io_and_dma();
        kprintln!("pci: virtio-blk en puertos {:#x}+{:#x}", base, len);
        let ports = Cap {
            object: KObject::IoPorts(Arc::new(cell::PortRange { base, len })),
            rights: cell::PORT_RIGHTS,
        };
        let ring = cell::MemObj::new(5).ok_or("sin memoria contigua para DMA")?;
        let buf = cell::MemObj::new(1).ok_or("sin memoria contigua para DMA")?;
        let mem = |m: &Arc<cell::MemObj>, rights| Cap {
            object: KObject::Memory(m.clone()),
            rights,
        };
        let allow_format = (code != b'v') as u8;
        let config = cell::MemObj::from_bytes(&[code, n, allow_format]).ok_or("sin memoria")?;
        let (drv_a, drv_b) = cell::channel_create();
        let (root1_a, root1_b) = cell::channel_create();
        let (root2_a, root2_b) = cell::channel_create();
        let (peer_a, peer_b) = cell::channel_create();
        let module = |n| crate::module(n).ok_or("falta un módulo del almacén");
        let mut blk_caps = alloc::vec![
            ports,
            mem(&ring, cell::DMA_RIGHTS),
            mem(&buf, cell::DMA_RIGHTS),
            chan(drv_a)
        ];
        blk_caps.extend(dev.irq_cap()); // slot 5, si la línea se pudo entregar
        cell::spawn("virtblk", module("virtblk")?, blk_caps).map_err(|_| "virtblk no arrancó")?;
        cell::spawn(
            "almacen",
            module("almacen")?,
            alloc::vec![
                chan(drv_b),
                mem(&buf, Rights::READ | Rights::WRITE),
                mem(&config, Rights::READ),
                chan(root1_a),
                chan(root2_a)
            ],
        )
        .map_err(|_| "almacen no arrancó")?;
        let mut caps = alloc::vec![chan(root1_b), mem(&config, Rights::READ)];
        if with_curioso {
            caps.push(chan(peer_a));
        }
        cell::spawn("escritor", module("escritor")?, caps).map_err(|_| "escritor no arrancó")?;
        if with_curioso {
            cell::spawn(
                "curioso",
                module("curioso")?,
                alloc::vec![chan(root2_b), chan(peer_b)],
            )
            .map_err(|_| "curioso no arrancó")?;
        }
        Ok(())
    };
    if let Err(e) = cpu::without_interrupts(spawn) {
        kprintln!("almacen: {}", e);
        return false;
    }
    sched::wait_all();
    let exits = crate::cell::EXITS.lock().clone();
    let mut ok = true;
    for (name, want) in expect {
        let st = exits.iter().find(|(n, _)| n == name).map(|(_, s)| *s);
        let good = st == Some(Exited(*want));
        kprintln!(
            "  {} terminó con {} ... {}",
            name,
            want,
            if good { "ok" } else { "FALLO" }
        );
        ok &= good;
    }
    kprintln!("almacen: {}", if ok { "OK" } else { "FALLO" });
    ok
}

pub fn run_cells() -> bool {
    kprintln!("ktest: cells en anillo 3");
    let free_before = pmm::stats().free_frames;
    if let Err(e) = crate::boot_demo_cells() {
        kprintln!("  FALLO: {}", e);
        return false;
    }
    sched::wait_all();
    let exits = crate::cell::EXITS.lock().clone();
    let status = |n: &str| exits.iter().find(|(name, _)| name == n).map(|(_, s)| *s);
    use crate::cell::ExitStatus::*;
    let mut ok = true;
    let mut check = |name: &str, cond: bool| {
        kprintln!("  {} ... {}", name, if cond { "ok" } else { "FALLO" });
        ok &= cond;
    };
    check("ping terminó con 0", status("ping") == Some(Exited(0)));
    check("pong terminó con 0", status("pong") == Some(Exited(0)));
    check(
        "intruso terminado por #PF al leer el kernel",
        matches!(
            status("intruso"),
            Some(Faulted {
                vector: crate::arch::FAULT_PAGE,
                addr: 0xffff_ffff_8000_0000,
                ..
            })
        ),
    );
    check(
        "caos: 50000 syscalls al azar y termina con 0",
        status("caos") == Some(Exited(0)),
    );
    check(
        "espera: semántica de wait y channel_call y termina con 0",
        status("espera") == Some(Exited(0)),
    );
    check(
        "desmapeo: mem_unmap y #PF al leer la página desmapeada",
        matches!(
            status("desmapeo"),
            Some(Faulted {
                vector: crate::arch::FAULT_PAGE,
                addr: 0x5800_0000,
                ..
            })
        ),
    );
    check(
        "init (supervisor) terminó con 0",
        status("init") == Some(Exited(0)),
    );
    check(
        "actualización en vivo con rollback: contador-v1, roto, v2 y cliente terminaron con 0",
        ["contador-v1", "contador-roto", "contador-v2", "cliente"]
            .iter()
            .all(|n| status(n) == Some(Exited(0))),
    );
    let fragil: alloc::vec::Vec<_> = exits
        .iter()
        .filter(|(n, _)| n == "fragil")
        .map(|(_, s)| *s)
        .collect();
    check(
        "fragil: #PF, pánico y al tercer intento sale con 0",
        matches!(
            fragil[..],
            [
                Faulted {
                    vector: crate::arch::FAULT_PAGE,
                    addr: 0,
                    ..
                },
                Exited(101),
                Exited(0)
            ]
        ),
    );
    check(
        "dormilona (bloqueada, girando y en wait) terminada por cell_kill",
        exits
            .iter()
            .filter(|(n, s)| n == "dormilona" && *s == Killed)
            .count()
            == 3,
    );
    if crate::virtio_console_present() {
        check(
            "virtcon recibió interrupciones (Irq + wait + irq_ack)",
            crate::irq::delivered() > 0,
        );
        check(
            "virtcon (driver virtio) terminó con 0",
            status("virtcon") == Some(Exited(0)),
        );
        check("hola terminó con 0", status("hola") == Some(Exited(0)));
    } else {
        kprintln!("  (sin virtio-console: se omite el driver)");
    }
    check(
        "la memoria de las cells se liberó",
        pmm::stats().free_frames == free_before,
    );
    check(
        "el kernel sigue: el heap responde",
        Box::new(1u8) == Box::new(1u8),
    );
    ok
}
