//! Implementación de las syscalls del ABI v1.
//!
//! Todo argumento viene de una cell y se trata como hostil: handles se
//! buscan en el CSpace de la cell que llama, punteros pasan por
//! `usercopy`, y los mensajes por `Message::from_bytes` (validación total).

use crate::arch::idt::TrapFrame;
use crate::arch::{apic, cpu};
use crate::cell::{self, Cell, ExitStatus, KObject, QueuedMsg, ENDPOINT_RIGHTS};
use crate::usercopy::{copy_from_user, copy_to_user};
use crate::{kprintln, sched};
use alloc::sync::Arc;
use alloc::vec;
use nsos_abi::cap::CapInfo;
use nsos_abi::error::encode_ret;
use nsos_abi::msg::MSG_SIZE;
use nsos_abi::syscall::{flags, Syscall};
use nsos_abi::{CapHandle, Error, Message, Rights, SysResult};
use nsos_kcore::cspace::Cap;

/// Máximo de bytes por `debug_log`.
const MAX_LOG: usize = 1024;

/// Atiende la syscall cuyo estado está en `f` (número en `rax`).
pub fn handle(f: &mut TrapFrame) {
    let cell = sched::current_cell().expect("syscall desde un hilo sin cell");
    let r = match Syscall::from_raw(f.rax) {
        None => Err(Error::NoSys),
        Some(Syscall::Exit) => {
            cell::terminate(&cell, ExitStatus::Exited(f.rdi as u32 as i32));
            drop(cell);
            sched::exit();
        }
        Some(s) => dispatch(s, &cell, [f.rdi, f.rsi, f.rdx, f.r10]),
    };
    f.rax = encode_ret(r) as u64;
}

fn dispatch(s: Syscall, cell: &Arc<Cell>, a: [u64; 4]) -> SysResult<u64> {
    let [a0, a1, a2, a3] = a;
    let h = |v: u64| CapHandle(v as u32);
    match s {
        Syscall::DebugLog => debug_log(cell, a0, a1),
        Syscall::Yield => {
            crate::arch::yield_now();
            Ok(0)
        }
        Syscall::ClockMonotonic => {
            let hz = apic::tsc_hz().max(1);
            Ok(((cpu::rdtsc() as u128 * 1_000_000_000) / hz as u128) as u64)
        }
        Syscall::CapDup => {
            let mask = Rights::from_bits(a1 as u32).ok_or(Error::InvalidArgs)?;
            cell.cspace.lock().dup(h(a0), mask).map(|n| n.0 as u64)
        }
        Syscall::CapDrop => {
            let cap = cell.cspace.lock().remove(h(a0))?;
            drop(cap); // fuera del lock
            cell.revoke_mappings();
            cell::ipc_gc();
            Ok(0)
        }
        Syscall::CapInfo => {
            let info = {
                let cs = cell.cspace.lock();
                let c = cs.get(h(a0))?;
                CapInfo {
                    object_type: c.object.object_type() as u32,
                    rights: c.rights.bits(),
                    koid: c.object.koid(),
                }
            };
            let mut b = [0u8; 16];
            b[0..4].copy_from_slice(&info.object_type.to_le_bytes());
            b[4..8].copy_from_slice(&info.rights.to_le_bytes());
            b[8..16].copy_from_slice(&info.koid.to_le_bytes());
            copy_to_user(cell, a1, &b)?;
            Ok(0)
        }
        Syscall::ChannelCreate => channel_create(cell, a0),
        Syscall::ChannelSend => channel_send(cell, h(a0), a1, a2),
        Syscall::ChannelRecv => channel_recv(cell, h(a0), a1, a2),
        Syscall::MemCreate => mem_create(cell, a0, a1),
        Syscall::MemMap => mem_map(cell, h(a0), a1, a2),
        Syscall::MemInfo => mem_info(cell, h(a0), a1),
        Syscall::CellSpawn => cell_spawn(cell, h(a0), a1, a2),
        Syscall::CellKill => cell_kill(cell, h(a0)),
        Syscall::CellStatus => cell_status(cell, h(a0), a1, a2),
        Syscall::IoPortRead => ioport(cell, h(a0), a1, a2, None),
        Syscall::IoPortWrite => ioport(cell, h(a0), a1, a2, Some(a3)),
        _ => Err(Error::Unsupported),
    }
}

