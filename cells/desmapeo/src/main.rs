//! Cell `desmapeo`: pruebas de `mem_unmap` (hito 13).
//!
//! Arranca sin capacidades. Crea un objeto de memoria de dos páginas, lo
//! mapea y escribe. Comprueba que `mem_unmap` rechaza todo lo que no sea un
//! objeto entero mapeado por ella (medio objeto, la segunda página, su
//! código, la pila, el kernel, una dirección libre, largo 0), que después de
//! desmapear la misma dirección se puede volver a mapear y el contenido
//! sigue ahí (la capacidad no se tocó), y que un segundo `mem_unmap` del
//! mismo rango falla. Al final lee la dirección desmapeada: la MMU tiene que
//! dar un fallo de página y el kernel terminar la cell (lo verifica ktest).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::Syscall;
use nsos_rt::abi::{error::decode_ret, Error, Rights};
use nsos_rt::{mem_create, mem_map, mem_unmap, println, raw_syscall};

/// Donde se mapea el objeto (lo comprueba ktest en la dirección del fallo).
const VADDR: u64 = 0x5800_0000;
const SIZE: u64 = 2 * 4096;
const KERNEL_ADDR: u64 = 0xffff_ffff_8000_0000;
const MY_CODE: u64 = 0x40_0000;

nsos_rt::entry!(main);

fn unmap_raw(vaddr: u64, len: u64) -> Result<u64, Error> {
    decode_ret(raw_syscall(Syscall::MemUnmap as u64, vaddr, len, 0, 0))
}

fn check(name: &str, ok: bool) -> bool {
    println!(
        "desmapeo: {:<46} {}",
        name,
        if ok { "ok" } else { "¡FALLÓ!" }
    );
    ok
}

fn run() -> Result<bool, Error> {
    let mut ok = true;
    let h = mem_create(SIZE as usize)?;
    let m = mem_map(h, VADDR, Rights::READ | Rights::WRITE)?;
    m.write_u64(8, 0x6e73_6f73);
    let stack = core::hint::black_box(&ok) as *const bool as u64 & !0xFFF;
    for (name, v, l) in [
        ("medio objeto", VADDR, 4096),
        ("solo la segunda página", VADDR + 4096, 4096),
        ("de más", VADDR, 3 * 4096),
        ("largo 0", VADDR, 0),
        ("desalineado", VADDR + 8, SIZE),
        ("mi código", MY_CODE, 4096),
        ("mi pila", stack, 4096),
        ("el kernel", KERNEL_ADDR, 4096),
        ("una dirección libre", 0x5900_0000, 4096),
    ] {
        ok &= check(name, unmap_raw(v, l) == Err(Error::InvalidArgs));
    }
    // El objeto sigue mapeado y entero.
    ok &= check(
        "después de los rechazos sigue mapeado",
        m.read_u64(8) == 0x6e73_6f73,
    );
    mem_unmap(m)?;
    let again = mem_map(h, VADDR, Rights::READ | Rights::WRITE);
    ok &= check(
        "la misma dirección se vuelve a mapear, mismo contenido",
        again.is_ok_and(|m| m.read_u64(8) == 0x6e73_6f73),
    );
    ok &= check("desmapear de nuevo", unmap_raw(VADDR, SIZE) == Ok(0));
    ok &= check(
        "y una tercera vez: ya no hay nada",
        unmap_raw(VADDR, SIZE) == Err(Error::InvalidArgs),
    );
    Ok(ok)
}

fn main() -> i32 {
    match run() {
        Ok(true) => {}
        Ok(false) => return 1,
        Err(e) => {
            println!("desmapeo: error inesperado {:?}", e);
            return 2;
        }
    }
    println!("desmapeo: ahora leo la página que desmapeé...");
    let p = core::hint::black_box(VADDR as *const u64);
    #[allow(unsafe_code)]
    // SAFETY: no lo es, a propósito: la página no está mapeada y la MMU
    // tiene que generar un fallo de página.
    let x = unsafe { p.read_volatile() };
    println!("desmapeo: ¡LEÍ UNA PÁGINA DESMAPEADA! ({})", x);
    3
}
