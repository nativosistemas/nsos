//! Cells: procesos aislados con su propio espacio de direcciones y su tabla
//! de capacidades, y los objetos del kernel que pueden referenciar.
//!
//! Orden de locks (para evitar interbloqueos):
//! `cspace` de una cell → lock de un canal → scheduler.
//! Nunca se suelta (drop) un mensaje con capacidades teniendo el lock de un
//! canal tomado, porque soltar un extremo toma el lock de su canal.

use crate::arch::paging::{flags, AddressSpace, PageSize, PAGE_SIZE};
use crate::kprintln;
use crate::mm::{self, phys_to_virt, pmm, PhysAddr};
use crate::sched::{self, ThreadId};
use crate::sync::IrqSpinLock;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use nsos_abi::msg::{MsgHeader, MAX_DATA};
use nsos_abi::{Error, ObjectType, Rights};
use nsos_kcore::cspace::{CSpace, Cap};
use nsos_kcore::elf;

/// Capacidades vivas máximas por cell.
pub const CSPACE_LIMIT: u32 = 1024;
/// Mensajes máximos encolados por extremo.
pub const QUEUE_LIMIT: usize = 64;
/// Tope de la pila de usuario.
pub const USER_STACK_TOP: u64 = 0x0000_7fff_fff0_0000;
/// Tamaño de la pila de usuario.
pub const USER_STACK_SIZE: u64 = 64 * 1024;

// ---------------------------------------------------------------- objetos

/// Objeto del kernel referenciable por una capacidad.
#[derive(Clone)]
pub enum KObject {
    /// Extremo de canal.
    Channel(Arc<Endpoint>),
    /// Objeto de memoria (marcos contiguos).
    Memory(Arc<MemObj>),
    /// Rango de puertos de E/S.
    IoPorts(Arc<PortRange>),
}

impl KObject {
    /// Tipo de objeto del ABI.
    pub fn object_type(&self) -> ObjectType {
        match self {
            KObject::Channel(_) => ObjectType::Channel,
            KObject::Memory(_) => ObjectType::Memory,
            KObject::IoPorts(_) => ObjectType::IoPorts,
        }
    }
    /// Identificador para depuración.
    pub fn koid(&self) -> u64 {
        match self {
            KObject::Channel(e) => Arc::as_ptr(e) as u64,
            KObject::Memory(m) => Arc::as_ptr(m) as u64,
            KObject::IoPorts(p) => Arc::as_ptr(p) as u64,
        }
    }
}

/// Objeto de memoria: `pages` marcos físicos contiguos, a cero al crearse.
/// Se libera cuando muere su última capacidad *y* su último mapeo.
pub struct MemObj {
    pub phys: PhysAddr,
    pub pages: usize,
}

impl MemObj {
    /// Reserva `pages` marcos contiguos.
    pub fn new(pages: usize) -> Option<Arc<MemObj>> {
        let phys = pmm::alloc_contiguous(pages, 1)?;
        // SAFETY: marcos recién reservados, nuestros, en el HHDM.
        unsafe {
            core::ptr::write_bytes(phys_to_virt(phys) as *mut u8, 0, pages * PAGE_SIZE as usize)
        };
        Some(Arc::new(MemObj { phys, pages }))
    }

    /// Tamaño en bytes.
    pub fn size(&self) -> u64 {
        self.pages as u64 * PAGE_SIZE
    }
}

impl Drop for MemObj {
    fn drop(&mut self) {
        for i in 0..self.pages {
            pmm::free_frame(self.phys + i as u64 * PAGE_SIZE);
        }
    }
}

/// Rango de puertos `[base, base+len)` entregado a un driver.
pub struct PortRange {
    pub base: u16,
    pub len: u16,
}

/// Derechos de una capacidad de memoria DMA para un driver.
pub const DMA_RIGHTS: Rights = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::CONTROL.0);
/// Derechos de un rango de puertos para un driver.
pub const PORT_RIGHTS: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);

/// Capacidad del kernel.
pub type KCap = Cap<KObject>;

// ---------------------------------------------------------------- canales

