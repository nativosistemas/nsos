//! Cells: procesos aislados con su propio espacio de direcciones y su tabla
//! de capacidades, y los objetos del kernel que pueden referenciar.
//!
//! Orden de locks (para evitar interbloqueos):
//! `cspace` de una cell → lock de un canal → registro `IN_FLIGHT` → scheduler.
//! Nunca se suelta (drop) un mensaje con capacidades teniendo el lock de un
//! canal tomado, porque soltar un extremo toma el lock de su canal.

use crate::arch::paging::{flags, AddressSpace, PageSize, PAGE_SIZE};
use crate::kprintln;
use crate::mm::{self, phys_to_virt, pmm, PhysAddr};
use crate::sched::{self, ThreadId};
use crate::sync::IrqSpinLock;
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// Una cell (para supervisarla: `cell_status`, `cell_kill`).
    Cell(Arc<Cell>),
    /// Imagen ELF desde la que se pueden crear cells.
    Image(Arc<Image>),
    /// Línea de interrupción de un dispositivo (para su driver).
    Irq(Arc<crate::irq::IrqLine>),
}

impl KObject {
    /// Tipo de objeto del ABI.
    pub fn object_type(&self) -> ObjectType {
        match self {
            KObject::Channel(_) => ObjectType::Channel,
            KObject::Memory(_) => ObjectType::Memory,
            KObject::IoPorts(_) => ObjectType::IoPorts,
            KObject::Cell(_) => ObjectType::Cell,
            KObject::Image(_) => ObjectType::Image,
            KObject::Irq(_) => ObjectType::Irq,
        }
    }
    /// Identificador para depuración.
    pub fn koid(&self) -> u64 {
        match self {
            KObject::Channel(e) => Arc::as_ptr(e) as u64,
            KObject::Memory(m) => Arc::as_ptr(m) as u64,
            KObject::IoPorts(p) => Arc::as_ptr(p) as u64,
            KObject::Cell(c) => Arc::as_ptr(c) as u64,
            KObject::Image(i) => Arc::as_ptr(i) as u64,
            KObject::Irq(i) => Arc::as_ptr(i) as u64,
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

    /// Objeto nuevo con `data` al principio (el resto a cero).
    pub fn from_bytes(data: &[u8]) -> Option<Arc<MemObj>> {
        let pages = (data.len() as u64).div_ceil(PAGE_SIZE).max(1) as usize;
        let m = MemObj::new(pages)?;
        // SAFETY: marcos recién reservados y nuestros (nadie más tiene el
        // `Arc` todavía); `data` cabe en `pages` páginas.
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                phys_to_virt(m.phys) as *mut u8,
                data.len(),
            )
        };
        Some(m)
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

/// Imagen ELF inmutable (un módulo del bootloader). Validarla es trabajo de
/// `spawn`; tenerla solo permite crear cells a partir de ella.
pub struct Image {
    pub name: String,
    pub bytes: &'static [u8],
}

/// Derechos de una capacidad a una imagen.
pub const IMAGE_RIGHTS: Rights = Rights(Rights::EXEC.0 | Rights::DUPLICATE.0 | Rights::GRANT.0);
/// Derechos de la capacidad que recibe quien crea una cell.
pub const CELL_RIGHTS: Rights =
    Rights(Rights::CONTROL.0 | Rights::WAIT.0 | Rights::DUPLICATE.0 | Rights::GRANT.0);

/// Derechos de la memoria que crea una cell con `mem_create`.
pub const MEM_RIGHTS: Rights =
    Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::DUPLICATE.0 | Rights::GRANT.0);
/// Derechos de una capacidad de memoria DMA para un driver.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub const DMA_RIGHTS: Rights = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::CONTROL.0);
/// Derechos de una línea de interrupción para un driver: `irq_ack` y
/// `wait`. Sin `GRANT` ni `DUPLICATE`: no se puede pasar a otra cell.
pub const IRQ_RIGHTS: Rights = Rights(Rights::CONTROL.0 | Rights::WAIT.0);
/// Derechos de un rango de puertos para un driver.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub const PORT_RIGHTS: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);

/// Capacidad del kernel.
pub type KCap = Cap<KObject>;

// ---------------------------------------------------------------- canales

/// Mensaje encolado: las capacidades ya no pertenecen a ninguna cell.
///
/// Crear uno con [`QueuedMsg::new`] marca sus extremos como "en vuelo" para
/// el recolector; sacarle las capacidades con [`QueuedMsg::take_caps`] o
/// soltarlo los desmarca.
pub struct QueuedMsg {
    pub header: MsgHeader,
    pub data: [u8; MAX_DATA],
    caps: Vec<KCap>,
}

impl QueuedMsg {
    /// Arma un mensaje con capacidades que acaban de salir de una CSpace.
    pub fn new(header: MsgHeader, data: [u8; MAX_DATA], caps: Vec<KCap>) -> QueuedMsg {
        mark_in_flight(&caps);
        QueuedMsg { header, data, caps }
    }

