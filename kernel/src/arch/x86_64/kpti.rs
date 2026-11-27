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
//! Sin PCID: cada cambio de raíz vacía la TLB.
//!
//! # Invariantes (`unsafe`)
//! - Un solo núcleo: la pila de entrada y las variables son globales.
//! - [`NSOS_USER_CR3`] y [`NSOS_KERNEL_CR3`] son las raíces de la cell del
//!   hilo que va a volver a anillo 3 (las fija el scheduler antes de
//!   reanudarlo).
//! - La plantilla se arma una vez en [`init`], antes de crear cells.

use super::paging::{flags, AddressSpace, PageSize, PAGE_SIZE};
use crate::mm::PhysAddr;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};

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
    let at_entry = NSOS_ENTRY_CR3.load(Ordering::Relaxed) & !0xFFF;
    if at_entry != NSOS_USER_CR3.load(Ordering::Relaxed) {
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

/// Fija las raíces de la cell que va a correr (las usan los stubs).
pub fn set_roots(kernel: PhysAddr, user: PhysAddr) {
    NSOS_KERNEL_CR3.store(kernel, Ordering::Relaxed);
    NSOS_USER_CR3.store(user, Ordering::Relaxed);
}