/// Mensaje encolado: las capacidades ya no pertenecen a ninguna cell.
pub struct QueuedMsg {
    pub header: MsgHeader,
    pub data: [u8; MAX_DATA],
    pub caps: Vec<KCap>,
}

#[derive(Default)]
struct Side {
    /// Mensajes que le llegan a este extremo.
    queue: VecDeque<QueuedMsg>,
    /// Este extremo ya no tiene dueños.
    closed: bool,
    /// Hilo esperando recibir en este extremo.
    recv_waiter: Option<ThreadId>,
    /// Hilo esperando espacio para enviar *hacia* este extremo.
    send_waiter: Option<ThreadId>,
}

/// Par de extremos conectados.
pub struct ChannelPair {
    sides: IrqSpinLock<[Side; 2]>,
}

/// Un extremo de canal. Se cierra cuando muere su última capacidad.
pub struct Endpoint {
    pair: Arc<ChannelPair>,
    side: usize,
}

/// Derechos iniciales de un extremo recién creado.
pub const ENDPOINT_RIGHTS: Rights = Rights(
    Rights::SEND.0 | Rights::RECV.0 | Rights::GRANT.0 | Rights::DUPLICATE.0 | Rights::WAIT.0,
);

/// Crea un canal y devuelve sus dos extremos.
pub fn channel_create() -> (Arc<Endpoint>, Arc<Endpoint>) {
    let pair = Arc::new(ChannelPair {
        sides: IrqSpinLock::new(Default::default()),
    });
    (
        Arc::new(Endpoint {
            pair: pair.clone(),
            side: 0,
        }),
        Arc::new(Endpoint { pair, side: 1 }),
    )
}

impl Endpoint {
    fn peer(&self) -> usize {
        1 - self.side
    }

    /// Envía un mensaje construido por `build` *bajo el lock del canal*:
    /// solo se llama a `build` (que mueve las capacidades fuera del CSpace
    /// del emisor) si el envío va a tener éxito, así el envío es atómico.
    /// Con la cola llena devuelve `WouldBlock` y, si `wait`, deja al hilo
    /// registrado y marcado bloqueado.
    pub fn send_with(
        &self,
        wait: bool,
        build: impl FnOnce() -> Result<QueuedMsg, Error>,
    ) -> Result<(), Error> {
        let mut sides = self.pair.sides.lock();
        let peer = &mut sides[self.peer()];
        if peer.closed {
            return Err(Error::PeerClosed);
        }
        if peer.queue.len() >= QUEUE_LIMIT {
            if wait {
                peer.send_waiter = Some(sched::current());
                sched::block_current();
            }
            return Err(Error::WouldBlock);
        }
        let msg = build()?;
        peer.queue.push_back(msg);
        if let Some(t) = peer.recv_waiter.take() {
            sched::wake(t);
        }
        Ok(())
    }

    /// Intenta sacar un mensaje. `Err(WouldBlock)` si no hay (y, si `wait`,
    /// el hilo queda registrado y marcado bloqueado); `Err(PeerClosed)` si no
    /// hay y el peer cerró.
    pub fn try_recv(&self, wait: bool) -> Result<QueuedMsg, Error> {
        let mut sides = self.pair.sides.lock();
        let peer_closed = sides[self.peer()].closed;
        let me = &mut sides[self.side];
        if let Some(m) = me.queue.pop_front() {
            if let Some(t) = me.send_waiter.take() {
                sched::wake(t);
            }
            return Ok(m);
        }
        if peer_closed {
            return Err(Error::PeerClosed);
        }
        if wait {
            me.recv_waiter = Some(sched::current());
            sched::block_current();
        }
        Err(Error::WouldBlock)
    }

    /// Devuelve un mensaje al frente de la cola (si el receptor no pudo
    /// aceptarlo, p. ej. por CSpace lleno).
    pub fn unrecv(&self, m: QueuedMsg) {
        self.pair.sides.lock()[self.side].queue.push_front(m);
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let (orphans, waiters) = {
            let mut sides = self.pair.sides.lock();
            let me = &mut sides[self.side];
            me.closed = true;
            let orphans = core::mem::take(&mut me.queue);
            let w1 = me.send_waiter.take();
            let peer = &mut sides[1 - self.side];
            let w2 = peer.recv_waiter.take();
            let w3 = peer.send_waiter.take();
            (orphans, [w1, w2, w3])
        };
        for t in waiters.into_iter().flatten() {
            sched::wake(t);
        }
        // Soltar fuera del lock: pueden contener extremos de este mismo canal.
        drop(orphans);
    }
}

