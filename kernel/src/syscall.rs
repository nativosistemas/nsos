//! Implementación de las syscalls del ABI v1.
//!
//! Todo argumento viene de una cell y se trata como hostil: handles se
//! buscan en el CSpace de la cell que llama, punteros pasan por
//! `usercopy`, y los mensajes por `Message::from_bytes` (validación total).

use crate::arch::TrapFrame;
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

/// Atiende la syscall cuyo estado está en `f` (convención en
/// `nsos_abi::syscall`).
pub fn handle(f: &mut TrapFrame) {
    let cell = sched::current_cell().expect("syscall desde un hilo sin cell");
    let args = f.syscall_args();
    let nr = Syscall::from_raw(f.syscall_nr());
    // Spectre v1: que una predicción equivocada del control del número no
    // lleve a ejecutar especulativamente otra syscall con estos argumentos.
    crate::arch::cpu::speculation_barrier();
    let r = match nr {
        None => Err(Error::NoSys),
        Some(Syscall::Exit) => {
            cell::terminate(&cell, ExitStatus::Exited(args[0] as u32 as i32));
            drop(cell);
            sched::exit();
        }
        Some(s) => dispatch(s, &cell, args),
    };
    f.set_syscall_ret(encode_ret(r) as u64);
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
        Syscall::ClockMonotonic => Ok(crate::arch::time::now_ns()),
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
        Syscall::ChannelCreate => channel_create(cell, a0, a1),
        Syscall::ChannelContract => {
            let cs = cell.cspace.lock();
            match &cs.get(h(a0))?.object {
                KObject::Channel(e) => Ok(e.contract()),
                _ => Err(Error::WrongType),
            }
        }
        Syscall::ChannelSend => channel_send(cell, h(a0), a1, a2),
        Syscall::ChannelRecv => channel_recv(cell, h(a0), a1, a2),
        Syscall::ChannelCall => channel_call(cell, h(a0), a1, a2),
        Syscall::Wait => wait(cell, a0, a1, a2),
        Syscall::MemCreate => mem_create(cell, a0, a1),
        Syscall::MemMap => mem_map(cell, h(a0), a1, a2),
        Syscall::MemUnmap => cell.unmap_memory(a0, a1).map(|_| 0),
        Syscall::MemInfo => mem_info(cell, h(a0), a1),
        Syscall::CellSpawn => cell_spawn(cell, h(a0), a1, a2, a3),
        Syscall::CellKill => cell_kill(cell, h(a0)),
        Syscall::CellStatus => cell_status(cell, h(a0), a1, a2),
        Syscall::IrqAck => irq_ack(cell, h(a0)),
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

fn channel_create(cell: &Cell, out: u64, contract: u64) -> SysResult<u64> {
    // `channel_contract` lo devuelve como resultado de syscall, que es
    // negativo si es un error: solo caben contratos menores que 2^63.
    if contract > i64::MAX as u64 {
        return Err(Error::InvalidArgs);
    }
    copy_to_user(cell, out, &[0u8; 8])?; // validar antes de crear nada
    let (a, b) = cell::channel_create(contract);
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
/// La cuota de la hija (`q` marcos) sale de la cuota disponible de quien
/// llama (`quota::Account::split`); al morir la hija, vuelve.
fn cell_spawn(cell: &Cell, img: CapHandle, uptr: u64, n: u64, q: u64) -> SysResult<u64> {
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
    // Reservar el lugar de la respuesta *antes* de mover las capacidades:
    // después de crear la cell, insertarla no puede fallar (si fallara, la
    // cell quedaría corriendo sin que nadie pueda matarla).
    let account = cell.account.split(q)?;
    let (reservation, caps) = {
        let mut cs = cell.cspace.lock();
        let reserved = cs
            .reserve()
            .and_then(|r| match cs.remove_all(&hs, Rights::GRANT) {
                Ok(caps) => Ok((r, caps)),
                Err(e) => {
                    cs.cancel(r);
                    Err(e)
                }
            });
        match reserved {
            Ok(x) => x,
            Err(e) => {
                drop(cs);
                account.close(); // la cuota vuelve entera
                return Err(e);
            }
        }
    };
    cell.revoke_mappings();
    // `spawn_in` se queda con la cuenta: si falla, la devuelve.
    let child = match cell::spawn_in(&image.name, image.bytes, caps, account) {
        Ok(c) => c,
        Err(e) => {
            kprintln!("cell_spawn \"{}\": {:?}", image.name, e);
            cell.cspace.lock().cancel(reservation);
            return Err(Error::NoMemory);
        }
    };
    let h = cell.cspace.lock().insert_reserved(
        reservation,
        Cap {
            object: KObject::Cell(child),
            rights: cell::CELL_RIGHTS,
        },
    );
    Ok(h.0 as u64)
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
    let obj = cell::MemObj::new(pages, cell.account.clone()).ok_or(Error::NoMemory)?;
    let h = cell.cspace.lock().insert(Cap {
        object: KObject::Memory(obj),
        rights: cell::MEM_RIGHTS,
    })?;
    Ok(h.0 as u64)
}

/// `mem_map(h, vaddr, rights)`: los derechos pedidos deben estar en la
/// capacidad; siempre hace falta READ; nunca EXEC (el mapeo es siempre NX).
fn mem_map(cell: &Cell, h: CapHandle, vaddr: u64, want: u64) -> SysResult<u64> {
    // Solo READ o READ | WRITE: `EXEC` se rechaza (cells selladas; Kani:
    // `map_request_never_exec`, modelo: `Mem.step_sealed`).
    let req = nsos_kcore::paging::map_request(want).ok_or(Error::InvalidArgs)?;
    let obj = match cell.cspace.lock().get_with(h, req.need)? {
        Cap {
            object: KObject::Memory(m),
            ..
        } => m.clone(),
        _ => return Err(Error::WrongType),
    };
    cell.map_memory(obj, vaddr, req.write)?;
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

/// `irq_ack(h)`: con `CONTROL` sobre una línea, borra `IRQ_PENDING` y la
/// desenmascara (`IrqModel.ack_needs_owner`).
fn irq_ack(cell: &Cell, h: CapHandle) -> SysResult<u64> {
    let line = match &cell.cspace.lock().get_with(h, Rights::CONTROL)?.object {
        KObject::Irq(i) => i.clone(),
        _ => return Err(Error::WrongType),
    };
    line.ack();
    Ok(0)
}

/// `ioport_read(h, off, width)` / `ioport_write(h, off, width, valor)`.
/// `off` es relativo al inicio del rango; el acceso entero debe caer dentro.
fn ioport(cell: &Cell, h: CapHandle, off: u64, width: u64, value: Option<u64>) -> SysResult<u64> {
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
    port_access(base, len, off, width, value)
}

/// Lee o escribe un puerto de E/S ya validado.
#[cfg(target_arch = "x86_64")]
fn port_access(base: u64, _len: u64, off: u64, width: u64, value: Option<u64>) -> SysResult<u64> {
    use crate::arch::port::Port;
    let port = (base + off) as u16;
    // SAFETY: el kernel entregó este rango de puertos a la cell al crearla
    // (un dispositivo PCI descubierto por el kernel, que no lo usa), y el
    // acceso cae completamente dentro del rango.
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

/// En ARM64 los puertos de E/S de PCI están en una ventana de memoria
/// (ver `pci::pio_access`).
#[cfg(target_arch = "aarch64")]
fn port_access(base: u64, len: u64, off: u64, width: u64, value: Option<u64>) -> SysResult<u64> {
    crate::pci::pio_access(base, len, off, width, value).ok_or(Error::InvalidArgs)
}

/// Espera de una operación de canal: qué hacer con `WouldBlock`.
/// `Poll`: devolverlo; `Never`: dormir; `At(d)`: dormir hasta `d` y después
/// `TimedOut`. El hilo ya quedó registrado y marcado bloqueado por
/// `send_with`/`try_recv`.
fn block_until(cell: &Cell, deadline: nsos_kcore::wait::Deadline) -> SysResult<()> {
    use nsos_kcore::wait::{expired, Deadline};
    if expired(deadline, crate::arch::time::now_ns()) {
        sched::wake(sched::current()); // no dormir: se venció el plazo
        return Err(Error::TimedOut);
    }
    if let Deadline::At(d) = deadline {
        sched::wake_at(d);
    }
    sleep(cell)
}

/// Plazo de `channel_send`/`channel_recv` según sus flags.
fn flags_deadline(fl: u64) -> SysResult<nsos_kcore::wait::Deadline> {
    use nsos_kcore::wait::Deadline;
    if fl & !flags::NONBLOCK != 0 {
        return Err(Error::InvalidArgs);
    }
    Ok(if fl & flags::NONBLOCK != 0 {
        Deadline::Poll
    } else {
        Deadline::Never
    })
}

fn channel_send(cell: &Cell, h: CapHandle, uptr: u64, fl: u64) -> SysResult<u64> {
    let deadline = flags_deadline(fl)?;
    let mut raw = [0u8; MSG_SIZE];
    copy_from_user(cell, uptr, &mut raw)?;
    let msg = Message::from_bytes(&raw)?;
    send_msg(cell, h, &msg, deadline).map(|_| 0)
}

/// Envía `msg` por `h` (requiere `SEND`; cada capacidad adjunta, `GRANT`),
/// esperando lugar en la cola hasta `deadline`. Atómico: si falla, el emisor
/// no perdió nada.
fn send_msg(
    cell: &Cell,
    h: CapHandle,
    msg: &Message,
    deadline: nsos_kcore::wait::Deadline,
) -> SysResult<()> {
    let wait = deadline != nsos_kcore::wait::Deadline::Poll;
    let r = loop {
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
            Err(Error::WouldBlock) if wait => {
                if let Err(e) = block_until(cell, deadline) {
                    break Err(e);
                }
            }
            Ok(()) if !msg.cap_slice().is_empty() => {
                // Las capacidades de memoria enviadas ya no se ven.
                cell.revoke_mappings();
                // Mover extremos puede dejar un ciclo inalcanzable.
                cell::ipc_gc();
                break Ok(());
            }
            r => break r,
        }
    };
    if matches!(deadline, nsos_kcore::wait::Deadline::At(_)) {
        sched::cancel_wake_at();
    }
    r
}

fn channel_recv(cell: &Cell, h: CapHandle, uptr: u64, fl: u64) -> SysResult<u64> {
    let deadline = flags_deadline(fl)?;
    copy_to_user(cell, uptr, &[0u8; MSG_SIZE])?; // el búfer debe ser escribible
    let ep = endpoint(cell, h, Rights::RECV)?;
    recv_msg(cell, &ep, uptr, deadline).map(|_| 0)
}

/// Recibe en `ep` hasta `deadline` y escribe el mensaje en `uptr` (ya
/// comprobado escribible). Las capacidades entran al CSpace o, si no caben,
/// el mensaje vuelve a la cola entero.
fn recv_msg(
    cell: &Cell,
    ep: &cell::Endpoint,
    uptr: u64,
    deadline: nsos_kcore::wait::Deadline,
) -> SysResult<()> {
    let wait = deadline != nsos_kcore::wait::Deadline::Poll;
    let r = loop {
        match ep.try_recv(wait) {
            Ok(mut q) => {
                let caps = q.take_caps();
                match cell.cspace.lock().insert_all(caps) {
                    Ok(hs) => break Ok((q, hs)),
                    Err((e, caps)) => {
                        q.put_caps(caps);
                        ep.unrecv(q);
                        break Err(e);
                    }
                }
            }
            Err(Error::WouldBlock) if wait => {
                if let Err(e) = block_until(cell, deadline) {
                    break Err(e);
                }
            }
            Err(e) => break Err(e),
        }
    };
    if matches!(deadline, nsos_kcore::wait::Deadline::At(_)) {
        sched::cancel_wake_at();
    }
    let (q, handles) = r?;
    let mut m = Message {
        header: q.header,
        data: q.data,
        ..Message::default()
    };
    m.header.cap_count = handles.len() as u8;
    m.caps[..handles.len()].copy_from_slice(&handles);
    copy_to_user(cell, uptr, &m.to_bytes())?;
    Ok(())
}

/// `channel_call(h, msg, timeout_ns)`: envía `msg` por `h` y espera la
/// respuesta en el mismo extremo, que se escribe sobre `msg`. Requiere
/// `SEND` y `RECV` (se comprueban antes de enviar). El plazo cubre las dos
/// mitades: `0` no espera nunca, `WAIT_FOREVER` no vence.
///
/// No agrega pasos al modelo de IPC: es un `send` seguido de un `recv` en el
/// mismo extremo (`Sys.call_inv`). Si el envío falla, no se mandó nada; si
/// falla la espera de la respuesta (`TimedOut`, `PeerClosed`, cell muerta), el
/// pedido **ya** se mandó. La respuesta es el próximo mensaje que llegue a
/// `h`: quien quiera emparejar pedidos y respuestas lo hace en el protocolo.
/// No se puede adjuntar el propio `h` (`InvalidArgs`).
fn channel_call(cell: &Cell, h: CapHandle, uptr: u64, timeout: u64) -> SysResult<u64> {
    let deadline = nsos_kcore::wait::deadline(crate::arch::time::now_ns(), timeout);
    let mut raw = [0u8; MSG_SIZE];
    copy_from_user(cell, uptr, &mut raw)?;
    copy_to_user(cell, uptr, &raw)?; // la respuesta va al mismo búfer
    let msg = Message::from_bytes(&raw)?;
    // Mandar el propio extremo lo sacaría del CSpace: esperar la respuesta en
    // él sería usar una capacidad que la cell ya no tiene.
    if msg.cap_slice().contains(&h) {
        return Err(Error::InvalidArgs);
    }
    let ep = endpoint(cell, h, Rights::SEND | Rights::RECV)?;
    send_msg(cell, h, &msg, deadline)?;
    recv_msg(cell, &ep, uptr, deadline).map(|_| 0)
}

/// Objeto que se puede esperar con `wait`.
enum Watched {
    Channel(Arc<cell::Endpoint>),
    Cell(Arc<Cell>),
    Irq(Arc<crate::irq::IrqLine>),
}

impl Watched {
    /// Paso `scan` del modelo (`verification/lean/Nsos/Wait.lean`): señales
    /// activas y, si `register`, el hilo queda anotado en las pedidas.
    fn watch(&self, wanted: u32, register: bool) -> u32 {
        match self {
            Watched::Channel(e) => e.watch(wanted, register),
            Watched::Cell(c) => nsos_kcore::wait::cell_signals(c.watch_exit(register)),
            Watched::Irq(i) => nsos_kcore::wait::irq_signals(i.watch(register)),
        }
    }
}

/// `wait(items, n, timeout_ns) -> n_ready`.
///
/// Sigue el modelo `Nsos.WaitModel`: primero marca al hilo bloqueado
/// (`start`), después recorre los ítems registrándose y mirando cada uno bajo
/// el lock de su objeto (`scan`; si alguno está listo se despierta a sí
/// mismo) y recién entonces cede la CPU (`finish`). Al volver (`resume`)
/// mira todo de nuevo. Así ningún evento que ocurra durante el recorrido se
/// pierde (`no_lost_wakeup`).
fn wait(cell: &Cell, uptr: u64, n: u64, timeout: u64) -> SysResult<u64> {
    use nsos_abi::syscall::WaitItem;
    use nsos_kcore::wait::{self as w, Deadline, WAIT_MAX};
    let n = usize::try_from(n).map_err(|_| Error::InvalidArgs)?;
    if n == 0 || n > WAIT_MAX {
        return Err(Error::InvalidArgs);
    }
    let mut raw = [0u8; WaitItem::SIZE * WAIT_MAX];
    let raw = &mut raw[..WaitItem::SIZE * n];
    copy_from_user(cell, uptr, raw)?;
    copy_to_user(cell, uptr, raw)?; // la respuesta va al mismo arreglo
    let mut items: alloc::vec::Vec<WaitItem> = raw
        .as_chunks::<{ WaitItem::SIZE }>()
        .0
        .iter()
        .map(WaitItem::from_bytes)
        .collect();
    // Resolver los handles una vez: cada uno con `WAIT` y señales que su
    // objeto pueda emitir. Las referencias mantienen vivos los objetos.
    let watched: alloc::vec::Vec<Watched> = {
        let cs = cell.cspace.lock();
        items
            .iter()
            .map(|it| {
                let c = cs.get_with(CapHandle(it.handle), Rights::WAIT)?;
                w::check_item(it, c.object.object_type())?;
                match &c.object {
                    KObject::Channel(e) => Ok(Watched::Channel(e.clone())),
                    KObject::Cell(x) => Ok(Watched::Cell(x.clone())),
                    KObject::Irq(x) => Ok(Watched::Irq(x.clone())),
                    // `check_item` ya rechazó los tipos que no emiten señales.
                    _ => Err(Error::WrongType),
                }
            })
            .collect::<SysResult<_>>()?
    };
    let deadline = w::deadline(crate::arch::time::now_ns(), timeout);
    let block = deadline != Deadline::Poll;
    let r = loop {
        if block {
            sched::block_current(); // start
        }
        let mut ready = 0;
        for (it, x) in items.iter_mut().zip(&watched) {
            it.observed = x.watch(it.wanted, block); // scan
            if w::ready(it) {
                ready += 1;
            }
        }
        if ready > 0 || w::expired(deadline, crate::arch::time::now_ns()) {
            if block {
                sched::wake(sched::current()); // seguimos corriendo
            }
            break if ready > 0 {
                Ok(ready as u64)
            } else {
                Err(Error::TimedOut)
            };
        }
        if let Deadline::At(d) = deadline {
            sched::wake_at(d);
        }
        // finish + resume; con `cell_kill` pendiente, abandona.
        if let Err(e) = sleep(cell) {
            break Err(e);
        }
    };
    if block {
        sched::cancel_wake_at();
    }
    for (chunk, it) in raw.chunks_mut(WaitItem::SIZE).zip(&items) {
        chunk.copy_from_slice(&it.to_bytes());
    }
    copy_to_user(cell, uptr, raw)?;
    r
}

/// Una cell causó una excepción de CPU (`vector`: código de la
/// arquitectura, en `pc`, sobre `addr` si es un fallo de memoria): se la
/// termina y el kernel sigue.
pub fn user_fault(vector: u64, pc: u64, addr: u64, raw: *mut TrapFrame) -> *mut TrapFrame {
    if let Some(cell) = sched::current_cell() {
        kprintln!(
            "cell \"{}\": excepción {:#x} en pc={:#x} (dir {:#x}); el kernel la termina",
            cell.name,
            vector,
            pc,
            addr
        );
        cell::terminate(
            &cell,
            ExitStatus::Faulted {
                vector,
                rip: pc,
                addr,
            },
        );
    }
    sched::exit_from_trap(raw)
}
