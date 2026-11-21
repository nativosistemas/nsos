//! Vectores de excepción de ARM64 y `TrapFrame`.
//!
//! Las 16 entradas de `VBAR_EL1` reservan un `TrapFrame` en la pila actual
//! (`SP_EL1`), guardan `x0`/`x1`, anotan qué entrada fue y saltan a la rutina
//! común, que guarda el resto y llama a [`trap_entry`]. Como en x86_64, el
//! frame devuelto es el que se **reanuda**: si es otro, eso es un cambio de
//! contexto.
//!
//! # Invariantes (`unsafe`)
//! - El kernel corre siempre con `SPSel` = 1 (`SP_EL1`), así una excepción
//!   desde EL1 usa la pila del hilo actual.
//! - Al volver a EL0, `SP_EL1` queda en el tope de la pila de kernel del
//!   hilo: el frame de un hilo en EL0 está siempre justo debajo del tope
//!   ([`initial_frame`] lo pone ahí, y una excepción desde EL0 lo guarda
//!   ahí porque llega con `SP_EL1` en el tope). Por eso no hace falta un
//!   equivalente del TSS.
//! - El frame devuelto por `trap_entry` debe ser uno guardado por esta ruta
//!   (o fabricado con el mismo layout) en una pila que siga viva.

use super::{gic, time};
use crate::sched;
use core::arch::global_asm;
use core::mem::size_of;

/// Registros guardados en una excepción.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct TrapFrame {
    /// `x0`..`x30`.
    pub x: [u64; 31],
    /// Pila de EL0.
    pub sp_el0: u64,
    /// Dirección de retorno.
    pub elr: u64,
    /// Estado guardado (`SPSR_EL1`).
    pub spsr: u64,
    /// Síndrome (`ESR_EL1`).
    pub esr: u64,
    /// Dirección que falló (`FAR_EL1`).
    pub far: u64,
    /// Entrada del vector (0..16).
    pub kind: u64,
    _pad: u64,
}

const FRAME_SIZE: usize = size_of::<TrapFrame>();
const _: () = assert!(FRAME_SIZE == 38 * 8 && FRAME_SIZE.is_multiple_of(16));

/// `SPSR` de un hilo de kernel: EL1h, interrupciones habilitadas.
const SPSR_EL1H: u64 = 0b0101;
/// Máscara DAIF en `SPSR`.
const SPSR_DAIF: u64 = 0xF << 6;
/// `PSTATE.PAN` en `SPSR`.
const SPSR_PAN: u64 = 1 << 22;

/// Clases de excepción (`ESR_EL1.EC`).
mod ec {
    pub const SVC64: u64 = 0x15;
    pub const IABT_LOWER: u64 = 0x20;
    pub const IABT_SAME: u64 = 0x21;
    pub const DABT_LOWER: u64 = 0x24;
    pub const DABT_SAME: u64 = 0x25;
    pub const BRK64: u64 = 0x3C;
}

/// Código de excepción de un fallo de página de una cell
/// (`ExitStatus::Faulted`): data abort desde EL0.
pub const FAULT_PAGE: u64 = ec::DABT_LOWER;

fn pan_bit() -> u64 {
    if super::paging::pan_enabled() {
        SPSR_PAN
    } else {
        0
    }
}

/// Interfaz común con la de otras arquitecturas (ver `crate::arch`).
impl TrapFrame {
    /// Frame inicial de un hilo de kernel: `entry(arg)` con interrupciones.
    pub fn kernel_thread(entry: u64, arg: u64) -> Self {
        let mut f = TrapFrame {
            elr: entry,
            spsr: SPSR_EL1H | pan_bit(),
            ..TrapFrame::default()
        };
        f.x[0] = arg;
        f
    }

    /// Frame inicial del hilo de una cell: EL0 en `entry`, con la pila que
    /// termina en `stack_top` (alineado a 16).
    pub fn user_thread(entry: u64, stack_top: u64) -> Self {
        TrapFrame {
            elr: entry,
            spsr: 0, // EL0t, interrupciones habilitadas
            sp_el0: stack_top,
            ..TrapFrame::default()
        }
    }

    /// ¿Se interrumpió (o vuelve) a una cell?
    pub fn is_user_mode(&self) -> bool {
        self.spsr & 0xF == 0
    }

    /// Número de syscall (`x8`).
    pub fn syscall_nr(&self) -> u64 {
        self.x[8]
    }

    /// Argumentos de la syscall (`x0`..`x3`).
    pub fn syscall_args(&self) -> [u64; 4] {
        [self.x[0], self.x[1], self.x[2], self.x[3]]
    }

    /// Valor de retorno de la syscall (`x0`).
    pub fn set_syscall_ret(&mut self, v: u64) {
        self.x[0] = v;
    }

    /// Reescribe un frame de EL0 para que continúe en EL1 en `pc`, sin
    /// interrupciones. Un frame de EL0 está en el tope de la pila de kernel,
    /// así que el hilo sigue con la pila vacía.
    pub fn redirect_to_kernel(&mut self, pc: u64, _frame_addr: u64) {
        *self = TrapFrame {
            elr: pc,
            spsr: SPSR_EL1H | SPSR_DAIF | pan_bit(),
            ..TrapFrame::default()
        };
    }
}

/// Dónde va el frame inicial de un hilo nuevo cuya pila de kernel termina
/// en `top` (alineado a 16): justo debajo del tope (ver invariantes).
pub fn initial_frame(top: u64, _init: &mut TrapFrame) -> *mut TrapFrame {
    (top - FRAME_SIZE as u64) as *mut TrapFrame
}

