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
use alloc::vec::Vec;
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
        Some(s) => dispatch(s, &cell, f.rdi, f.rsi, f.rdx),
    };
    f.rax = encode_ret(r) as u64;
}

fn dispatch(s: Syscall, cell: &Arc<Cell>, a0: u64, a1: u64, a2: u64) -> SysResult<u64> {
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

fn endpoint(cell: &Cell, h: CapHandle, need: Rights) -> SysResult<Arc<cell::Endpoint>> {
    match &cell.cspace.lock().get_with(h, need)?.object {
        KObject::Channel(e) => Ok(e.clone()),
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
            };
            ep.send_with(wait, || {
                let caps = cs.remove_all(msg.cap_slice(), Rights::GRANT)?;
                Ok(QueuedMsg {
                    header: msg.header,
                    data: msg.data,
                    caps,
                })
            })
        };
        match r {
            Err(Error::WouldBlock) if wait => crate::arch::yield_now(),
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
                let caps: Vec<_> = core::mem::take(&mut q.caps);
                match cell.cspace.lock().insert_all(caps) {
                    Ok(hs) => break (q, hs),
                    Err((e, caps)) => {
                        q.caps = caps;
                        ep.unrecv(q);
                        return Err(e);
                    }
                }
            }
            Err(Error::WouldBlock) if wait => crate::arch::yield_now(),
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
