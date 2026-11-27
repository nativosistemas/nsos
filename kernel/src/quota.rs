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
//! Orden de los locks: una cuenta antes que las de sus supervisores. Nunca
//! se toma la de una hija con la de su supervisor tomada.

use crate::sync::IrqSpinLock;
use alloc::sync::Arc;
use nsos_abi::Error;
use nsos_kcore::quota::Quota;

/// Marcos que el kernel se guarda para sí: lo que reserva directamente del
/// allocador después de arrancar (tablas del kernel, `map_mmio`, ktest).
pub const KERNEL_RESERVE: u64 = 2048;

/// Cuota de las cells que crea el kernel al arrancar (8 MiB).
pub const BOOT_QUOTA: u64 = 2048;

/// Cuenta de memoria de una cell.
pub struct Account {
    state: IrqSpinLock<State>,
    /// Supervisor: quien recibe la cuenta cuando la cell muere.
    parent: Option<Arc<Account>>,
}

struct State {
    quota: Quota,
    /// La cell murió: sus marcos se cuentan en el supervisor.
    dead: bool,
}

static ROOT: IrqSpinLock<Option<Arc<Account>>> = IrqSpinLock::new(None);

/// Crea la cuenta raíz con `free` marcos libres (menos la reserva del
/// kernel). Se llama una vez, después de iniciar el allocador de marcos.
pub fn init(free: u64) {
    let root = Account::new(Quota::new(free.saturating_sub(KERNEL_RESERVE)), None);
    *ROOT.lock() = Some(Arc::new(root));
}

/// La cuenta raíz (del kernel).
pub fn root() -> Arc<Account> {
    ROOT.lock().clone().expect("cuotas sin inicializar")
}

impl Account {
    fn new(quota: Quota, parent: Option<Arc<Account>>) -> Self {
        Self {
            state: IrqSpinLock::new(State { quota, dead: false }),
            parent,
        }
    }

    /// Cobra `n` marcos. `NoMemory` si se pasan de la cuota disponible.
    pub fn charge(&self, n: u64) -> Result<(), Error> {
        let mut g = self.state.lock();
        if g.dead || !g.quota.charge(n) {
            return Err(Error::NoMemory);
        }
        Ok(())
    }

    /// Devuelve `n` marcos liberados. Si la cell murió, van a la cuenta que
    /// heredó sus usados (su supervisor, o el primero vivo hacia arriba).
    pub fn refund(&self, n: u64) {
        let mut g = self.state.lock();
        if g.dead {
            let parent = self.parent.as_deref().expect("la cuenta raíz no muere");
            // Con este lock tomado: así `close` no puede estar a mitad de
            // pasarle los usados al supervisor.
            parent.refund(n);
            drop(g);
            return;
        }
        let ok = g.quota.refund(n);
        debug_assert!(ok, "se devolvieron marcos que no estaban cobrados");
    }

    /// Separa `q` marcos de la cuota disponible para una cell hija.
    pub fn split(self: &Arc<Self>, q: u64) -> Result<Arc<Account>, Error> {
        let child = {
            let mut g = self.state.lock();
            if g.dead {
                return Err(Error::NoMemory);
            }
            g.quota.split(q).ok_or(Error::NoMemory)?
        };
        Ok(Arc::new(Account::new(child, Some(self.clone()))))
    }

    /// La cell murió: lo disponible vuelve al supervisor y lo usado pasa a
    /// ser suyo (paso `kill` del modelo). Si el supervisor también murió,
    /// sube hasta el primero vivo. Idempotente.
    pub fn close(&self) {
        let mut g = self.state.lock();
        if g.dead {
            return;
        }
        let parent = self.parent.as_deref().expect("la cuenta raíz no muere");
        parent.absorb(g.quota);
        g.quota = Quota::default();
        g.dead = true;
    }

    fn absorb(&self, q: Quota) {
        let g = self.state.lock();
        if g.dead {
            let parent = self.parent.as_deref().expect("la cuenta raíz no muere");
            parent.absorb(q);
            return;
        }
        let mut g = g;
        let ok = g.quota.absorb(q);
        assert!(ok, "cuenta de memoria fuera de rango");
    }

    /// Cuota disponible (para `cell_status` y los tests).
    pub fn avail(&self) -> u64 {
        self.state.lock().quota.avail
    }

    /// Marcos en uso.
    pub fn used(&self) -> u64 {
        self.state.lock().quota.used
    }
}
