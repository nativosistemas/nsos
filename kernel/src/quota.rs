//! Cuotas de memoria por supervisor.
//!
//! Cada cell tiene una [`Account`]. Todo marco que el kernel reserva para
//! una cell (su imagen, su pila, sus tablas de páginas, los objetos
//! `Memory` que crea) se cobra a su cuenta, y se le devuelve al liberarse.
//! `cell_spawn` separa la cuota de la hija de la cuota disponible de quien
//! la crea. Al morir, lo que le quedaba disponible vuelve a su supervisor, y
//! lo que todavía usa (un objeto `Memory` que mandó a otra cell) pasa a
//! contarse como usado del supervisor hasta que se libere.
//!
//! Diseño demostrado en `verification/lean/Nsos/Quota.lean` (los marcos
//! libres son la suma de las cuotas disponibles; una cell con cuota siempre
//! consigue memoria, haga lo que haga el resto: `QuotaModel.no_starvation`).
//! La aritmética de cada paso es `nsos_kcore::quota`, verificada con Kani
//! contra los pasos del modelo.
//!
//! La cuenta raíz es del kernel: arranca con los marcos libres menos
//! [`KERNEL_RESERVE`], y de ella salen las cuotas de las cells que crea el
//! kernel y los objetos `Memory` que reparte (búferes de DMA).
//!
//! Hay dos recursos, cada uno con su [`Quota`] y las mismas reglas: los
//! **marcos** de las cells y la **memoria del kernel** (`kmem`, bytes del
//! heap) que el kernel gasta por ellas: su pila de kernel, su CSpace, las
//! colas de mensajes, los canales, los mapeos. Cada marco de cuota trae
//! [`KMEM_PER_FRAME`] bytes de `kmem`, así que `cell_spawn` reparte los dos
//! en la misma proporción y el modelo de Lean vale para cada uno por
//! separado (los mismos pasos sobre el mismo árbol de supervisores).
//!
//! La cuenta lleva además la **reserva de CPU** de la cell: unidades de
//! [`CPU_UNITS`] por período (un segundo) que el scheduler le garantiza.
//! También se reparte con `cell_spawn` y vuelve al morir (la misma regla que
//! los otros dos recursos), y el scheduler le da la CPU primero a quien
//! todavía tiene reserva en el período (`verification/lean/Nsos/Cpu.lean`:
//! si las reservas suman como mucho el período, cada cell recibe la suya).
//!
//! Orden de los locks: una cuenta antes que las de sus supervisores. Nunca
//! se toma la de una hija con la de su supervisor tomada. El lock de un
//! canal se puede tener tomado al cobrar (los mensajes se cobran al
//! encolarse); nunca al revés.

use crate::sync::IrqSpinLock;
use alloc::sync::Arc;
use nsos_abi::Error;
use nsos_kcore::quota::Quota;

/// Marcos que el kernel se guarda para sí: lo que reserva directamente del
/// allocador después de arrancar (tablas del kernel, `map_mmio`, ktest).
pub const KERNEL_RESERVE: u64 = 2048;

/// Bytes del heap que el kernel se guarda para sí (sus hilos, la PCI, los
/// tests y lo que se libera un poco después de cobrarse, como la pila de
/// kernel de una cell que terminó).
pub const KERNEL_KMEM_RESERVE: u64 = 3 << 20;

/// Bytes de memoria del kernel que trae cada marco de cuota.
pub const KMEM_PER_FRAME: u64 = 512;

/// Cuota de las cells que crea el kernel al arrancar (8 MiB).
pub const BOOT_QUOTA: u64 = 2048;

pub use nsos_abi::syscall::CPU_UNITS;

/// Unidades de CPU por período que el kernel se guarda para sus hilos.
pub const KERNEL_CPU_RESERVE: u64 = 10;

/// Duración del período del scheduler.
pub const PERIOD_NS: u64 = 1_000_000_000;