global_asm!(
    r#"
.macro VECTOR kind
    .balign 0x80
    sub sp, sp, #{size}
    stp x0, x1, [sp, #0]
    mov x1, #\kind
    b nsos_trap_common
.endm

.section .text
.balign 2048
.global nsos_vectors
nsos_vectors:
    VECTOR 0
    VECTOR 1
    VECTOR 2
    VECTOR 3
    VECTOR 4
    VECTOR 5
    VECTOR 6
    VECTOR 7
    VECTOR 8
    VECTOR 9
    VECTOR 10
    VECTOR 11
    VECTOR 12
    VECTOR 13
    VECTOR 14
    VECTOR 15

nsos_trap_common:
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    mrs x2, sp_el0
    stp x30, x2, [sp, #240]
    mrs x2, elr_el1
    mrs x3, spsr_el1
    stp x2, x3, [sp, #256]
    mrs x2, esr_el1
    mrs x3, far_el1
    stp x2, x3, [sp, #272]
    str x1, [sp, #288]
    mov x0, sp
    bl {entry}
    mov sp, x0
    ldp x2, x3, [sp, #256]
    msr elr_el1, x2
    msr spsr_el1, x3
    ldp x30, x2, [sp, #240]
    msr sp_el0, x2
    ldp x0, x1, [sp, #0]
    ldp x2, x3, [sp, #16]
    ldp x4, x5, [sp, #32]
    ldp x6, x7, [sp, #48]
    ldp x8, x9, [sp, #64]
    ldp x10, x11, [sp, #80]
    ldp x12, x13, [sp, #96]
    ldp x14, x15, [sp, #112]
    ldp x16, x17, [sp, #128]
    ldp x18, x19, [sp, #144]
    ldp x20, x21, [sp, #160]
    ldp x22, x23, [sp, #176]
    ldp x24, x25, [sp, #192]
    ldp x26, x27, [sp, #208]
    ldp x28, x29, [sp, #224]
    add sp, sp, #{size}
    eret
"#,
    size = const FRAME_SIZE,
    entry = sym trap_entry,
);

extern "C" {
    static nsos_vectors: u8;
}

/// Instala los vectores de excepción.
pub fn init() {
    // SAFETY: `nsos_vectors` está alineado a 2 KiB y tiene las 16 entradas
    // de 0x80 bytes que exige `VBAR_EL1`.
    unsafe {
        core::arch::asm!(
            "msr vbar_el1, {}",
            "isb",
            in(reg) core::ptr::addr_of!(nsos_vectors) as u64,
            options(nostack),
        );
    }
}

/// Punto de entrada común desde los vectores.
extern "C" fn trap_entry(frame: *mut TrapFrame) -> *mut TrapFrame {
    // SAFETY: la rutina común pasa un puntero a un TrapFrame recién guardado
    // en la pila actual, válido y exclusivo durante esta llamada.
    let f = unsafe { &mut *frame };
    crate::trap::finish(handle(f, frame))
}

fn handle(f: &mut TrapFrame, raw: *mut TrapFrame) -> *mut TrapFrame {
    let class = f.esr >> 26;
    match f.kind {
        // Síncrona desde EL1: `svc` del propio kernel (ceder la CPU) o `brk`.
        4 if class == ec::SVC64 => sched::schedule(raw),
        4 if class == ec::BRK64 => {
            crate::trap::note_breakpoint();
            f.elr += 4;
            raw
        }
        // IRQ desde EL1 o desde EL0.
        5 | 9 => irq(raw),
        // Síncrona desde EL0: syscall, o una excepción que es culpa de la cell.
        8 if class == ec::SVC64 => {
            crate::syscall::handle(f);
            raw
        }
        8 => {
            let addr = if matches!(class, ec::DABT_LOWER | ec::IABT_LOWER) {
                f.far
            } else {
                0
            };
            crate::syscall::user_fault(class, f.elr, addr, raw)
        }
        // FIQ, SError o AArch32 desde EL0: la cell no sigue.
        10..=15 => crate::syscall::user_fault(0x100 | f.kind, f.elr, 0, raw),
        _ => fatal_exception(f),
    }
}

fn irq(raw: *mut TrapFrame) -> *mut TrapFrame {
    let id = gic::ack();
    match id {
        gic::SPURIOUS => raw,
        time::TIMER_INTID => {
            time::on_tick();
            gic::eoi(id);
            sched::schedule(raw)
        }
        id if gic::valid_spi(id) => {
            crate::irq::on_irq(id);
            gic::eoi(id);
            sched::schedule(raw)
        }
        _ => {
            crate::kprintln!("interrupción inesperada: INTID {}", id);
            gic::eoi(id);
            raw
        }
    }
}

fn fatal_exception(f: &TrapFrame) -> ! {
    let class = f.esr >> 26;
    let why = match class {
        ec::DABT_SAME => {
            let dfsc = f.esr & 0x3F;
            let write = f.esr & (1 << 6) != 0;
            match (dfsc & 0x3C, write) {
                (0x04, false) => "lectura de página no presente",
                (0x04, true) => "escritura en página no presente",
                (0x0C, false) => "lectura sin permiso",
                (0x0C, true) => "escritura sin permiso",
                _ => "data abort",
            }
        }
        ec::IABT_SAME => "ejecución",
        _ => "",
    };
    // Los fallos de cells se desvían antes (ver `handle`): llegar aquí es
    // un bug del kernel.
    panic!(
        "excepción {} (EC={:#x}, ESR={:#x}) en pc={:#x} far={:#x} {}\n{:#x?}",
        f.kind, class, f.esr, f.elr, f.far, why, f
    );
}