fn debug_log(cell: &Cell, ptr: u64, len: u64) -> SysResult<u64> {
    let len = usize::try_from(len).map_err(|_| Error::InvalidArgs)?;
    if len > MAX_LOG {
        return Err(Error::InvalidArgs);
    }
    let mut buf = vec![0u8; len];
    copy_from_user(cell, ptr, &mut buf)?;
    let text = core::str::from_utf8(&buf).map_err(|_| Error::InvalidArgs)?;
    crate::kprint!("[{}] {}", cell.name, text);
    Ok(0)
}

fn channel_create(cell: &Cell, out: u64) -> SysResult<u64> {
    copy_to_user(cell, out, &[0u8; 8])?; // validar antes de crear nada
    let (a, b) = cell::channel_create();
    let caps = vec![
        Cap {
            object: KObject::Channel(a),
            rights: ENDPOINT_RIGHTS,
        },
        Cap {
            object: KObject::Channel(b),
            rights: ENDPOINT_RIGHTS,
        },
    ];
    let hs = cell.cspace.lock().insert_all(caps).map_err(|(e, caps)| {
        drop(caps);
        e
    })?;
    let mut bytes = [0u8; 8];
    bytes[0..4].copy_from_slice(&hs[0].0.to_le_bytes());
    bytes[4..8].copy_from_slice(&hs[1].0.to_le_bytes());
    copy_to_user(cell, out, &bytes)?;
    Ok(0)
}

/// Cede la CPU tras marcarse bloqueado. Si mientras tanto se pidió matar a
/// la cell, abandona la espera: la syscall vuelve y la ruta de salida a
/// anillo 3 termina la cell (`trap::dispatch`).
fn sleep(cell: &Cell) -> SysResult<()> {
    crate::arch::yield_now();
    if cell.kill_requested() {
        return Err(Error::PeerClosed);
    }
    Ok(())
}

/// Máximo de capacidades que se pueden entregar al crear una cell.
const SPAWN_MAX_CAPS: usize = 16;

/// `cell_spawn(h_image, caps, n) -> h_cell`.
fn cell_spawn(cell: &Cell, img: CapHandle, uptr: u64, n: u64) -> SysResult<u64> {
    let n = usize::try_from(n).map_err(|_| Error::InvalidArgs)?;
    if n > SPAWN_MAX_CAPS {
        return Err(Error::InvalidArgs);
    }
    let mut raw = [0u8; 4 * SPAWN_MAX_CAPS];
    copy_from_user(cell, uptr, &mut raw[..4 * n])?;
    let hs: alloc::vec::Vec<CapHandle> = raw[..4 * n]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| CapHandle(u32::from_le_bytes(*b)))
        .collect();
    let image = match &cell.cspace.lock().get_with(img, Rights::EXEC)?.object {
        KObject::Image(i) => i.clone(),
        _ => return Err(Error::WrongType),
    };
    // Validar la imagen antes de quitarle nada a quien llama.
    nsos_kcore::elf::parse(image.bytes).map_err(|_| Error::InvalidArgs)?;
    // Reservar el slot de la respuesta *antes* de mover las capacidades,
    // para no fallar después de crear la cell.
    let caps = {
        let mut cs = cell.cspace.lock();
        if cs.len() + 1 > crate::cell::CSPACE_LIMIT as usize {
            return Err(Error::NoMemory);
        }
        cs.remove_all(&hs, Rights::GRANT)?
    };
    cell.revoke_mappings();
    let child = cell::spawn(&image.name, image.bytes, caps).map_err(|e| {
        kprintln!("cell_spawn \"{}\": {:?}", image.name, e);
        Error::NoMemory
    })?;
    let r = cell.cspace.lock().insert(Cap {
        object: KObject::Cell(child),
        rights: cell::CELL_RIGHTS,
    });
    // Si aun así no entra, la cell nueva sigue corriendo sin supervisor.
    r.map(|h| h.0 as u64)
}

fn target_cell(cell: &Cell, h: CapHandle, need: Rights) -> SysResult<Arc<Cell>> {
    match &cell.cspace.lock().get_with(h, need)?.object {
        KObject::Cell(c) => Ok(c.clone()),
        _ => Err(Error::WrongType),
    }
}

/// `cell_kill(h_cell)`: pide terminarla; no espera.
fn cell_kill(cell: &Cell, h: CapHandle) -> SysResult<u64> {
    let target = target_cell(cell, h, Rights::CONTROL)?;
    target.request_kill();
    Ok(0)
}