/// Tiempo de una unidad de reserva de CPU.
pub const CPU_UNIT_NS: u64 = PERIOD_NS / CPU_UNITS;

/// Recurso de una cuenta.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Res {
    /// Marcos físicos de la cell.
    Frames,
    /// Bytes del heap del kernel gastados por la cell.
    Kmem,
}

/// Cuenta de memoria de una cell.
pub struct Account {
    state: IrqSpinLock<State>,
    /// Supervisor: quien recibe la cuenta cuando la cell muere.
    parent: Option<Arc<Account>>,
}

struct State {
    frames: Quota,
    kmem: Quota,
    /// Reserva de CPU en `avail` (unidades por período; `used` siempre 0).
    cpu: Quota,
    /// Tiempo de reserva que le queda en el período `cpu_period`.
    cpu_left_ns: u64,
    cpu_period: u64,
    /// La cell murió: lo suyo se cuenta en el supervisor.
    dead: bool,
}

impl State {
    fn get(&mut self, r: Res) -> &mut Quota {
        match r {
            Res::Frames => &mut self.frames,
            Res::Kmem => &mut self.kmem,
        }
    }
}

static ROOT: IrqSpinLock<Option<Arc<Account>>> = IrqSpinLock::new(None);

/// Crea la cuenta raíz con `free` marcos libres y `heap` bytes de heap
/// (menos las reservas del kernel). Se llama una vez, después de iniciar el
/// allocador de marcos y el heap.
pub fn init(free: u64, heap: u64) {
    let root = Account::new(
        Quota::new(free.saturating_sub(KERNEL_RESERVE)),
        Quota::new(heap.saturating_sub(KERNEL_KMEM_RESERVE)),
        Quota::new(CPU_UNITS - KERNEL_CPU_RESERVE),
        None,
    );
    *ROOT.lock() = Some(Arc::new(root));
}

/// La cuenta raíz (del kernel).
pub fn root() -> Arc<Account> {
    ROOT.lock().clone().expect("cuotas sin inicializar")
}

impl Account {
    fn new(frames: Quota, kmem: Quota, cpu: Quota, parent: Option<Arc<Account>>) -> Self {
        Self {
            state: IrqSpinLock::new(State {
                frames,
                kmem,
                cpu,
                cpu_left_ns: 0,
                cpu_period: u64::MAX,
                dead: false,
            }),
            parent,
        }
    }

    /// Cobra `n` marcos. `NoMemory` si se pasan de la cuota disponible.
    pub fn charge(&self, n: u64) -> Result<(), Error> {
        self.charge_res(Res::Frames, n)
    }

    /// Devuelve `n` marcos liberados.
    pub fn refund(&self, n: u64) {
        self.refund_res(Res::Frames, n)
    }

    /// Cobra `n` bytes de memoria del kernel.
    pub fn charge_kmem(&self, n: u64) -> Result<(), Error> {
        self.charge_res(Res::Kmem, n)
    }

    /// Devuelve `n` bytes de memoria del kernel.
    pub fn refund_kmem(&self, n: u64) {
        self.refund_res(Res::Kmem, n)
    }

    fn charge_res(&self, r: Res, n: u64) -> Result<(), Error> {
        let mut g = self.state.lock();
        if g.dead || !g.get(r).charge(n) {
            return Err(Error::NoMemory);
        }
        Ok(())
    }

    /// Si la cell murió, lo devuelto va a la cuenta que heredó lo suyo (su
    /// supervisor, o el primero vivo hacia arriba).
    fn refund_res(&self, r: Res, n: u64) {
        let mut g = self.state.lock();
        if g.dead {
            let parent = self.parent.as_deref().expect("la cuenta raíz no muere");
            // Con este lock tomado: así `close` no puede estar a mitad de
            // pasarle lo suyo al supervisor.
            parent.refund_res(r, n);
            drop(g);
            return;
        }
        let ok = g.get(r).refund(n);
        debug_assert!(ok, "se devolvió memoria que no estaba cobrada");
    }

