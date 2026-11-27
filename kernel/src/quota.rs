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
        None,
    );
    *ROOT.lock() = Some(Arc::new(root));
}

/// La cuenta raíz (del kernel).
pub fn root() -> Arc<Account> {
    ROOT.lock().clone().expect("cuotas sin inicializar")
}

impl Account {
    fn new(frames: Quota, kmem: Quota, parent: Option<Arc<Account>>) -> Self {
        Self {
            state: IrqSpinLock::new(State {
                frames,
                kmem,
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
    /// `q * KMEM_PER_FRAME` bytes de memoria del kernel. Las dos o ninguna.
    pub fn split(self: &Arc<Self>, q: u64) -> Result<Arc<Account>, Error> {
        let k = q.checked_mul(KMEM_PER_FRAME).ok_or(Error::NoMemory)?;
        let (frames, kmem) = {
            let mut g = self.state.lock();
            if g.dead || q > g.frames.avail || k > g.kmem.avail {
                return Err(Error::NoMemory);
            }
            let frames = g.frames.split(q).expect("comprobado arriba");
            let kmem = g.kmem.split(k).expect("comprobado arriba");
            (frames, kmem)
        };
        Ok(Arc::new(Account::new(frames, kmem, Some(self.clone()))))
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
        parent.absorb(g.frames, g.kmem);
        g.frames = Quota::default();
        g.kmem = Quota::default();
        g.dead = true;
    }

    fn absorb(&self, frames: Quota, kmem: Quota) {
        let mut g = self.state.lock();
        if g.dead {
            let parent = self.parent.as_deref().expect("la cuenta raíz no muere");
            parent.absorb(frames, kmem);
            return;
        }
        let ok = g.frames.absorb(frames) && g.kmem.absorb(kmem);
        assert!(ok, "cuenta de memoria fuera de rango");
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