/// `cell_status(h_cell, out, flags)`.
fn cell_status(cell: &Cell, h: CapHandle, out: u64, fl: u64) -> SysResult<u64> {
    use nsos_abi::syscall::{cell_state, CellStatus};
    if fl & !flags::NONBLOCK != 0 {
        return Err(Error::InvalidArgs);
    }
    copy_to_user(cell, out, &[0u8; CellStatus::SIZE])?;
    let wait = fl & flags::NONBLOCK == 0;
    let target = target_cell(cell, h, if wait { Rights::WAIT } else { Rights::NONE })?;
    let st = loop {
        match target.exit_status(wait) {
            Some(st) => break Some(st),
            None if wait => sleep(cell)?,
            None => break None,
        }
    };
    let s = match st {
        None => CellStatus::default(),
        Some(ExitStatus::Exited(code)) => CellStatus {
            state: cell_state::EXITED,
            code,
            ..CellStatus::default()
        },
        Some(ExitStatus::Faulted { vector, addr, .. }) => CellStatus {
            state: cell_state::FAULTED,
            vector,
            addr,
            ..CellStatus::default()
        },
        Some(ExitStatus::Killed) => CellStatus {
            state: cell_state::KILLED,
            ..CellStatus::default()
        },
    };
    copy_to_user(cell, out, &s.to_bytes())?;
    Ok(0)
}

fn endpoint(cell: &Cell, h: CapHandle, need: Rights) -> SysResult<Arc<cell::Endpoint>> {
    match &cell.cspace.lock().get_with(h, need)?.object {
        KObject::Channel(e) => Ok(e.clone()),
        _ => Err(Error::WrongType),
    }
}

/// `mem_map(h, vaddr, rights)`: los derechos pedidos deben estar en la
/// capacidad; siempre hace falta READ; nunca WRITE y EXEC juntos.
/// Tope de `mem_create` (los objetos son contiguos).
const MEM_CREATE_MAX: u64 = 1 << 20;

/// `mem_create(size, flags) -> h`: memoria nueva, a cero, para compartir
/// (por ejemplo el estado de una actualización en vivo). Sin `CONTROL`: quien
/// la crea no conoce su dirección física.
fn mem_create(cell: &Cell, size: u64, fl: u64) -> SysResult<u64> {
    if fl != 0 || size == 0 || size > MEM_CREATE_MAX {
        return Err(Error::InvalidArgs);
    }
    let pages = size.div_ceil(crate::arch::paging::PAGE_SIZE) as usize;
    let obj = cell::MemObj::new(pages).ok_or(Error::NoMemory)?;
    let h = cell.cspace.lock().insert(Cap {
        object: KObject::Memory(obj),
        rights: cell::MEM_RIGHTS,
    })?;
    Ok(h.0 as u64)
}

fn mem_map(cell: &Cell, h: CapHandle, vaddr: u64, want: u64) -> SysResult<u64> {
    let want = Rights::from_bits(want as u32).ok_or(Error::InvalidArgs)?;
    if !want.contains(Rights::READ)
        || want.bits() & !(Rights::READ | Rights::WRITE | Rights::EXEC).bits() != 0
    {
        return Err(Error::InvalidArgs);
    }
    let obj = match cell.cspace.lock().get_with(h, want)? {
        Cap {
            object: KObject::Memory(m),
            ..
        } => m.clone(),
        _ => return Err(Error::WrongType),
    };
    cell.map_memory(
        obj,
        vaddr,
        want.contains(Rights::WRITE),
        want.contains(Rights::EXEC),
    )?;
    Ok(vaddr)
}

fn mem_info(cell: &Cell, h: CapHandle, out: u64) -> SysResult<u64> {
    let (size, phys) = match cell.cspace.lock().get(h)? {
        Cap {
            object: KObject::Memory(m),
            rights,
        } => {
            let phys = if rights.contains(Rights::CONTROL) {
                m.phys
            } else {
                0
            };
            (m.size(), phys)
        }
        _ => return Err(Error::WrongType),
    };
    let mut b = [0u8; 16];
    b[0..8].copy_from_slice(&size.to_le_bytes());
    b[8..16].copy_from_slice(&phys.to_le_bytes());
    copy_to_user(cell, out, &b)?;
    Ok(0)
}

