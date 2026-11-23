//! Cell `intruso`: test de aislamiento.
//!
//! Arranca sin ninguna capacidad e intenta abusar del kernel: handles que no
//! tiene, punteros al kernel o a su propio código, tipos de objeto
//! equivocados, derechos que no tiene, y cada syscall de memoria, puertos,
//! cells y espera con argumentos inválidos. Cada intento debe fallar con el error
//! esperado. Para probar derechos se fabrica sus propios objetos (canales y
//! memoria) y los atenúa. Al final lee memoria del kernel, lo que debe
//! producir un fallo de página: el kernel termina esta cell y sigue
//! funcionando.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::{flags::NONBLOCK, Syscall};
use nsos_rt::abi::{error::decode_ret, CapHandle, Error, Message, Rights};
use nsos_rt::{
    cap_dup, cap_info, channel_create, channel_send, mem_create, mem_info, println, raw_syscall,
};

nsos_rt::entry!(main);

const KERNEL_ADDR: u64 = 0xffff_ffff_8000_0000;
/// Inicio de mi código (ver `cells/cell.ld`).
const MY_CODE: u64 = 0x40_0000;
/// Zona libre de mi espacio para mapear.
const FREE: u64 = 0x5000_0000;

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

fn s4(n: Syscall, a: u64, b: u64, c: u64, d: u64) -> Result<u64, Error> {
    decode_ret(raw_syscall(n as u64, a, b, c, d))
}

