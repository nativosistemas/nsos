//! Despacho de interrupciones y excepciones (independiente del ensamblador).

use crate::arch::apic;
use crate::arch::cpu;
use crate::arch::idt::{TrapFrame, YIELD_VECTOR};
use crate::arch::syscall::SYSCALL_VECTOR;
use crate::sched;
use core::sync::atomic::{AtomicU64, Ordering};

static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);

const EXCEPTIONS: [&str; 32] = [
    "división por cero (#DE)",
    "debug (#DB)",
    "NMI",
    "breakpoint (#BP)",
    "overflow (#OF)",
    "bound range (#BR)",
    "opcode inválido (#UD)",
    "dispositivo no disponible (#NM)",
    "doble fallo (#DF)",
    "coprocessor segment overrun",
    "TSS inválido (#TS)",
    "segmento no presente (#NP)",
    "fallo de pila (#SS)",
    "protección general (#GP)",
    "fallo de página (#PF)",
    "reservado",
    "x87 (#MF)",
    "alineación (#AC)",
    "machine check (#MC)",
    "SIMD (#XM)",
    "virtualización (#VE)",
    "control protection (#CP)",
    "reservado",
    "reservado",
    "reservado",
    "reservado",
    "reservado",
    "reservado",
    "hypervisor injection (#HV)",
    "VMM communication (#VC)",
    "seguridad (#SX)",
    "reservado",
];

/// Cantidad de breakpoints (`int3`) atendidos; lo usan los tests.
pub fn breakpoints() -> u64 {
    BREAKPOINTS.load(Ordering::Relaxed)
}

/// Atiende una interrupción. Devuelve el frame desde el que reanudar.
pub fn dispatch(f: &mut TrapFrame, raw: *mut TrapFrame) -> *mut TrapFrame {
    match f.vector {
        // Excepción en anillo 3: es culpa de la cell, no del kernel.
        0..=31 if f.cs & 3 == 3 => crate::syscall::user_fault(f, raw),
        v if v == SYSCALL_VECTOR => {
            crate::syscall::handle(f);
            raw
        }
        3 => {
            BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
            raw
        }
        0..=31 => fatal_exception(f),
        v if v == apic::TIMER_VECTOR as u64 => {
            apic::on_tick();
            apic::eoi();
            sched::schedule(raw)
        }
        v if v == YIELD_VECTOR as u64 => sched::schedule(raw),
        v if v == apic::SPURIOUS_VECTOR as u64 => raw,
        v => {
            crate::kprintln!("interrupción inesperada: vector {}", v);
            apic::eoi();
            raw
        }
    }
}

fn fatal_exception(f: &TrapFrame) -> ! {
    let name = EXCEPTIONS[f.vector as usize];
    // Los fallos de cells se desvían antes (ver `dispatch`): llegar aquí es
    // un bug del kernel.
    let extra = if f.vector == 14 {
        // SAFETY de formato solamente: CR2 se lee sin efectos.
        let cr2 = cpu::read_cr2();
        let why = match (f.error_code & 1, f.error_code & 2, f.error_code & 16) {
            (_, _, 16) => "ejecución",
            (0, 0, _) => "lectura de página no presente",
            (0, _, _) => "escritura en página no presente",
            (_, 0, _) => "lectura sin permiso",
            _ => "escritura sin permiso",
        };
        (cr2, why)
    } else {
        (0, "")
    };
    panic!(
        "excepción {}: {} (error={:#x}) rip={:#x} rsp={:#x} cr2={:#x} {}\n{:#x?}",
        f.vector, name, f.error_code, f.rip, f.rsp, extra.0, extra.1, f
    );
}
