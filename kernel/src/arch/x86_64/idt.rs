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
//! KPTI (`super::kpti`): los stubs están en `.kpti_text`, que ven las tablas
//! de usuario. Desde anillo 3 la CPU deja el marco en la pila de entrada
//! (`TSS.RSP0`); `nsos_trap_common` carga la raíz del kernel y copia el
//! marco a la pila de kernel del hilo. Al volver a anillo 3, copia el marco
//! de `iretq` a la pila de entrada y carga la raíz de usuario.
//!
//! # Invariantes (`unsafe`)
//! - Los stubs respetan el ABI de la CPU: la pila queda alineada a 16 antes
//!   del `call` (5 palabras de la CPU + 2 + 15 registros = 22 palabras).
//! - Lo que tocan los stubs antes de cambiar de raíz (la pila de entrada,
//!   `NSOS_KERNEL_CR3`, `NSOS_KSTACK_TOP`) está en `.kpti_data`.
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

.section .kpti_text, "ax"
.global nsos_trap_common
nsos_trap_common:
    // Desde anillo 3 (CS del marco: rsp+24), con las tablas de usuario y en
    // la pila de entrada: cargar las del kernel y pasar a la pila del hilo,
    // copiando rbx, rax y las 7 palabras del marco (72 bytes).
    test qword ptr [rsp + 24], 3
    jz nsos_trap_save
    push rax
    push rbx
    mov rax, cr3
    mov [rip + NSOS_ENTRY_CR3], rax
    mov rax, [rip + NSOS_KERNEL_CR3]
    mov cr3, rax
    mov rax, [rip + NSOS_KSTACK_TOP]
    sub rax, 72
    .irp off, 0, 8, 16, 24, 32, 40, 48, 56, 64
    mov rbx, [rsp + \off]
    mov [rax + \off], rbx
    .endr
    mov rsp, rax
    pop rbx
    pop rax
.global nsos_trap_save
nsos_trap_save:
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
    // Hacia anillo 3: copiar rbx, rax y el marco de `iretq` (7 palabras) a
    // la pila de entrada y cargar las tablas de usuario.
    test qword ptr [rsp + 8], 3
    jz 2f
    push rax
    push rbx
    lea rax, [rip + NSOS_ENTRY_STACK + {entry_size} - 56]
    .irp off, 0, 8, 16, 24, 32, 40, 48
    mov rbx, [rsp + \off]
    mov [rax + \off], rbx
    .endr
    mov rsp, rax
    mov rax, [rip + NSOS_USER_CR3]
    mov cr3, rax
    pop rbx
    pop rax
2:
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
    entry_size = const super::kpti::ENTRY_STACK_SIZE,
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

// En `.kpti_data`: la CPU la lee también con las tablas de usuario.
#[link_section = ".kpti_data"]
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
    if f.cs & 3 == 3 {
        super::kpti::check_entry();
    }
    // Una interrupción no limpia RFLAGS.AC: si la cell lo había puesto, SMAP
    // quedaría desactivado dentro del kernel.
    if super::paging::protections()[1].1 {
        // SAFETY: SMAP está soportado (se activó tras verificarlo con CPUID).
        unsafe { crate::arch::cpu::clac() };
    }
    super::trap::dispatch(f, frame)
}

/// Interfaz común con la de otras arquitecturas (ver `crate::arch`).
impl TrapFrame {
    /// Frame inicial de un hilo de kernel: `entry(arg)` con interrupciones.
    pub fn kernel_thread(entry: u64, arg: u64) -> Self {
        TrapFrame {
            rip: entry,
            cs: gdt::KERNEL_CS as u64,
            rflags: 0x202, // IF=1
            ss: gdt::KERNEL_DS as u64,
            rdi: arg,
            ..TrapFrame::default()
        }
    }

    /// Frame inicial del hilo de una cell: anillo 3 en `entry`, con la pila
    /// de usuario que termina en `stack_top`.
    pub fn user_thread(entry: u64, stack_top: u64) -> Self {
        TrapFrame {
            rip: entry,
            cs: gdt::USER_CS as u64,
            rflags: 0x202,
            rsp: stack_top - 8, // como si se hubiera hecho un `call`
            ss: gdt::USER_DS as u64,
            ..TrapFrame::default()
        }
    }

    /// ¿Se interrumpió (o vuelve) a una cell?
    pub fn is_user_mode(&self) -> bool {
        self.cs & 3 == 3
    }

    /// Número de syscall (`rax`).
    pub fn syscall_nr(&self) -> u64 {
        self.rax
    }

    /// Argumentos de la syscall (`rdi`, `rsi`, `rdx`, `r10`).
    pub fn syscall_args(&self) -> [u64; 4] {
        [self.rdi, self.rsi, self.rdx, self.r10]
    }

    /// Valor de retorno de la syscall (`rax`).
    pub fn set_syscall_ret(&mut self, v: u64) {
        self.rax = v;
    }

    /// Reescribe un frame de anillo 3 (guardado en `frame_addr`) para que
    /// continúe en anillo 0 en `pc`, sin interrupciones, en la misma pila de
    /// kernel.
    pub fn redirect_to_kernel(&mut self, pc: u64, frame_addr: u64) {
        let below = (frame_addr & !0xF) - 8; // como si se hubiera hecho un `call`
        *self = TrapFrame {
            rip: pc,
            cs: gdt::KERNEL_CS as u64,
            rflags: 0x2, // sin interrupciones, como una syscall
            rsp: below,
            ss: gdt::KERNEL_DS as u64,
            ..TrapFrame::default()
        };
    }
}

/// Dónde va el frame inicial de un hilo nuevo cuya pila de kernel termina
/// en `top` (alineado a 16). Para un hilo de kernel también fija su `rsp`.
pub fn initial_frame(top: u64, init: &mut TrapFrame) -> *mut TrapFrame {
    if !init.is_user_mode() {
        init.rsp = top - 8; // como si se hubiera hecho un `call`
    }
    (top - 16 - size_of::<TrapFrame>() as u64) as *mut TrapFrame
}
