//! Instrucciones privilegiadas de la CPU (registros de control, MSR, TSC,
//! interrupciones).
//!
//! # Invariantes (`unsafe`)
//! Las funciones de solo lectura (`read_cr2`, `read_cr3`, `rdtsc`, `rflags`)
//! no tienen efectos y son seguras. Las que modifican estado global de la CPU
//! (`write_cr3`, `wrmsr`, `write_cr0`, `write_cr4`, `clac`) son `unsafe`: el llamador garantiza que
//! el nuevo estado mantiene válido el código y la pila en ejecución.

use core::arch::asm;

/// Lee CR2 (dirección que causó el último fallo de página).
pub fn read_cr2() -> u64 {
    let v;
    // SAFETY: lectura de un registro de control sin efectos secundarios.
    unsafe { asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Lee CR3 (raíz de las tablas de páginas activa).
pub fn read_cr3() -> u64 {
    let v;
    // SAFETY: ídem.
    unsafe { asm!("mov {}, cr3", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Cambia la raíz de las tablas de páginas.
///
/// # Safety
/// `pml4_phys` debe apuntar a una jerarquía válida que mapee el código, la
/// pila y los datos que el kernel está usando.
pub unsafe fn write_cr3(pml4_phys: u64) {
    // SAFETY: delegada al llamador.
    unsafe { asm!("mov cr3, {}", in(reg) pml4_phys, options(nostack, preserves_flags)) };
}

/// Lee CR0.
pub fn read_cr0() -> u64 {
    let v;
    // SAFETY: lectura sin efectos.
    unsafe { asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Escribe CR0.
///
/// # Safety
/// No debe desactivar paginación ni modo protegido.
pub unsafe fn write_cr0(v: u64) {
    // SAFETY: delegada al llamador.
    unsafe { asm!("mov cr0, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// Lee CR4.
pub fn read_cr4() -> u64 {
    let v;
    // SAFETY: lectura sin efectos.
    unsafe { asm!("mov {}, cr4", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Escribe CR4.
///
/// # Safety
/// Solo puede activar bits que la CPU soporta (si no, #GP) y que no
/// invaliden el código en ejecución.
pub unsafe fn write_cr4(v: u64) {
    // SAFETY: delegada al llamador.
    unsafe { asm!("mov cr4, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// `cpuid(leaf, subleaf)` → `(eax, ebx, ecx, edx)`.
pub fn cpuid(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    let r = core::arch::x86_64::__cpuid_count(leaf, subleaf);
    (r.eax, r.ebx, r.ecx, r.edx)
}

/// `clac`: vuelve a prohibir el acceso del kernel a páginas de usuario.
///
/// # Safety
/// Solo con SMAP soportado (si no, #UD).
pub unsafe fn clac() {
    // SAFETY: delegada al llamador.
    unsafe { asm!("clac", options(nomem, nostack)) };
}

/// Lee un MSR.
///
/// # Safety
/// `msr` debe existir en esta CPU (si no, #GP).
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: delegada al llamador.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    };
    ((hi as u64) << 32) | lo as u64
}

/// Escribe un MSR.
///
/// # Safety
/// `msr` debe existir y el valor no debe romper invariantes del kernel.
pub unsafe fn wrmsr(msr: u32, v: u64) {
    // SAFETY: delegada al llamador.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") v as u32, in("edx") (v >> 32) as u32, options(nostack, preserves_flags))
    };
}

/// Contador de ciclos.
pub fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: rdtsc no tiene efectos.
    unsafe {
        asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    };
    ((hi as u64) << 32) | lo as u64
}

/// RFLAGS actual.
pub fn rflags() -> u64 {
    let v;
    // SAFETY: pushfq/pop solo usa la pila propia.
    unsafe { asm!("pushfq", "pop {}", out(reg) v, options(nomem, preserves_flags)) };
    v
}

/// `true` si las interrupciones están habilitadas.
pub fn interrupts_enabled() -> bool {
    rflags() & (1 << 9) != 0
}

/// Habilita interrupciones.
pub fn enable_interrupts() {
    // SAFETY: `sti` solo cambia IF; los handlers ya están instalados antes de
    // la primera llamada (ver `arch::init`).
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Deshabilita interrupciones.
pub fn disable_interrupts() {
    // SAFETY: `cli` solo cambia IF.
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Ejecuta `f` con interrupciones deshabilitadas y restaura el estado previo.
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let was = interrupts_enabled();
    disable_interrupts();
    let r = f();
    if was {
        enable_interrupts();
    }
    r
}

/// Espera la próxima interrupción (habilitándolas atómicamente).
pub fn wait_for_interrupt() {
    // SAFETY: `sti; hlt` es atómico respecto a interrupciones.
    unsafe { asm!("sti", "hlt", options(nomem, nostack)) };
}

/// Barrera de especulación (Spectre v1): ninguna instrucción posterior se
/// ejecuta, ni especulativamente, hasta que terminen las anteriores
/// (incluidas las comparaciones de un control de rango). También es barrera
/// del compilador: sin `nomem`, no mueve accesos a memoria de un lado al otro.
pub fn speculation_barrier() {
    // SAFETY: `lfence` solo ordena la ejecución; no cambia estado.
    unsafe { asm!("lfence", options(nostack, preserves_flags)) };
}

/// Invalida la entrada TLB de una dirección.
pub fn invlpg(addr: u64) {
    // SAFETY: invalidar TLB nunca rompe invariantes (solo cuesta rendimiento).
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags)) };
}

/// MSRs usados por el kernel.
pub mod msr {
    /// Base del APIC local.
    pub const IA32_APIC_BASE: u32 = 0x1B;
    /// Extended Feature Enable Register.
    pub const IA32_EFER: u32 = 0xC000_0080;
}
