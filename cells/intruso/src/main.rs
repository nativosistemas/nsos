//! Cell `intruso`: test de aislamiento.
//!
//! Arranca sin ninguna capacidad e intenta abusar del kernel. Cada intento
//! debe fallar con el error esperado. Al final lee memoria del kernel, lo que
//! debe producir un fallo de página: el kernel termina esta cell y sigue
//! funcionando.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::Syscall;
use nsos_rt::abi::{error::decode_ret, CapHandle, Error, Message};
use nsos_rt::{cap_dup, cap_info, channel_send, println, raw_syscall};

nsos_rt::entry!(main);

const KERNEL_ADDR: u64 = 0xffff_ffff_8000_0000;

fn expect(name: &str, got: Result<u64, Error>, want: Error) -> bool {
    let ok = got == Err(want);
    println!(
        "intruso: {:<42} -> {:?} {}",
        name,
        got,
        if ok { "(bloqueado)" } else { "¡INESPERADO!" }
    );
    ok
}

fn main() -> i32 {
    let mut ok = true;
    let msg = Message::default();
    let s = |n: Syscall, a: u64, b: u64, c: u64| decode_ret(raw_syscall(n as u64, a, b, c, 0));

    ok &= expect(
        "usar un handle que nunca recibí",
        channel_send(CapHandle::new(1, 0).unwrap(), &msg, 0).map(|_| 0),
        Error::BadHandle,
    );
    ok &= expect(
        "handle con generación falsificada",
        cap_info(CapHandle::new(1, 7).unwrap()).map(|_| 0),
        Error::BadHandle,
    );
    ok &= expect(
        "duplicar algo que no tengo",
        cap_dup(CapHandle(u32::MAX), nsos_rt::abi::Rights::ALL).map(|h| h.0 as u64),
        Error::BadHandle,
    );
    ok &= expect(
        "debug_log con puntero al kernel",
        s(Syscall::DebugLog, KERNEL_ADDR, 16, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "debug_log con longitud enorme",
        s(Syscall::DebugLog, 0x40_0000, u64::MAX, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "channel_create escribiendo en el kernel",
        s(Syscall::ChannelCreate, KERNEL_ADDR, 0, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "channel_create escribiendo en mi código",
        s(Syscall::ChannelCreate, 0x40_0000, 0, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "syscall inexistente",
        decode_ret(raw_syscall(9999, 0, 0, 0, 0)),
        Error::NoSys,
    );

    if !ok {
        return 1;
    }
    println!("intruso: ahora leo memoria del kernel directamente...");
    // Leer el kernel desde anillo 3: las páginas no tienen el bit USER, así
    // que la MMU genera #PF y el kernel debe matar a esta cell.
    let p = KERNEL_ADDR as *const u8;
    let v = core::hint::black_box(p);
    #[allow(unsafe_code)]
    // SAFETY: no lo es, a propósito: es el test de aislamiento por hardware.
    let x = unsafe { v.read_volatile() };
    println!("intruso: ¡LEÍ MEMORIA DEL KERNEL! ({})", x);
    2
}