/// Abusos con objetos propios: memoria, canales, puertos y cells.
fn abuse_objects() -> bool {
    use Syscall::*;
    let (Ok(mem), Ok((a, b))) = (mem_create(4096), channel_create()) else {
        println!("intruso: no pude crear mis objetos");
        return false;
    };
    let (m, ch) = (mem.0 as u64, a.0 as u64);
    let rw = (Rights::READ | Rights::WRITE).bits() as u64;
    let rwx = (Rights::READ | Rights::WRITE | Rights::EXEC).bits() as u64;
    let mut ok = true;
    // Memoria.
    ok &= expect(
        "mem_create de 0 bytes",
        s4(MemCreate, 0, 0, 0, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "mem_create de 2 GiB",
        s4(MemCreate, 1 << 31, 0, 0, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "mem_create con flags desconocidos",
        s4(MemCreate, 4096, 1, 0, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "mem_map sobre el kernel",
        s4(MemMap, m, KERNEL_ADDR, rw, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "mem_map encima de mi código",
        s4(MemMap, m, MY_CODE, rw, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "mem_map W+X (y sin derecho EXEC)",
        s4(MemMap, m, FREE, rwx, 0),
        Error::AccessDenied,
    );
    ok &= expect(
        "mem_map desalineado",
        s4(MemMap, m, FREE + 8, rw, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "mem_map de un canal",
        s4(MemMap, ch, FREE, rw, 0),
        Error::AccessDenied,
    );
    ok &= expect(
        "mem_info de un canal",
        s4(MemInfo, ch, FREE, 0, 0),
        Error::WrongType,
    );
    match mem_info(mem) {
        Ok(i) if i.phys == 0 => println!(
            "intruso: {:<42} -> phys = 0 (bloqueado)",
            "ver la dirección física sin CONTROL"
        ),
        other => {
            println!("intruso: ¡INESPERADO! mem_info sin CONTROL: {:?}", other);
            ok = false;
        }
    }
    match cap_dup(mem, Rights::READ | Rights::DUPLICATE) {
        Ok(ro) => {
            ok &= expect(
                "mapear escribible una memoria de solo lectura",
                s4(MemMap, ro.0 as u64, FREE, rw, 0),
                Error::AccessDenied,
            );
        }
        Err(e) => {
            println!("intruso: ¡INESPERADO! cap_dup falló: {:?}", e);
            ok = false;
        }
    }
    // Contratos de canal (`channel_contract`): solo describe canales, y no
    // se puede declarar un contrato que no cabría en el resultado.
    ok &= expect(
        "channel_contract de una memoria",
        s4(ChannelContract, m, 0, 0, 0),
        Error::WrongType,
    );
    ok &= expect(
        "channel_contract de un handle inventado",
        s4(ChannelContract, 0x7_0000, 0, 0, 0),
        Error::BadHandle,
    );
    let mut out = [0u32; 2];
    ok &= expect(
        "channel_create con un contrato de 64 bits",
        s4(ChannelCreate, out.as_mut_ptr() as u64, 1 << 63, 0, 0),
        Error::InvalidArgs,
    );
    // Puertos de E/S.
    ok &= expect(
        "ioport_read con un objeto de memoria",
        s4(IoPortRead, m, 0, 1, 0),
        Error::WrongType,
    );
    ok &= expect(
        "ioport_write sin capacidad",
        s4(IoPortWrite, 0x7_0000, 0x60, 1, 0),
        Error::BadHandle,
    );
    // Cells.
    let mut buf = [0u8; 24];
    let out = buf.as_mut_ptr() as u64;
    ok &= expect(
        "cell_spawn con un canal como imagen",
        s4(CellSpawn, ch, 0, 0, 0),
        Error::AccessDenied,
    );
    ok &= expect(
        "cell_spawn con 17 capacidades",
        s4(CellSpawn, ch, FREE, 17, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "cell_kill de un canal",
        s4(CellKill, ch, 0, 0, 0),
        Error::AccessDenied,
    );
    ok &= expect(
        "cell_status de una memoria",
        s4(CellStatus, m, out, NONBLOCK, 0),
        Error::WrongType,
    );
    ok &= expect(
        "cell_status escribiendo en el kernel",
        s4(CellStatus, m, KERNEL_ADDR, NONBLOCK, 0),
        Error::InvalidArgs,
    );
    // Espera.
    ok &= abuse_wait(m, a, b);
    // Mover una capacidad sin GRANT.
    match cap_dup(b, Rights::SEND | Rights::RECV) {
        Ok(no_grant) => {
            let mut msg = Message::new(0, b"").unwrap();
            msg.push_cap(no_grant).unwrap();
            ok &= expect(
                "enviar una capacidad sin GRANT",
                channel_send(a, &msg, 0).map(|_| 0),
                Error::AccessDenied,
            );
        }
        Err(e) => {
            println!("intruso: ¡INESPERADO! cap_dup falló: {:?}", e);
            ok = false;
        }
    }
    ok
}

/// Abusos de `wait`: con `m` (memoria) y `a`/`b` (extremos propios, vacíos).
fn abuse_wait(m: u64, a: CapHandle, b: CapHandle) -> bool {
    use nsos_rt::abi::cap::signals::{CELL_EXITED, READABLE};
    use nsos_rt::abi::syscall::WaitItem;
    use Syscall::Wait;
    let item = |h: u64, wanted: u32, reserved: u32| WaitItem {
        handle: h as u32,
        wanted,
        observed: 0,
        reserved,
    };
    let mut items = [item(a.0 as u64, READABLE, 0); 17];
    // El kernel lee los ítems desde este arreglo (y escribe `observed`).
    let w = |items: &mut [WaitItem; 17], n: u64| s4(Wait, items.as_mut_ptr() as u64, n, 0, 0);
    let mut ok = true;
    ok &= expect("wait con 0 ítems", w(&mut items, 0), Error::InvalidArgs);
    ok &= expect("wait con 17 ítems", w(&mut items, 17), Error::InvalidArgs);
    ok &= expect(
        "wait con los ítems en el kernel",
        s4(Wait, KERNEL_ADDR, 1, 0, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "wait con los ítems en mi código",
        s4(Wait, MY_CODE, 1, 0, 0),
        Error::InvalidArgs,
    );
    items[0] = item(0x7_0000, READABLE, 0);
    ok &= expect(
        "wait sobre un handle que no tengo",
        w(&mut items, 1),
        Error::BadHandle,
    );
    items[0] = item(m, READABLE, 0);
    ok &= expect(
        "wait sobre una memoria (sin WAIT)",
        w(&mut items, 1),
        Error::AccessDenied,
    );
    match cap_dup(b, Rights::SEND | Rights::RECV) {
        Ok(no_wait) => {
            items[0] = item(no_wait.0 as u64, READABLE, 0);
            ok &= expect(
                "wait sobre un canal sin derecho WAIT",
                w(&mut items, 1),
                Error::AccessDenied,
            );
        }
        Err(e) => {
            println!("intruso: ¡INESPERADO! cap_dup falló: {:?}", e);
            ok = false;
        }
    }
    items[0] = item(a.0 as u64, CELL_EXITED, 0);
    ok &= expect(
        "wait de CELL_EXITED sobre un canal",
        w(&mut items, 1),
        Error::InvalidArgs,
    );
    items[0] = item(a.0 as u64, 0, 0);
    ok &= expect(
        "wait sin señales pedidas",
        w(&mut items, 1),
        Error::InvalidArgs,
    );
    items[0] = item(a.0 as u64, READABLE, 1);
    ok &= expect(
        "wait con reserved distinto de 0",
        w(&mut items, 1),
        Error::InvalidArgs,
    );
    items[0] = item(a.0 as u64, nsos_rt::abi::cap::signals::IRQ_PENDING, 0);
    ok &= expect(
        "wait de IRQ_PENDING sobre un canal",
        w(&mut items, 1),
        Error::InvalidArgs,
    );
    ok &= expect(
        "irq_ack sin capacidad",
        s4(Syscall::IrqAck, 0x7_0000, 0, 0, 0),
        Error::BadHandle,
    );
    ok &= expect(
        "irq_ack de un canal (sin CONTROL)",
        s4(Syscall::IrqAck, a.0 as u64, 0, 0, 0),
        Error::AccessDenied,
    );
    ok &= expect(
        "irq_ack de una memoria",
        s4(Syscall::IrqAck, m, 0, 0, 0),
        Error::AccessDenied,
    );
    // Un mensaje válido (todo a cero) en la pila: escribible, como pide
    // `channel_call` para la respuesta.
    let mut msg_buf = [0u8; nsos_rt::abi::msg::MSG_SIZE];
    let pm = core::hint::black_box(msg_buf.as_mut_ptr()) as u64;
    ok &= expect(
        "channel_call con el mensaje en el kernel",
        s4(Syscall::ChannelCall, a.0 as u64, KERNEL_ADDR, 0, 0),
        Error::InvalidArgs,
    );
    ok &= expect(
        "channel_call sin capacidad",
        s4(Syscall::ChannelCall, 0x7_0000, pm, 0, 0),
        Error::BadHandle,
    );
    ok &= expect(
        "channel_call sobre una memoria",
        s4(Syscall::ChannelCall, m, pm, 0, 0),
        Error::AccessDenied,
    );
    // Un ítem malo invalida la llamada entera, aunque otro esté bien.
    items[0] = item(a.0 as u64, READABLE, 0);
    items[1] = item(m, READABLE, 0);
    ok &= expect(
        "wait con un ítem bueno y uno malo",
        w(&mut items, 2),
        Error::AccessDenied,
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

    ok &= abuse_objects();

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
