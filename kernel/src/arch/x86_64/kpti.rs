//! KPTI: las cells corren con tablas de páginas que no mapean el kernel.
//!
//! En una CPU vulnerable a Meltdown, una cell podría leer por ejecución
//! especulativa cualquier página mapeada en su espacio aunque no tenga el
//! bit USER: el kernel, el HHDM (toda la RAM), las pilas de kernel. Por eso
//! cada cell tiene dos raíces:
//!
//! * la **del kernel** (la de siempre): la mitad baja de la cell y la mitad
//!   alta entera;
//! * la **de usuario**: la misma mitad baja (las mismas tablas) y, de la
//!   mitad alta, solo las páginas de `.kpti_text` (los stubs de entrada y
//!   salida) y `.kpti_data` (GDT, TSS, IDT, la pila de entrada, la del doble
//!   fallo y las variables de este módulo), que la CPU o los stubs necesitan
//!   antes de cambiar de tablas.
//!
//! Al entrar desde anillo 3 (interrupción o `syscall`), el stub carga la
//! raíz del kernel y recién entonces pasa a la pila de kernel del hilo; al
//! volver, copia el marco de `iretq` a la pila de entrada, carga la raíz de
//! usuario y vuelve (`idt.rs`, `syscall.rs`). El kernel no usa páginas
//! globales, así que cambiar de raíz no deja sus traducciones en la TLB.
//!
//! **PCID** (si la CPU lo tiene): cada cell que corre recibe un lugar de
//! `nsos_kcore::asid` con dos identificadores, uno para su raíz del kernel y
//! otro para la de usuario, así que entrar y salir del kernel, o volver a una
//! cell que conserva su lugar, no vacía la TLB (bit 63 de CR3). Se vacía un
//! identificador cuando su lugar pasa a otra raíz, cuando la raíz se libera
//! ([`forget`]) y cuando se desmapea algo de ella ([`invalidate`]: `invlpg`
//! solo limpia el identificador cargado). Kani demuestra con un modelo de la
//! TLB que, si no se vacía, no queda ninguna traducción de otra raíz ni una
//! vieja (`asid::switch_is_safe`). Sin PCID, cada cambio de raíz vacía la
//! TLB.
//!
//! # Invariantes (`unsafe`)
//! - Un solo núcleo: la pila de entrada y las variables son globales.
//! - [`NSOS_USER_CR3`] y [`NSOS_KERNEL_CR3`] son las raíces de la cell del
//!   hilo que va a volver a anillo 3 (las fija el scheduler antes de
//!   reanudarlo).
//! - La plantilla se arma una vez en [`init`], antes de crear cells.

use super::paging::{flags, AddressSpace, PageSize, PAGE_SIZE};
use crate::mm::PhysAddr;
use crate::sync::IrqSpinLock;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use nsos_kcore::asid::Asids;

/// Bit 63 de CR3 con PCID: cargar sin vaciar la TLB del identificador.
const NOFLUSH: u64 = 1 << 63;
/// La dirección de la raíz en un valor de CR3.
const ROOT_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// ¿Hay PCID (CR4.PCIDE)?
static PCID: AtomicBool = AtomicBool::new(false);

/// ¿Se pidió PCID (bandera `pcid`)? Apagado por defecto: QEMU sin KVM no
/// emula PCID, así que este camino solo se prueba con KVM o en hardware.
static WANT_PCID: AtomicBool = AtomicBool::new(false);

/// Pide (o no) usar PCID si la CPU lo tiene.
pub fn want_pcid(on: bool) {
    WANT_PCID.store(on, Ordering::Relaxed);
}

/// `NOFLUSH` si hay PCID, si no 0: el stub de salida se lo agrega a
/// `NSOS_USER_CR3` después de cargarla (así se vacía una sola vez).
#[no_mangle]
#[link_section = ".kpti_data"]
static NSOS_NOFLUSH: AtomicU64 = AtomicU64::new(0);

/// Lugares de PCID y la raíz del kernel cargada (la de una cell o la del
/// kernel).
struct Tlb {
    asids: Asids,
    current: PhysAddr,
}

static TLB: IrqSpinLock<Tlb> = IrqSpinLock::new(Tlb {
    asids: Asids::new(),
    current: 0,
});

/// Raíz del kernel de la cell actual (la carga el stub de entrada).
#[no_mangle]
#[link_section = ".kpti_data"]
static NSOS_KERNEL_CR3: AtomicU64 = AtomicU64::new(0);