// ---------------------------------------------------------------- cells

/// Cómo terminó una cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// Llamó a `exit(code)`.
    Exited(i32),
    /// El kernel la terminó por una excepción de CPU.
    Faulted { vector: u64, rip: u64, addr: u64 },
}

/// Una cell.
pub struct Cell {
    pub id: u64,
    pub name: String,
    root: AtomicU64,
    space: IrqSpinLock<Option<AddressSpace>>,
    pub cspace: IrqSpinLock<CSpace<KObject>>,
    /// Marcos de usuario propiedad de la cell.
    frames: IrqSpinLock<Vec<PhysAddr>>,
    /// Objetos de memoria mapeados (los mantiene vivos mientras estén
    /// mapeados, aunque la cell suelte la capacidad).
    mappings: IrqSpinLock<Vec<Arc<MemObj>>>,
}

static NEXT_CELL: AtomicU64 = AtomicU64::new(1);

/// Registro de terminaciones (lo consultan los tests).
pub static EXITS: IrqSpinLock<Vec<(String, ExitStatus)>> = IrqSpinLock::new(Vec::new());

impl Cell {
    /// CR3 de la cell (0 si ya se destruyó).
    pub fn root(&self) -> PhysAddr {
        self.root.load(Ordering::Relaxed)
    }

    /// Ejecuta `f` sobre el espacio de direcciones (si sigue vivo).
    pub fn with_space<R>(&self, f: impl FnOnce(&AddressSpace) -> R) -> Option<R> {
        self.space.lock().as_ref().map(f)
    }
}

/// Error al crear una cell (los campos se muestran vía `Debug`).
#[derive(Debug)]
#[allow(dead_code)]
pub enum SpawnError {
    Elf(elf::ElfError),
    NoMemory,
    Caps(Error),
}

fn map_user_page(
    space: &mut AddressSpace,
    frames: &mut Vec<PhysAddr>,
    v: u64,
    f: u64,
) -> Result<PhysAddr, SpawnError> {
    let p = pmm::alloc_frame().ok_or(SpawnError::NoMemory)?;
    frames.push(p);
    let mut alloc = pmm::alloc_frame;
    space
        .map(v, p, PageSize::Size4K, f | flags::USER, &mut alloc)
        .map_err(|_| SpawnError::NoMemory)?;
    Ok(p)
}

/// Crea una cell a partir de una imagen ELF y le entrega `caps` en los slots
/// 1..=n de su CSpace. Arranca su hilo principal.
pub fn spawn(name: &str, image: &[u8], caps: Vec<KCap>) -> Result<Arc<Cell>, SpawnError> {
    let img = elf::parse(image).map_err(SpawnError::Elf)?;
    let mut alloc = pmm::alloc_frame;
    let mut space = {
        let k = mm::KERNEL_SPACE.lock();
        AddressSpace::new_user(k.as_ref().expect("mm sin inicializar"), &mut alloc)
            .ok_or(SpawnError::NoMemory)?
    };
    let mut frames = Vec::new();

    for seg in &img.segments {
        let mut f = 0;
        if seg.write {
            f |= flags::WRITABLE;
        }
        if !seg.exec {
            f |= flags::NO_EXECUTE;
        }
        let mut off = 0;
        while off < seg.memsz {
            let p = map_user_page(&mut space, &mut frames, seg.vaddr + off, f)?;
            // Copiar la parte del archivo que cae en esta página (el resto ya
            // está a cero).
            if off < seg.filesz {
                let n = (seg.filesz - off).min(PAGE_SIZE) as usize;
                let src = &image[(seg.offset + off) as usize..][..n];
                // SAFETY: `p` es un marco recién reservado (nuestro) en el HHDM.
                unsafe {
                    core::ptr::copy_nonoverlapping(src.as_ptr(), phys_to_virt(p) as *mut u8, n)
                };
            }
            off += PAGE_SIZE;
        }
    }
    // Pila de usuario (con una página de guarda sin mapear debajo).
    let mut v = USER_STACK_TOP - USER_STACK_SIZE;
    while v < USER_STACK_TOP {
        map_user_page(
            &mut space,
            &mut frames,
            v,
            flags::WRITABLE | flags::NO_EXECUTE,
        )?;
        v += PAGE_SIZE;
    }

    let mut cspace = CSpace::new(CSPACE_LIMIT);
    cspace
        .insert_all(caps)
        .map_err(|(e, _)| SpawnError::Caps(e))?;

    let cell = Arc::new(Cell {
        id: NEXT_CELL.fetch_add(1, Ordering::Relaxed),
        name: String::from(name),
        root: AtomicU64::new(space.root()),
        space: IrqSpinLock::new(Some(space)),
        cspace: IrqSpinLock::new(cspace),
        frames: IrqSpinLock::new(frames),
        mappings: IrqSpinLock::new(Vec::new()),
    });
    kprintln!(
        "cell {} \"{}\": {} segmentos, entrada {:#x}",
        cell.id,
        name,
        img.segments.len(),
        img.entry
    );
    let tname: &'static str =
        alloc::boxed::Box::leak(alloc::string::String::from(name).into_boxed_str());
    sched::spawn_user(tname, cell.clone(), img.entry, USER_STACK_TOP - 8);
    Ok(cell)
}