/// `ioport_read(h, off, width)` / `ioport_write(h, off, width, valor)`.
/// `off` es relativo al inicio del rango; el acceso entero debe caer dentro.
fn ioport(cell: &Cell, h: CapHandle, off: u64, width: u64, value: Option<u64>) -> SysResult<u64> {
    use crate::arch::port::Port;
    let need = if value.is_some() {
        Rights::WRITE
    } else {
        Rights::READ
    };
    let (base, len) = match cell.cspace.lock().get_with(h, need)? {
        Cap {
            object: KObject::IoPorts(r),
            ..
        } => (r.base as u64, r.len as u64),
        _ => return Err(Error::WrongType),
    };
    if !matches!(width, 1 | 2 | 4) || off.checked_add(width).is_none_or(|e| e > len) {
        return Err(Error::InvalidArgs);
    }
    let port = (base + off) as u16;
    // SAFETY: el kernel entregó este rango de puertos a la cell al crearla
    // (un dispositivo PCI descubierto por el kernel o COM2; el kernel no usa
    // ninguno de los dos), y el acceso cae completamente dentro del rango.
    unsafe {
        Ok(match (width, value) {
            (1, None) => Port::<u8>::new(port).read() as u64,
            (2, None) => Port::<u16>::new(port).read() as u64,
            (4, None) => Port::<u32>::new(port).read() as u64,
            (1, Some(v)) => {
                Port::<u8>::new(port).write(v as u8);
                0
            }
            (2, Some(v)) => {
                Port::<u16>::new(port).write(v as u16);
                0
            }
            (_, Some(v)) => {
                Port::<u32>::new(port).write(v as u32);
                0
            }
            _ => unreachable!(),
        })
    }
}

fn channel_send(cell: &Cell, h: CapHandle, uptr: u64, fl: u64) -> SysResult<u64> {
    if fl & !flags::NONBLOCK != 0 {
        return Err(Error::InvalidArgs);
    }
    let mut raw = [0u8; MSG_SIZE];
    copy_from_user(cell, uptr, &mut raw)?;
    let msg = Message::from_bytes(&raw)?;
    let wait = fl & flags::NONBLOCK == 0;
    loop {
        let r = {
            let mut cs = cell.cspace.lock();
            let ep = match &cs.get_with(h, Rights::SEND)?.object {
                KObject::Channel(e) => e.clone(),
                _ => return Err(Error::WrongType),
            };
            ep.send_with(wait, || {
                let caps = cs.remove_all(msg.cap_slice(), Rights::GRANT)?;
                Ok(QueuedMsg::new(msg.header, msg.data, caps))
            })
        };
        match r {
            Err(Error::WouldBlock) if wait => sleep(cell)?,
            Ok(()) if !msg.cap_slice().is_empty() => {
                // Las capacidades de memoria enviadas ya no se ven.
                cell.revoke_mappings();
                // Mover extremos puede dejar un ciclo inalcanzable.
                cell::ipc_gc();
                return Ok(0);
            }
            r => return r.map(|_| 0),
        }
    }
}

fn channel_recv(cell: &Cell, h: CapHandle, uptr: u64, fl: u64) -> SysResult<u64> {
    if fl & !flags::NONBLOCK != 0 {
        return Err(Error::InvalidArgs);
    }
    copy_to_user(cell, uptr, &[0u8; MSG_SIZE])?; // el búfer debe ser escribible
    let wait = fl & flags::NONBLOCK == 0;
    let ep = endpoint(cell, h, Rights::RECV)?;
    let (q, handles) = loop {
        match ep.try_recv(wait) {
            Ok(mut q) => {
                let caps = q.take_caps();
                match cell.cspace.lock().insert_all(caps) {
                    Ok(hs) => break (q, hs),
                    Err((e, caps)) => {
                        q.put_caps(caps);
                        ep.unrecv(q);
                        return Err(e);
                    }
                }
            }
            Err(Error::WouldBlock) if wait => sleep(cell)?,
            Err(e) => return Err(e),
        }
    };
    let mut m = Message {
        header: q.header,
        data: q.data,
        ..Message::default()
    };
    m.header.cap_count = handles.len() as u8;
    m.caps[..handles.len()].copy_from_slice(&handles);
    copy_to_user(cell, uptr, &m.to_bytes())?;
    Ok(0)
}

/// Una cell causó una excepción de CPU: se la termina y el kernel sigue.
pub fn user_fault(f: &TrapFrame, raw: *mut TrapFrame) -> *mut TrapFrame {
    let addr = if f.vector == 14 { cpu::read_cr2() } else { 0 };
    if let Some(cell) = sched::current_cell() {
        kprintln!(
            "cell \"{}\": excepción {} en rip={:#x} (dir {:#x}); el kernel la termina",
            cell.name,
            f.vector,
            f.rip,
            addr
        );
        cell::terminate(
            &cell,
            ExitStatus::Faulted {
                vector: f.vector,
                rip: f.rip,
                addr,
            },
        );
    }
    sched::exit_from_trap(raw)
}
