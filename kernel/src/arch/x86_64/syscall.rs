//! Entrada de syscalls (`syscall`) desde anillo 3.
//!
//! El stub cambia a la pila de kernel del hilo actual y construye un
//! `TrapFrame` con el mismo layout que una interrupción (vector
//! [`SYSCALL_VECTOR`]), así toda la lógica pasa por `trap::dispatch` y una
//! syscall que bloquea es un cambio de contexto normal. Se vuelve siempre con
//! `iretq`: más lento que `sysret` pero uniforme, y evita la trampa de
//! `sysret` con direcciones no canónicas.
//!
//! Las syscalls corren con interrupciones deshabilitadas (SFMASK limpia IF):
//! el kernel no es apropiable dentro de una syscall.
//!
//! # Invariantes (`unsafe`)
//! - Un solo núcleo: `KSTACK_TOP` y `USER_RSP` son globales. Con SMP pasarán
//!   a ser por CPU (vía `swapgs`).
//! - `KSTACK_TOP` siempre vale el tope de la pila de kernel del hilo actual
//!   cuando ese hilo está en anillo 3 (lo fija el scheduler).

use super::cpu;
use super::gdt;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// Valor de `vector` en el `TrapFrame` de una syscall.
pub const SYSCALL_VECTOR: u64 = 0x80;

#[no_mangle]
static NSOS_KSTACK_TOP: AtomicU64 = AtomicU64::new(0);
#[no_mangle]
static NSOS_USER_RSP: AtomicU64 = AtomicU64::new(0);

const MSR_STAR: u32 = 0xC000_0081;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_SFMASK: u32 = 0xC000_0084;

global_asm!(
    r#"
.section .text
.global nsos_syscall_entry
nsos_syscall_entry:
    mov [rip + NSOS_USER_RSP], rsp
    mov rsp, [rip + NSOS_KSTACK_TOP]
    push {user_ds}
    push [rip + NSOS_USER_RSP]
    push r11
    push {user_cs}
    push rcx
    push 0
    push {vector}
    jmp nsos_trap_common
"#,
    user_ds = const gdt::USER_DS as u64,
    user_cs = const gdt::USER_CS as u64,
    vector = const SYSCALL_VECTOR,
);

extern "C" {
    fn nsos_syscall_entry();
}

/// Habilita `syscall` y configura sus MSR.
pub fn init() {
    // SAFETY: MSRs arquitecturales de x86_64; STAR usa los selectores de la
    // GDT (kernel 0x08/0x10, base de usuario 0x10 → SS 0x18, CS 0x20).
    unsafe {
        let efer = cpu::rdmsr(cpu::msr::IA32_EFER);
        cpu::wrmsr(cpu::msr::IA32_EFER, efer | 1); // SCE
        cpu::wrmsr(
            MSR_STAR,
            ((0x10u64) << 48) | ((gdt::KERNEL_CS as u64) << 32),
        );
        cpu::wrmsr(MSR_LSTAR, nsos_syscall_entry as *const () as u64);
        // Limpiar TF, IF, DF y AC: con AC=1 heredado de la cell, SMAP no
        // protegería las páginas de usuario durante la syscall.
        cpu::wrmsr(MSR_SFMASK, 0x4_0700);
    }
}

/// Fija la pila de kernel para entradas desde anillo 3 (syscall y TSS).
pub fn set_kernel_stack(top: u64) {
    NSOS_KSTACK_TOP.store(top, Ordering::Relaxed);
    gdt::set_kernel_stack(top);
}
