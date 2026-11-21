//! Despacho de interrupciones y excepciones de x86_64.

use super::apic;
use super::cpu;
use super::idt::{TrapFrame, YIELD_VECTOR};
use super::syscall::SYSCALL_VECTOR;
use crate::sched;

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

/// Atiende una interrupción. Devuelve el frame desde el que reanudar.
pub fn dispatch(f: &mut TrapFrame, raw: *mut TrapFrame) -> *mut TrapFrame {
    crate::trap::finish(handle(f, raw))
}

fn handle(f: &mut TrapFrame, raw: *mut TrapFrame) -> *mut TrapFrame {
    match f.vector {
        // Excepción en anillo 3: es culpa de la cell, no del kernel.
        0..=31 if f.cs & 3 == 3 => {
            let addr = if f.vector == 14 { cpu::read_cr2() } else { 0 };
            crate::syscall::user_fault(f.vector, f.rip, addr, raw)
        }
        v if v == SYSCALL_VECTOR => {
            crate::syscall::handle(f);
            raw
        }
        3 => {
            crate::trap::note_breakpoint();
            raw
        }
        0..=31 => fatal_exception(f),
        v if v == apic::TIMER_VECTOR as u64 => {
            apic::on_tick();
            apic::eoi();
            sched::schedule(raw)
        }
        v if v == YIELD_VECTOR as u64 => sched::schedule(raw),
        v if (super::ioapic::VECTOR_BASE as u64..super::ioapic::VECTOR_BASE as u64 + 24)
            .contains(&v) =>
        {
            crate::irq::on_irq((v - super::ioapic::VECTOR_BASE as u64) as u32);
            apic::eoi();
            sched::schedule(raw)
        }
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
    // Los fallos de cells se desvían antes (ver `handle`): llegar aquí es
    // un bug del kernel.
    let extra = if f.vector == 14 {
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