/// Termina la cell `cell` desde un contexto que corre en su espacio (su
/// propio hilo). Libera capacidades (los peers ven `PEER_CLOSED`), memoria y
/// tablas. El llamador después termina el hilo.
pub fn terminate(cell: &Cell, status: ExitStatus) {
    kprintln!("cell {} \"{}\" terminó: {:?}", cell.id, cell.name, status);
    EXITS.lock().push((cell.name.clone(), status));

    let caps = cell.cspace.lock().clear();
    drop(caps); // fuera del lock del CSpace

    // Dejar de usar el espacio de la cell antes de liberarlo.
    // SAFETY: el espacio del kernel mapea todo lo que el kernel usa.
    unsafe { crate::arch::cpu::write_cr3(mm::kernel_root()) };
    cell.root.store(0, Ordering::Relaxed);
    if let Some(space) = cell.space.lock().take() {
        space.destroy_user(&mut |p| pmm::free_frame(p));
    }
    for p in core::mem::take(&mut *cell.frames.lock()) {
        pmm::free_frame(p);
    }
    drop(core::mem::take(&mut *cell.mappings.lock()));
}

impl Cell {
    /// Mapea `obj` en `vaddr` con los permisos pedidos (W^X).
    pub fn map_memory(
        &self,
        obj: Arc<MemObj>,
        vaddr: u64,
        write: bool,
        exec: bool,
    ) -> Result<(), Error> {
        if write && exec {
            return Err(Error::InvalidArgs);
        }
        if !vaddr.is_multiple_of(PAGE_SIZE) || vaddr < elf::USER_MIN {
            return Err(Error::InvalidArgs);
        }
        let end = vaddr.checked_add(obj.size()).ok_or(Error::InvalidArgs)?;
        if end > elf::USER_MAX {
            return Err(Error::InvalidArgs);
        }
        let mut g = self.space.lock();
        let space = g.as_mut().ok_or(Error::InvalidArgs)?;
        // Todo o nada: comprobar que el rango está libre antes de mapear.
        for i in 0..obj.pages as u64 {
            if space.translate(vaddr + i * PAGE_SIZE).is_some() {
                return Err(Error::InvalidArgs);
            }
        }
        let mut f = flags::USER;
        if write {
            f |= flags::WRITABLE;
        }
        if !exec {
            f |= flags::NO_EXECUTE;
        }
        let mut alloc = pmm::alloc_frame;
        for i in 0..obj.pages as u64 {
            space
                .map(
                    vaddr + i * PAGE_SIZE,
                    obj.phys + i * PAGE_SIZE,
                    PageSize::Size4K,
                    f,
                    &mut alloc,
                )
                .map_err(|_| Error::NoMemory)?;
        }
        drop(g);
        self.mappings.lock().push(obj);
        Ok(())
    }
}
