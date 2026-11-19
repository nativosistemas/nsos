//! IDT y puntos de entrada de interrupciones/excepciones.
//!
//! Cada vector tiene un stub en ensamblador que normaliza la pila (empuja un
//! código de error 0 si la CPU no lo hace, y el número de vector), guarda los
//! registros generales y llama a `trap_entry` con un puntero al
//! [`TrapFrame`]. `trap_entry` devuelve el `TrapFrame` desde el que hay que
//! **reanudar**: si es otro, eso es un cambio de contexto. Así el scheduler
//! reutiliza el mismo camino que cualquier interrupción, sin código especial
//! de context switch.
//!
//! # Invariantes (`unsafe`)
//! - Los stubs respetan el ABI de la CPU: la pila queda alineada a 16 antes
//!   del `call` (5 palabras de la CPU + 2 + 15 registros = 22 palabras).
//! - El `TrapFrame` devuelto por `trap_entry` debe ser uno guardado por un
//!   stub (o fabricado por el scheduler con el mismo layout) en una pila que
//!   siga viva.
//! - La IDT es `static` y se escribe solo en `init`.

use super::gdt;
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::mem::size_of;

/// Registros guardados en una interrupción (orden en memoria).
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    // Empujados por la CPU:
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

const _: () = assert!(size_of::<TrapFrame>() == 22 * 8);

/// Cantidad de vectores con stub (0..NUM_STUBS).
pub const NUM_STUBS: usize = 256;

// Stubs de entrada. Los vectores 8, 10-14, 17, 21, 29 y 30 traen código de
// error de la CPU; para el resto se empuja un 0 y así el frame es uniforme.
global_asm!(
    r#"
.altmacro
.macro ISR n, err
    .global nsos_isr_\n
    .align 16
nsos_isr_\n:
    .if \err == 0
    push 0
    .endif
    push \n
    jmp nsos_trap_common
.endm

.macro ISR_ADDR n
    .quad nsos_isr_\n
.endm

.section .text
.global nsos_trap_common
nsos_trap_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    cld
    mov rdi, rsp
    call {entry}
    mov rsp, rax
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16
    iretq

.set i, 0
.rept 256
    .if i == 8 || i == 10 || i == 11 || i == 12 || i == 13 || i == 14 || i == 17 || i == 21 || i == 29 || i == 30
        ISR %i, 1
    .else
        ISR %i, 0
    .endif
    .set i, i + 1
.endr

.section .rodata
.global nsos_isr_table
.align 8
nsos_isr_table:
.set i, 0
.rept 256
    ISR_ADDR %i
    .set i, i + 1
.endr
.section .text
.noaltmacro
"#,
    entry = sym trap_entry,
);

extern "C" {
    static nsos_isr_table: [u64; NUM_STUBS];
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Gate {
    off_lo: u16,
    sel: u16,
    ist: u8,
    attr: u8,
    off_mid: u16,
    off_hi: u32,
    _r: u32,
}

impl Gate {
    const EMPTY: Gate = Gate {
        off_lo: 0,
        sel: 0,
        ist: 0,
        attr: 0,
        off_mid: 0,
        off_hi: 0,
        _r: 0,
    };

    fn new(handler: u64, ist: u8, dpl: u8) -> Gate {
        Gate {
            off_lo: handler as u16,
            sel: gdt::KERNEL_CS,
            ist,
            // presente | DPL | interrupt gate (0xE): IF se limpia al entrar
            attr: 0x80 | (dpl << 5) | 0x0E,
            off_mid: (handler >> 16) as u16,
            off_hi: (handler >> 32) as u32,
            _r: 0,
        }
    }
}

#[repr(C, align(16))]
struct Idt(UnsafeCell<[Gate; 256]>);
// SAFETY: solo `init` escribe, antes de habilitar interrupciones.
unsafe impl Sync for Idt {}

static IDT: Idt = Idt(UnsafeCell::new([Gate::EMPTY; 256]));

/// Vector de la interrupción de software para ceder la CPU.
pub const YIELD_VECTOR: u8 = 0x81;

/// Instala la IDT.
pub fn init() {
    // SAFETY: ver invariantes del módulo; la tabla de stubs la genera el
    // ensamblador de arriba y tiene exactamente 256 entradas.
    unsafe {
        let idt = &mut *IDT.0.get();
        let table = &*core::ptr::addr_of!(nsos_isr_table);
        for (v, gate) in idt.iter_mut().enumerate() {
            let ist = if v == 8 { gdt::IST_DOUBLE_FAULT } else { 0 };
            *gate = Gate::new(table[v], ist, 0);
        }
        #[repr(C, packed)]
        struct Ptr {
            limit: u16,
            base: u64,
        }
        let ptr = Ptr {
            limit: (size_of::<[Gate; 256]>() - 1) as u16,
            base: idt.as_ptr() as u64,
        };
        asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
    }
}

/// Punto de entrada común desde los stubs.
extern "C" fn trap_entry(frame: *mut TrapFrame) -> *mut TrapFrame {
    // SAFETY: el stub pasa un puntero a un TrapFrame recién guardado en la
    // pila actual, válido y exclusivo durante esta llamada.
    let f = unsafe { &mut *frame };
    crate::trap::dispatch(f, frame)
}
