//! Tests que corren dentro del kernel, en QEMU, al arrancar con `test-exit`.
//!
//! Complementan los tests del host: aquí se prueba lo que depende del
//! hardware (paginación, interrupciones, cambio de contexto).

use crate::arch::paging::{flags, PageSize};
use crate::arch::{apic, cpu};
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
    ("excepciones: int3 vuelve", breakpoint),
    ("timer: llegan ticks del APIC", timer_ticks),
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

/// Recorre las tablas a mano para comprobar los bits de una dirección.
fn leaf_flags(virt: u64) -> Option<u64> {
    let mut table = cpu::read_cr3() & 0x000f_ffff_ffff_f000;
    for level in (1..=4).rev() {
        let idx = (virt >> (12 + 9 * (level - 1))) & 0x1ff;
        // SAFETY: las tablas activas están en marcos del HHDM.
        let e = unsafe { (phys_to_virt(table) as *const u64).add(idx as usize).read() };
        if e & flags::PRESENT == 0 {
            return None;
        }
        if level == 1 || e & flags::HUGE != 0 {
            return Some(e);
        }
        table = e & 0x000f_ffff_ffff_f000;
    }
    None
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
    // SAFETY: int3 genera #BP, que el handler atiende y retorna.
    unsafe { core::arch::asm!("int3") };
    check(
        trap::breakpoints() == before + 1,
        "no se contó el breakpoint",
    )
}

fn wait_ticks(n: u64) {
    let target = apic::ticks() + n;
    while apic::ticks() < target {
        cpu::wait_for_interrupt();
    }
}

fn timer_ticks() -> Result<(), &'static str> {
    let t0 = apic::ticks();
    wait_ticks(3);
    check(apic::ticks() >= t0 + 3, "no avanzan los ticks")
}

fn preemption() -> Result<(), &'static str> {
    // Tres hilos que nunca ceden la CPU: si los tres avanzan y se alternan,
    // el timer los está apropiando.
    let counters: Arc<[AtomicU64; 3]> = Arc::new(Default::default());
    let last = Arc::new(AtomicU64::new(u64::MAX));
    let alternations = Arc::new(AtomicU64::new(0));
    let end = apic::ticks() + 15;
    for i in 0..3u64 {
        let (c, l, a) = (counters.clone(), last.clone(), alternations.clone());
        sched::spawn("busy", move || {
            while apic::ticks() < end {
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
                vector: 14,
                addr: 0xffff_ffff_8000_0000,
                ..
            })
        ),
    );
    check(
        "init (supervisor) terminó con 0",
        status("init") == Some(Exited(0)),
    );
    check(
        "actualización en vivo: contador-v1, v2 y cliente terminaron con 0",
        ["contador-v1", "contador-v2", "cliente"]
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
                    vector: 14,
                    addr: 0,
                    ..
                },
                Exited(101),
                Exited(0)
            ]
        ),
    );
    check(
        "dormilona (bloqueada y girando) terminada por cell_kill",
        exits
            .iter()
            .filter(|(n, s)| n == "dormilona" && *s == Killed)
            .count()
            == 2,
    );
    if crate::pci::find(crate::VIRTIO_CONSOLE.0, crate::VIRTIO_CONSOLE.1).is_some() {
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