    /// Saca las capacidades (para entregarlas a una CSpace).
    pub fn take_caps(&mut self) -> Vec<KCap> {
        let caps = core::mem::take(&mut self.caps);
        unmark_in_flight(&caps);
        caps
    }

    /// Devuelve capacidades que no se pudieron entregar.
    pub fn put_caps(&mut self, caps: Vec<KCap>) {
        mark_in_flight(&caps);
        self.caps = caps;
    }
}

impl Drop for QueuedMsg {
    fn drop(&mut self) {
        unmark_in_flight(&self.caps);
    }
}

// ------------------------------------------------- recolector de ciclos

/// Extremos que viajan (o viajaron) en algún mensaje. Puede tener entradas
/// muertas o repetidas: el recolector las limpia.
static IN_FLIGHT: IrqSpinLock<Vec<Weak<Endpoint>>> = IrqSpinLock::new(Vec::new());

fn mark_in_flight(caps: &[KCap]) {
    // Solo los extremos de canal pueden formar ciclos (son los únicos objetos
    // que contienen capacidades).
    for c in caps {
        let KObject::Channel(e) = &c.object else {
            continue;
        };
        if e.in_flight.fetch_add(1, Ordering::Relaxed) == 0 {
            IN_FLIGHT.lock().push(Arc::downgrade(e));
        }
    }
}

fn unmark_in_flight(caps: &[KCap]) {
    for c in caps {
        let KObject::Channel(e) = &c.object else {
            continue;
        };
        e.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Libera los extremos en vuelo que ninguna cell puede volver a alcanzar
/// (ciclos de extremos que viajan en colas). El algoritmo está en
/// [`nsos_kcore::ipc_gc`]; la especificación, en `Sys.gc` del modelo en Lean.
///
/// Corre con las interrupciones deshabilitadas (un solo núcleo), así que los
/// contadores de referencias no cambian mientras arma el grafo. Las
/// referencias temporales de otros hilos cuentan como externas: el recolector
/// es conservador y nunca libera algo vivo.
pub fn ipc_gc() -> usize {
    let (nodes, garbage_queues) = crate::arch::cpu::without_interrupts(|| {
        // Nodos: extremos vivos con algo en vuelo, sin repetir. El registro
        // se suelta antes de tomar locks de canales (orden: canal → registro).
        let mut index = BTreeMap::new();
        let mut nodes: Vec<Arc<Endpoint>> = Vec::new();
        IN_FLIGHT.lock().retain(|w| match w.upgrade() {
            Some(e) if e.in_flight.load(Ordering::Relaxed) > 0 => {
                let key = Arc::as_ptr(&e) as usize;
                if index.contains_key(&key) {
                    return false;
                }
                index.insert(key, nodes.len());
                nodes.push(e);
                true
            }
            _ => false,
        });
        if nodes.is_empty() {
            return (nodes, Vec::new());
        }
        let graph: Vec<nsos_kcore::ipc_gc::Node> = nodes
            .iter()
            .map(|e| {
                let in_flight = e.in_flight.load(Ordering::Relaxed);
                let sides = e.pair.sides.lock();
                let contents = sides[e.side]
                    .queue
                    .iter()
                    .flat_map(|m| m.caps.iter())
                    .filter_map(|c| {
                        let KObject::Channel(x) = &c.object else {
                            return None;
                        };
                        index.get(&(Arc::as_ptr(x) as usize)).copied()
                    })
                    .collect();
                nsos_kcore::ipc_gc::Node {
                    // Una referencia es la nuestra (`nodes`).
                    candidate: Arc::strong_count(e) - 1 == in_flight,
                    in_flight,
                    contents,
                }
            })
            .collect();
        let garbage = nsos_kcore::ipc_gc::garbage(&graph);
        let queues: Vec<VecDeque<QueuedMsg>> = nodes
            .iter()
            .zip(&garbage)
            .filter(|(_, g)| **g)
            .map(|(e, _)| core::mem::take(&mut e.pair.sides.lock()[e.side].queue))
            .collect();
        (nodes, queues)
    });
    let freed = garbage_queues.len();
    // Soltar fuera de los locks: al caer las últimas referencias corre
    // `Drop for Endpoint`, que cierra el canal y despierta a los peers.
    drop(garbage_queues);
    drop(nodes);
    freed
}

#[derive(Default)]
struct Side {
    /// Mensajes que le llegan a este extremo.
    queue: VecDeque<QueuedMsg>,
    /// Este extremo ya no tiene dueños.
    closed: bool,
    /// Hilos esperando recibir en este extremo. Pueden ser varios (varias
    /// cells con el mismo extremo): con un solo lugar, la segunda espera
    /// pisaba a la primera y esa no se despertaba nunca (lo encontró el
    /// modelo de `verification/lean/Nsos/Ipc.lean`).
    recv_waiters: Vec<ThreadId>,
    /// Hilos esperando espacio para enviar *hacia* este extremo.
    send_waiters: Vec<ThreadId>,
}

/// Registra al hilo actual en una lista de espera (una sola vez) y lo marca
/// bloqueado.
fn wait_on(list: &mut Vec<ThreadId>) {
    let me = sched::current();
    if !list.contains(&me) {
        list.push(me);
    }
    sched::block_current();
}

/// Despierta a todos los hilos de una lista de espera y la vacía.
fn wake_all(list: &mut Vec<ThreadId>) {
    for t in core::mem::take(list) {
        sched::wake(t);
    }
}

/// Par de extremos conectados.
pub struct ChannelPair {
    sides: IrqSpinLock<[Side; 2]>,
    /// Contrato declarado al crearlo (0 = sin contrato). No cambia nunca: no
    /// hay forma de escribirlo después de `channel_create`
    /// (`Sys.step_contract_fixed` en `verification/lean/Nsos/ContractId.lean`).
    contract: u64,
}

/// Un extremo de canal. Se cierra cuando muere su última capacidad.
pub struct Endpoint {
    pair: Arc<ChannelPair>,
    side: usize,
    /// Cuántas capacidades a este extremo viajan en mensajes encolados.
    in_flight: AtomicUsize,
}

/// Derechos iniciales de un extremo recién creado.
pub const ENDPOINT_RIGHTS: Rights = Rights(
    Rights::SEND.0 | Rights::RECV.0 | Rights::GRANT.0 | Rights::DUPLICATE.0 | Rights::WAIT.0,
);

/// Crea un canal con el contrato `contract` y devuelve sus dos extremos.
pub fn channel_create(contract: u64) -> (Arc<Endpoint>, Arc<Endpoint>) {
    let pair = Arc::new(ChannelPair {
        sides: IrqSpinLock::new(Default::default()),
        contract,
    });
    (
        Arc::new(Endpoint {
            pair: pair.clone(),
            side: 0,
            in_flight: AtomicUsize::new(0),
        }),
        Arc::new(Endpoint {
            pair,
            side: 1,
            in_flight: AtomicUsize::new(0),
        }),
    )
}

impl Endpoint {
    fn peer(&self) -> usize {
        1 - self.side
    }

    /// El contrato del canal (el mismo para los dos extremos).
    pub fn contract(&self) -> u64 {
        self.pair.contract
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
                wait_on(&mut peer.send_waiters);
            }
            return Err(Error::WouldBlock);
        }
        let msg = build()?;
        peer.queue.push_back(msg);
        wake_all(&mut peer.recv_waiters);
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
            wake_all(&mut me.send_waiters);
            return Ok(m);
        }
        if peer_closed {
            return Err(Error::PeerClosed);
        }
        if wait {
            wait_on(&mut me.recv_waiters);
        }
        Err(Error::WouldBlock)
    }