    /// Separa `q` marcos de la cuota disponible para una cell hija, con
    /// `q * KMEM_PER_FRAME` bytes de memoria del kernel y `cpu` unidades de
    /// reserva de CPU. Todo o nada.
    pub fn split(self: &Arc<Self>, q: u64, cpu: u64) -> Result<Arc<Account>, Error> {
        let k = q.checked_mul(KMEM_PER_FRAME).ok_or(Error::NoMemory)?;
        let (frames, kmem, cpu) = {
            let mut g = self.state.lock();
            if g.dead || q > g.frames.avail || k > g.kmem.avail || cpu > g.cpu.avail {
                return Err(Error::NoMemory);
            }
            let frames = g.frames.split(q).expect("comprobado arriba");
            let kmem = g.kmem.split(k).expect("comprobado arriba");
            let cpu = g.cpu.split(cpu).expect("comprobado arriba");
            // Lo que le queda en este período no puede pasar de su reserva nueva.
            g.cpu_left_ns = g.cpu_left_ns.min(g.cpu.avail * CPU_UNIT_NS);
            (frames, kmem, cpu)
        };
        Ok(Arc::new(Account::new(
            frames,
            kmem,
            cpu,
            Some(self.clone()),
        )))
    }

    /// Renueva la reserva de CPU al empezar un período nuevo.
    fn refresh_cpu(g: &mut State, now_ns: u64) {
        let period = now_ns / PERIOD_NS;
        if g.cpu_period != period {
            g.cpu_period = period;
            g.cpu_left_ns = g.cpu.avail * CPU_UNIT_NS;
        }
    }

    /// ¿Le queda reserva de CPU en el período de `now_ns`? Una cell muerta,
    /// no.
    pub fn cpu_in_reserve(&self, now_ns: u64) -> bool {
        let mut g = self.state.lock();
        if g.dead {
            return false;
        }
        Self::refresh_cpu(&mut g, now_ns);
        g.cpu_left_ns > 0
    }

    /// Le cobra `ns` de CPU (lo que corrió hasta `now_ns`) a la reserva del
    /// período.
    pub fn cpu_charge(&self, ns: u64, now_ns: u64) {
        let mut g = self.state.lock();
        Self::refresh_cpu(&mut g, now_ns);
        g.cpu_left_ns = g.cpu_left_ns.saturating_sub(ns);
    }

    /// Reserva de CPU (unidades por período).
    pub fn cpu_reserve(&self) -> u64 {
        self.state.lock().cpu.avail
    }

    /// La cell murió: lo disponible vuelve al supervisor y lo usado pasa a
    /// ser suyo (paso `kill` del modelo, para cada recurso). Si el supervisor
    /// también murió, sube hasta el primero vivo. Idempotente.
    pub fn close(&self) {
        let mut g = self.state.lock();
        if g.dead {
            return;
        }
        let parent = self.parent.as_deref().expect("la cuenta raíz no muere");
        parent.absorb(g.frames, g.kmem, g.cpu);
        g.frames = Quota::default();
        g.kmem = Quota::default();
        g.cpu = Quota::default();
        g.cpu_left_ns = 0;
        g.dead = true;
    }

    fn absorb(&self, frames: Quota, kmem: Quota, cpu: Quota) {
        let mut g = self.state.lock();
        if g.dead {
            let parent = self.parent.as_deref().expect("la cuenta raíz no muere");
            parent.absorb(frames, kmem, cpu);
            return;
        }
        let ok = g.frames.absorb(frames) && g.kmem.absorb(kmem) && g.cpu.absorb(cpu);
        assert!(ok, "cuenta fuera de rango");
    }

    /// Cuota disponible de marcos.
    pub fn avail(&self) -> u64 {
        self.state.lock().frames.avail
    }

    /// Cuota disponible de memoria del kernel, en bytes.
    pub fn kmem_avail(&self) -> u64 {
        self.state.lock().kmem.avail
    }
}