/// Raíz de usuario de la cell actual (la carga el stub de salida).
#[no_mangle]
#[link_section = ".kpti_data"]
static NSOS_USER_CR3: AtomicU64 = AtomicU64::new(0);

/// La CR3 con la que llegó la última entrada desde anillo 3 (la guarda el
/// stub antes de cargar la del kernel): para comprobar que las cells corren
/// de verdad con su raíz de usuario.
#[no_mangle]
#[link_section = ".kpti_data"]
static NSOS_ENTRY_CR3: AtomicU64 = AtomicU64::new(0);

/// Entradas desde anillo 3, y cuántas llegaron con otra raíz que la de
/// usuario de la cell (ktest exige 0).
static ENTRIES: AtomicU64 = AtomicU64::new(0);
static WRONG_ROOT: AtomicU64 = AtomicU64::new(0);

/// Llamada al entrar desde anillo 3: ¿la cell corría con su raíz de usuario?
pub fn check_entry() {
    ENTRIES.fetch_add(1, Ordering::Relaxed);
    let at_entry = NSOS_ENTRY_CR3.load(Ordering::Relaxed) & ROOT_MASK;
    if at_entry != NSOS_USER_CR3.load(Ordering::Relaxed) & ROOT_MASK {
        WRONG_ROOT.fetch_add(1, Ordering::Relaxed);
    }
}

/// `(entradas desde anillo 3, con otra raíz que la de usuario)`.
pub fn entry_stats() -> (u64, u64) {
    (
        ENTRIES.load(Ordering::Relaxed),
        WRONG_ROOT.load(Ordering::Relaxed),
    )
}

/// Tamaño de la pila de entrada.
pub const ENTRY_STACK_SIZE: usize = 4096;

#[repr(C, align(4096))]
struct EntryStack(UnsafeCell<[u8; ENTRY_STACK_SIZE]>);
// SAFETY: solo la usan los stubs, con interrupciones apagadas (un núcleo).
unsafe impl Sync for EntryStack {}

/// Pila donde la CPU deja el marco al entrar desde anillo 3 (`TSS.RSP0`):
/// la única pila que ven las tablas de usuario.
#[no_mangle]
#[link_section = ".kpti_data"]
static NSOS_ENTRY_STACK: EntryStack = EntryStack(UnsafeCell::new([0; ENTRY_STACK_SIZE]));

/// Tope de la pila de entrada.
pub fn entry_stack_top() -> u64 {
    NSOS_ENTRY_STACK.0.get() as u64 + ENTRY_STACK_SIZE as u64
}

struct Template(UnsafeCell<[u64; 256]>);
// SAFETY: se escribe una vez en `init`, antes de crear cells; después solo
// se lee.
unsafe impl Sync for Template {}

/// Entradas 256..512 del PML4 de usuario de todas las cells: solo las
/// páginas de `.kpti_text` y `.kpti_data`.
static TEMPLATE: Template = Template(UnsafeCell::new([0; 256]));

extern "C" {
    static __kpti_text_start: u8;
    static __kpti_text_end: u8;
    static __kpti_data_start: u8;
    static __kpti_data_end: u8;
}

/// Las páginas que ven las tablas de usuario: `(inicio, fin, ejecutable)`.
pub fn shared_ranges() -> [(u64, u64, bool); 2] {
    // SAFETY: símbolos del linker script; solo se usa su dirección.
    unsafe {
        [
            (
                &__kpti_text_start as *const u8 as u64,
                &__kpti_text_end as *const u8 as u64,
                true,
            ),
            (
                &__kpti_data_start as *const u8 as u64,
                &__kpti_data_end as *const u8 as u64,
                false,
            ),
        ]
    }
}

/// Arma la plantilla de la mitad alta de las tablas de usuario: un espacio
/// aparte que mapea solo las páginas compartidas, en las mismas direcciones
/// y con la misma física que en el kernel (`kernel`).
pub fn init(kernel: &AddressSpace, alloc: &mut dyn FnMut() -> Option<PhysAddr>) {
    let mut t = AddressSpace::new(alloc).expect("sin marcos para KPTI");
    for (start, end, exec) in shared_ranges() {
        let f = if exec {
            0
        } else {
            flags::WRITABLE | flags::NO_EXECUTE
        };
        let mut v = start;
        while v < end {
            let p = kernel.translate(v).expect("página de KPTI sin mapear");
            t.map(v, p, PageSize::Size4K, f, alloc)
                .expect("no se pudo mapear KPTI");
            v += PAGE_SIZE;
        }
    }
    // SAFETY: `init` corre una vez, antes de crear cells (ver invariantes).
    unsafe { *TEMPLATE.0.get() = t.high_half() };
}

