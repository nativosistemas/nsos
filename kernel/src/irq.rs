//! Líneas de interrupción entregadas a drivers en user-space (objeto `Irq`).
//!
//! Sigue el modelo `verification/lean/Nsos/Irq.lean`:
//!
//! * `claim` reclama la línea (a lo sumo un dueño, `claim_exclusive`) y la
//!   programa habilitada en el controlador (IOAPIC o GIC).
//! * `on_irq` (paso `deliver`), desde la ruta de interrupción: enmascara la
//!   línea, marca `IRQ_PENDING` y despierta a quienes esperan con `wait`. El
//!   kernel no toca el dispositivo.
//! * `IrqLine::ack` (`irq_ack`, paso `ack`): borra `IRQ_PENDING` y
//!   desenmascara, en un solo paso bajo el lock del objeto.
//! * `Drop` (paso `release`): enmascara y suelta la línea.
//!
//! Invariante (`IrqModel.Inv`): con dueño, la línea está enmascarada
//! exactamente cuando hay `IRQ_PENDING`; sin dueño, enmascarada.

use crate::sched::{self, ThreadId};
use crate::sync::IrqSpinLock;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use nsos_kcore::irq::IrqClaims;

/// Interrupciones entregadas a líneas con dueño (lo consultan los tests).
static DELIVERED: AtomicU64 = AtomicU64::new(0);

/// Interrupciones entregadas a drivers desde el arranque.
pub fn delivered() -> u64 {
    DELIVERED.load(Ordering::Relaxed)
}

struct State {
    /// `IRQ_PENDING`.
    pending: bool,
    /// Hilos esperando `IRQ_PENDING` con `wait`.
    waiters: Vec<ThreadId>,
}

/// Una línea de interrupción con dueño.
pub struct IrqLine {
    /// GSI (pin del IOAPIC) en x86_64, INTID del GIC en ARM64.
    pub line: u32,
    state: IrqSpinLock<State>,
}

static CLAIMS: IrqSpinLock<IrqClaims> = IrqSpinLock::new(IrqClaims::new());
/// Objetos vivos, para encontrarlos desde la interrupción.
static LINES: IrqSpinLock<Vec<(u32, Weak<IrqLine>)>> = IrqSpinLock::new(Vec::new());

/// Reclama `line` para un driver. `None` si el controlador no la entrega o
/// si ya tiene dueño (por ejemplo, dos dispositivos que comparten línea: el
/// segundo driver sigue sin interrupciones).
pub fn claim(line: u32) -> Option<Arc<IrqLine>> {
    if !crate::arch::irq::valid(line) || !CLAIMS.lock().claim(line) {
        return None;
    }
    let obj = Arc::new(IrqLine {
        line,
        state: IrqSpinLock::new(State {
            pending: false,
            waiters: Vec::new(),
        }),
    });
    LINES.lock().push((line, Arc::downgrade(&obj)));
    crate::arch::irq::route(line);
    Some(obj)
}

/// Llegó la interrupción de `line` (con las interrupciones apagadas).
pub fn on_irq(line: u32) {
    crate::arch::irq::mask(line);
    let obj = LINES
        .lock()
        .iter()
        .find(|(l, _)| *l == line)
        .and_then(|(_, w)| w.upgrade());
    let Some(obj) = obj else {
        // Sin dueño: queda enmascarada (`unowned_silent`).
        return;
    };
    let waiters = {
        let mut st = obj.state.lock();
        st.pending = true;
        DELIVERED.fetch_add(1, Ordering::Relaxed);
        core::mem::take(&mut st.waiters)
    };
    for t in waiters {
        sched::wake(t);
    }
}

impl IrqLine {
    /// `irq_ack`: borra `IRQ_PENDING` y desenmascara. Si el dispositivo
    /// sigue con la línea activa, la interrupción vuelve a llegar.
    pub fn ack(&self) {
        let mut st = self.state.lock();
        st.pending = false;
        crate::arch::irq::unmask(self.line);
    }

    /// Paso `scan` de `wait`: ¿hay `IRQ_PENDING`? Si no y `register`, deja
    /// anotado al hilo actual (lo despierta `on_irq`).
    pub fn watch(&self, register: bool) -> bool {
        let mut st = self.state.lock();
        if !st.pending && register {
            let me = sched::current();
            if !st.waiters.contains(&me) {
                st.waiters.push(me);
            }
        }
        st.pending
    }
}

impl Drop for IrqLine {
    fn drop(&mut self) {
        crate::arch::irq::mask(self.line);
        LINES
            .lock()
            .retain(|(l, w)| *l != self.line && w.strong_count() > 0);
        CLAIMS.lock().release(self.line);
    }
}
