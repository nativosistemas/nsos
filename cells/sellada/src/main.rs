//! Cell `sellada`: prueba negativa de que las cells están selladas.
//!
//! Una cell solo ejecuta el código de su imagen ELF: ningún objeto `Memory`
//! se mapea ejecutable (`Mem.step_sealed` en el modelo, Kani:
//! `map_request_never_exec`). Arranca sin capacidades, crea un objeto de
//! memoria, lo mapea escribible y escribe una instrucción `ret`. Comprueba que
//! el kernel no deja mapear ese objeto (ni una copia de su capacidad) como
//! ejecutable, y que ninguna capacidad de memoria tiene `EXEC`. Al final salta
//! a la página escrita: como es NX, la CPU da un fallo de página (x86_64) o un
//! instruction abort (ARM64) y el kernel termina la cell (lo verifica ktest).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::Syscall;
use nsos_rt::abi::{error::decode_ret, CapHandle, Error, Rights};
use nsos_rt::{cap_dup, cap_info, mem_create, mem_map, println, raw_syscall};

/// Donde se escribe el código (lo comprueba ktest en la dirección del fallo).
const VADDR: u64 = 0x5a00_0000;
/// Donde se intenta mapear el mismo objeto como ejecutable.
const VADDR_X: u64 = 0x5a10_0000;

/// `ret`.
#[cfg(target_arch = "x86_64")]
const RET: &[u8] = &[0xc3];
#[cfg(target_arch = "aarch64")]
const RET: &[u8] = &0xd65f_03c0u32.to_le_bytes();

nsos_rt::entry!(main);

fn map_raw(h: CapHandle, vaddr: u64, rights: Rights) -> Result<u64, Error> {
    decode_ret(raw_syscall(
        Syscall::MemMap as u64,
        h.0 as u64,
        vaddr,
        rights.bits() as u64,
        0,
    ))
}

fn check(name: &str, ok: bool) -> bool {
    println!(
        "sellada: {:<52} {}",
        name,
        if ok { "ok" } else { "¡FALLÓ!" }
    );
    ok
}

fn no_exec(h: CapHandle) -> bool {
    cap_info(h).is_ok_and(|i| i.rights & Rights::EXEC.bits() == 0)
}

fn run() -> Result<bool, Error> {
    let (r, w, x) = (Rights::READ, Rights::WRITE, Rights::EXEC);
    let mut ok = true;
    let h = mem_create(4096)?;
    let m = mem_map(h, VADDR, r | w)?;
    m.write_bytes(0, RET);
    ok &= check("mem_create no da EXEC", no_exec(h));
    ok &= check(
        "mapear RX el objeto que tengo RW",
        map_raw(h, VADDR_X, r | x) == Err(Error::InvalidArgs),
    );
    ok &= check(
        "mapear RWX",
        map_raw(h, VADDR_X, r | w | x) == Err(Error::InvalidArgs),
    );
    // `cap_dup` solo atenúa: pedir EXEC no lo agrega.
    let d = cap_dup(h, r | w | x | Rights::DUPLICATE)?;
    ok &= check("cap_dup pidiendo EXEC no lo agrega", no_exec(d));
    ok &= check(
        "mapear RX una copia de la capacidad",
        map_raw(d, VADDR_X, r | x) == Err(Error::InvalidArgs),
    );
    ok &= check(
        "nada quedó mapeado en la dirección del intento",
        map_raw(d, VADDR_X, r) == Ok(VADDR_X),
    );
    Ok(ok)
}

fn main() -> i32 {
    match run() {
        Ok(true) => {}
        Ok(false) => return 1,
        Err(e) => {
            println!("sellada: ¡error inesperado! {:?}", e);
            return 2;
        }
    }
    println!("sellada: ahora salto al código que escribí...");
    let f = core::hint::black_box(VADDR);
    #[allow(unsafe_code)]
    // SAFETY: no lo es, a propósito: es la prueba negativa de NX. La página
    // está mapeada sin permiso de ejecución, así que la CPU no ejecuta nada
    // y el kernel termina la cell.
    let code: extern "C" fn() = unsafe { core::mem::transmute(f as usize) };
    code();
    println!("sellada: ¡EJECUTÉ CÓDIGO QUE ESCRIBÍ!");
    3
}