/// Las entradas 256..512 del PML4 de usuario.
pub fn template() -> [u64; 256] {
    // SAFETY: después de `init` solo se lee.
    unsafe { *TEMPLATE.0.get() }
}

/// Activa PCID si la CPU lo tiene (`CPUID.1:ECX[17]`). Con la raíz del
/// kernel cargada con identificador 0, como pide la CPU.
pub fn enable_pcid() -> bool {
    let (_, _, ecx, _) = super::cpu::cpuid(1, 0);
    if !WANT_PCID.load(Ordering::Relaxed) || ecx & (1 << 17) == 0 {
        return false;
    }
    // SAFETY: CPUID dice que hay PCID y CR3 tiene identificador 0 (la raíz
    // del kernel se cargó sin identificador); solo cambia cómo se etiqueta
    // la TLB.
    unsafe { super::cpu::write_cr4(super::cpu::read_cr4() | (1 << 17)) };
    PCID.store(true, Ordering::Relaxed);
    NSOS_NOFLUSH.store(NOFLUSH, Ordering::Relaxed);
    true
}

/// ¿Está activo PCID?
pub fn pcid() -> bool {
    PCID.load(Ordering::Relaxed)
}

/// Carga la raíz del kernel de la cell que va a correr y deja lista su raíz
/// de usuario para el stub de salida.
///
/// # Safety
/// `kernel` y `user` son las raíces de una cell viva.
pub unsafe fn enter_cell(kernel: PhysAddr, user: PhysAddr) {
    let mut t = TLB.lock();
    if !pcid() {
        NSOS_KERNEL_CR3.store(kernel, Ordering::Relaxed);
        NSOS_USER_CR3.store(user, Ordering::Relaxed);
        if t.current != kernel {
            t.current = kernel;
            // SAFETY: delegada al llamador.
            unsafe { super::cpu::write_cr3(kernel) };
        }
        return;
    }
    if t.current == kernel {
        return; // la misma cell: sus identificadores ya están cargados
    }
    // Si la cell que deja de correr tenía pendiente vaciar su identificador
    // de usuario (no llegó a volver a anillo 3), que quede anotado.
    let prev = NSOS_USER_CR3.load(Ordering::Relaxed);
    if prev != 0 && prev & NOFLUSH == 0 {
        let prev_kernel = NSOS_KERNEL_CR3.load(Ordering::Relaxed) & ROOT_MASK;
        t.asids.invalidate(prev_kernel, false, true);
    }
    let s = t.asids.switch(kernel);
    let (kid, uid) = (2 * s.slot as u64 + 1, 2 * s.slot as u64 + 2);
    NSOS_KERNEL_CR3.store(kernel | kid | NOFLUSH, Ordering::Relaxed);
    let keep_user = if s.flush_user { 0 } else { NOFLUSH };
    NSOS_USER_CR3.store(user | uid | keep_user, Ordering::Relaxed);
    let keep_kernel = if s.flush_kernel { 0 } else { NOFLUSH };
    t.current = kernel;
    // SAFETY: delegada al llamador; el identificador es de esta raíz.
    unsafe { super::cpu::write_cr3(kernel | kid | keep_kernel) };
}

/// Se cargó la raíz `root` con identificador 0 (la del kernel, o una de
/// prueba): `enter_cell` no puede suponer que sigue cargada la de una cell.
pub fn loaded_plain(root: PhysAddr) {
    TLB.lock().current = root;
}

/// Se desmapeó algo de las tablas de `root` (`invlpg` ya limpió el
/// identificador cargado): la próxima vez que se usen los otros, vaciarlos.
pub fn invalidate(root: PhysAddr) {
    if !pcid() {
        return;
    }
    let mut t = TLB.lock();
    if t.current == root {
        // La cell que corre: su raíz de usuario se carga vaciando.
        NSOS_USER_CR3.fetch_and(!NOFLUSH, Ordering::Relaxed);
        t.asids.invalidate(root, false, true);
    } else {
        t.asids.invalidate(root, true, true);
    }
}

/// Las tablas de `root` se liberan: su lugar queda libre.
pub fn forget(root: PhysAddr) {
    let mut t = TLB.lock();
    t.asids.forget(root);
    if t.current == root {
        t.current = 0;
    }
}