    /// Devuelve un mensaje al frente de la cola (si el receptor no pudo
    /// aceptarlo, p. ej. por CSpace lleno). Vuelve a haber un mensaje: se
    /// despierta a quienes esperaban `READABLE` (como `raise` en
    /// `verification/lean/Nsos/Wait.lean`).
    pub fn unrecv(&self, m: QueuedMsg) {
        let mut sides = self.pair.sides.lock();
        let me = &mut sides[self.side];
        me.queue.push_front(m);
        wake_all(&mut me.recv_waiters);
    }

    /// Paso `scan` de `wait` para este extremo, bajo el lock del canal:
    /// devuelve las señales activas y, si `register`, deja anotado al hilo
    /// actual en la lista de espera de cada señal pedida en `wanted`:
    /// * `READABLE` y `PEER_CLOSED` en la de este extremo (la despiertan
    ///   `send` y el cierre de cualquiera de las dos puntas);
    /// * `WRITABLE` en la de quienes envían hacia el peer (la despiertan
    ///   `recv` del peer y el cierre).
    pub fn watch(&self, wanted: u32, register: bool) -> u32 {
        use nsos_abi::cap::signals::{PEER_CLOSED, READABLE, WRITABLE};
        let mut sides = self.pair.sides.lock();
        if register {
            let me = sched::current();
            let add = |list: &mut Vec<ThreadId>| {
                if !list.contains(&me) {
                    list.push(me);
                }
            };
            if wanted & (READABLE | PEER_CLOSED) != 0 {
                add(&mut sides[self.side].recv_waiters);
            }
            if wanted & WRITABLE != 0 {
                add(&mut sides[self.peer()].send_waiters);
            }
        }
        nsos_kcore::wait::channel_signals(nsos_kcore::wait::ChannelState {
            queued: sides[self.side].queue.len(),
            peer_closed: sides[self.peer()].closed,
            peer_queued: sides[self.peer()].queue.len(),
            limit: QUEUE_LIMIT,
        })
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let (orphans, waiters) = {
            let mut sides = self.pair.sides.lock();
            let me = &mut sides[self.side];
            me.closed = true;
            let orphans = core::mem::take(&mut me.queue);
            // Todos los que esperaban en este extremo o en su peer: ninguno
            // puede seguir esperando (como `gc` en el modelo de Lean).
            let mut waiters = core::mem::take(&mut me.recv_waiters);
            waiters.append(&mut me.send_waiters);
            let peer = &mut sides[1 - self.side];
            waiters.append(&mut peer.recv_waiters);
            waiters.append(&mut peer.send_waiters);
            (orphans, waiters)
        };
        for t in waiters {
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
    /// La terminó otra cell con `cell_kill`.
    Killed,
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
    /// Objetos de memoria mapeados. Cada mapeo vale mientras la cell tenga
    /// una capacidad al objeto con los derechos del mapeo: ver
    /// `revoke_mappings`.
    mappings: IrqSpinLock<Vec<Mapping>>,
    /// Hilo principal (para despertarlo si se pide matarla).
    thread: AtomicU64,
    /// Se pidió terminarla (`cell_kill`): el hilo termina la próxima vez que
    /// vaya a volver a anillo 3.
    kill: AtomicBool,
    /// Cómo terminó (`None` mientras corre) e hilos esperando ese momento.
    exit: IrqSpinLock<(Option<ExitStatus>, Vec<ThreadId>)>,
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

    /// Pide terminar la cell. Si su hilo está bloqueado en el kernel, se lo
    /// despierta: los bucles de espera de las syscalls miran
    /// [`Cell::kill_requested`] y vuelven, y la ruta de vuelta a anillo 3
    /// termina la cell (ver `trap::dispatch`).
    pub fn request_kill(&self) {
        if self.exit.lock().0.is_some() {
            return;
        }
        self.kill.store(true, Ordering::Release);
        sched::wake(self.thread.load(Ordering::Relaxed));
    }

    /// ¿Se pidió terminarla?
    pub fn kill_requested(&self) -> bool {
        self.kill.load(Ordering::Acquire)
    }

    /// Paso `scan` de `wait` para `CELL_EXITED`: ¿ya terminó? Si sigue viva
    /// y `register`, deja anotado al hilo actual (lo despierta `terminate`).
    pub fn watch_exit(&self, register: bool) -> bool {
        let mut g = self.exit.lock();
        if g.0.is_none() && register {
            let me = sched::current();
            if !g.1.contains(&me) {
                g.1.push(me);
            }
        }
        g.0.is_some()
    }

    /// Cómo terminó, o `None` si sigue viva. Con `wait`, si sigue viva deja
    /// al hilo actual registrado y marcado bloqueado (como `try_recv`).
    pub fn exit_status(&self, wait: bool) -> Option<ExitStatus> {
        let mut g = self.exit.lock();
        if g.0.is_none() && wait {
            g.1.push(sched::current());
            sched::block_current();
        }
        g.0
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
        thread: AtomicU64::new(0),
        kill: AtomicBool::new(false),
        exit: IrqSpinLock::new((None, Vec::new())),
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
    let tid = sched::spawn_user(tname, cell.clone(), img.entry, USER_STACK_TOP);
    cell.thread.store(tid, Ordering::Relaxed);
    Ok(cell)
}

/// Termina la cell `cell` desde un contexto que corre en su espacio (su
/// propio hilo). Libera capacidades (los peers ven `PEER_CLOSED`), memoria y
/// tablas. El llamador después termina el hilo.
pub fn terminate(cell: &Cell, status: ExitStatus) {
    // Si se pidió matarla, ese es el motivo aunque haya llegado antes a
    // `exit` o a un fallo.
    let status = if cell.kill_requested() {
        ExitStatus::Killed
    } else {
        status
    };
    kprintln!("cell {} \"{}\" terminó: {:?}", cell.id, cell.name, status);
    EXITS.lock().push((cell.name.clone(), status));

    let caps = cell.cspace.lock().clear();
    drop(caps); // fuera del lock del CSpace
    ipc_gc(); // lo que solo esta cell alcanzaba ya es basura

    // Dejar de usar el espacio de la cell antes de liberarlo.
    // SAFETY: el espacio del kernel mapea todo lo que el kernel usa.
    unsafe { crate::arch::paging::switch_to(mm::kernel_root()) };
    cell.root.store(0, Ordering::Relaxed);
    if let Some(space) = cell.space.lock().take() {
        space.destroy_user(&mut |p| pmm::free_frame(p));
    }
    for p in core::mem::take(&mut *cell.frames.lock()) {
        pmm::free_frame(p);
    }
    drop(core::mem::take(&mut *cell.mappings.lock()));

    // Recién ahora (recursos liberados) se avisa a quienes esperan.
    let waiters = {
        let mut g = cell.exit.lock();
        g.0 = Some(status);
        core::mem::take(&mut g.1)
    };
    for t in waiters {
        sched::wake(t);
    }
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
        // Solo la mitad baja: demostrado con Kani en `nsos_kcore::paging`.
        if !nsos_kcore::paging::user_range_ok(vaddr, obj.size(), elf::USER_MIN, elf::USER_MAX) {
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
        self.mappings.lock().push(Mapping {
            obj,
            vaddr,
            write,
            exec,
        });
        Ok(())
    }

    /// `mem_unmap(vaddr, len)`: quita el mapeo de un objeto de memoria que
    /// empieza en `vaddr` y mide exactamente `len` (elegido por
    /// `nsos_kcore::paging::unmap_target`, verificado con Kani). Primero las
    /// páginas de la tabla y recién después suelta la referencia al objeto,
    /// fuera de los locks: los marcos no se liberan mientras estén mapeados.
    /// Modelo: `Mem.unmap_removes`, `Mem.unmap_only_shrinks`.
    pub fn unmap_memory(&self, vaddr: u64, len: u64) -> Result<(), Error> {
        let m = {
            let mut maps = self.mappings.lock();
            let spans: Vec<(u64, u64)> = maps.iter().map(|m| (m.vaddr, m.obj.size())).collect();
            let i =
                nsos_kcore::paging::unmap_target(&spans, vaddr, len).ok_or(Error::InvalidArgs)?;
            maps.swap_remove(i)
        };
        let mut g = self.space.lock();
        if let Some(space) = g.as_mut() {
            for i in 0..m.obj.pages as u64 {
                space.unmap(m.vaddr + i * PAGE_SIZE);
            }
        }
        drop(g);
        drop(m);
        Ok(())
    }

    /// Revocación: quita los mapeos de objetos de memoria para los que la
    /// cell ya no tiene una capacidad con los derechos del mapeo (porque la
    /// envió, la pasó a una cell hija o la soltó). Se llama después de toda
    /// operación que saca capacidades de la CSpace de la cell, desde la
    /// propia cell (su espacio está activo, así que `invlpg` alcanza).
    ///
    /// Modelo y demostración: `verification/lean/Nsos/Memory.lean`.
    pub fn revoke_mappings(&self) {
        let stale: Vec<Mapping> = {
            let mut maps = self.mappings.lock();
            if maps.is_empty() {
                return;
            }
            let cs = self.cspace.lock();
            let (keep, stale) = core::mem::take(&mut *maps)
                .into_iter()
                .partition(|m| cs.caps().any(|c| m.allowed_by(c)));
            *maps = keep;
            stale
        };
        if stale.is_empty() {
            return;
        }
        let mut g = self.space.lock();
        if let Some(space) = g.as_mut() {
            for m in &stale {
                for i in 0..m.obj.pages as u64 {
                    space.unmap(m.vaddr + i * PAGE_SIZE);
                }
            }
        }
        drop(g);
        // Soltar los objetos fuera de los locks (puede liberar marcos).
        drop(stale);
    }
}

/// Un objeto de memoria mapeado en una cell.
struct Mapping {
    obj: Arc<MemObj>,
    vaddr: u64,
    write: bool,
    exec: bool,
}

impl Mapping {
    /// `c` es una capacidad a este objeto con los derechos del mapeo.
    fn allowed_by(&self, c: &Cap<KObject>) -> bool {
        let mut need = Rights::READ;
        if self.write {
            need = need | Rights::WRITE;
        }
        if self.exec {
            need = need | Rights::EXEC;
        }
        matches!(&c.object, KObject::Memory(m) if Arc::ptr_eq(m, &self.obj))
            && c.rights.contains(need)
    }
}
