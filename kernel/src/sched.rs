//! Scheduler round-robin apropiativo de hilos del kernel.
//!
//! Un cambio de contexto es simplemente devolver a la ruta de interrupción un
//! `TrapFrame` distinto del que se guardó (ver `arch::idt`). Cada hilo tiene
//! su pila de kernel; el hilo de arranque (`kmain`) se convierte en el hilo
//! *idle*, que solo corre cuando la cola está vacía.
//!
//! Un hilo puede pertenecer a una cell: entonces corre en anillo 3 con el
//! espacio de direcciones de la cell, y al cambiar a él se cargan su CR3 y
//! su pila de kernel (TSS y `syscall`).

use crate::arch::TrapFrame;
use crate::cell::Cell;
use crate::sync::IrqSpinLock;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::mem::size_of;

/// Identificador de hilo.
pub type ThreadId = u64;

const STACK_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    /// Esperando un evento (`wake`).
    Blocked,
    Dead,
}

struct Thread {
    id: ThreadId,
    #[allow(dead_code)]
    name: &'static str,
    /// `None` para el hilo de arranque (usa la pila de Limine).
    _stack: Option<Box<[u8]>>,
    frame: *mut TrapFrame,
    state: State,
    /// Cell dueña (None = hilo de kernel).
    cell: Option<Arc<Cell>>,
    /// Tope de la pila de kernel (para entradas desde anillo 3).
    kstack_top: u64,
}

// SAFETY: `frame` apunta dentro de la pila propia del hilo; solo el scheduler
// (bajo su lock) lo lee o escribe.
unsafe impl Send for Thread {}

struct Scheduler {
    threads: Vec<Thread>,
    ready: VecDeque<ThreadId>,
    current: ThreadId,
    idle: ThreadId,
    /// Hilo muerto cuya pila aún no se puede liberar (podríamos estar en ella).
    zombie: Option<Thread>,
    next_id: ThreadId,
    switches: u64,
}

impl Scheduler {
    fn find(&mut self, id: ThreadId) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.id == id)
    }
}

static SCHED: IrqSpinLock<Option<Scheduler>> = IrqSpinLock::new(None);

/// Inicializa el scheduler: el contexto actual pasa a ser el hilo idle.
pub fn init() {
    let idle = Thread {
        id: 0,
        name: "idle",
        _stack: None,
        frame: core::ptr::null_mut(),
        state: State::Running,
        cell: None,
        kstack_top: 0,
    };
    *SCHED.lock() = Some(Scheduler {
        threads: vec![idle],
        ready: VecDeque::new(),
        current: 0,
        idle: 0,
        zombie: None,
        next_id: 1,
        switches: 0,
    });
}

extern "C" fn thread_trampoline(closure: *mut Box<dyn FnOnce() + Send>) -> ! {
    // SAFETY: `spawn` pasó un `Box::into_raw` de exactamente este tipo y es
    // la única vez que se reconstruye.
    let f = unsafe { Box::from_raw(closure) };
    f();
    exit()
}

/// Crea un hilo de kernel que ejecuta `f`.
pub fn spawn(name: &'static str, f: impl FnOnce() + Send + 'static) -> ThreadId {
    let closure: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(f));
    let init = TrapFrame::kernel_thread(
        thread_trampoline as *const () as u64,
        Box::into_raw(closure) as u64,
    );
    add_thread(name, init, None)
}

/// Crea el hilo principal de una cell: empieza en `entry` (modo usuario)
/// con la pila de usuario que termina en `user_stack_top`.
pub fn spawn_user(
    name: &'static str,
    cell: Arc<Cell>,
    entry: u64,
    user_stack_top: u64,
) -> ThreadId {
    let init = TrapFrame::user_thread(entry, user_stack_top);
    add_thread(name, init, Some(cell))
}

fn add_thread(name: &'static str, mut init: TrapFrame, cell: Option<Arc<Cell>>) -> ThreadId {
    let mut stack = vec![0u8; STACK_SIZE].into_boxed_slice();
    let top = (stack.as_mut_ptr() as u64 + STACK_SIZE as u64) & !0xF;
    let frame = crate::arch::initial_frame(top, &mut init);
    debug_assert!(
        frame as u64 >= stack.as_ptr() as u64
            && frame as u64 + size_of::<TrapFrame>() as u64 <= top
    );
    // SAFETY: `frame` está dentro de `stack`, alineado a 8 y sin alias.
    unsafe { frame.write(init) };
    let mut g = SCHED.lock();
    let s = g.as_mut().expect("scheduler sin inicializar");
    let id = s.next_id;
    s.next_id += 1;
    s.threads.push(Thread {
        id,
        name,
        _stack: Some(stack),
        frame,
        state: State::Ready,
        cell,
        kstack_top: top,
    });
    s.ready.push_back(id);
    id
}

/// Termina el hilo actual.
pub fn exit() -> ! {
    {
        let mut g = SCHED.lock();
        let s = g.as_mut().expect("scheduler sin inicializar");
        let cur = s.current;
        assert_ne!(cur, s.idle, "el hilo idle no puede terminar");
        s.find(cur).unwrap().state = State::Dead;
    }
    crate::arch::yield_now();
    unreachable!("un hilo muerto volvió a ejecutarse");
}

/// Elige el próximo hilo. Se llama desde la ruta de interrupción con
/// interrupciones deshabilitadas.
pub fn schedule(frame: *mut TrapFrame) -> *mut TrapFrame {
    let mut g = SCHED.lock();
    let Some(s) = g.as_mut() else { return frame };

    // Ya estamos en la pila del hilo actual: la del zombie anterior se puede
    // liberar sin riesgo.
    s.zombie = None;

    let cur_id = s.current;
    let idle = s.idle;
    let cur_state = {
        let cur = s.find(cur_id).expect("hilo actual inexistente");
        cur.frame = frame;
        if cur.state == State::Running {
            cur.state = State::Ready;
        }
        cur.state
    };
    // Bloqueado o muerto: no vuelve a la cola.
    if cur_state == State::Ready && cur_id != idle {
        s.ready.push_back(cur_id);
    }

    let next_id = s.ready.pop_front().unwrap_or(idle);
    if cur_state == State::Dead {
        let pos = s.threads.iter().position(|t| t.id == cur_id).unwrap();
        s.zombie = Some(s.threads.swap_remove(pos));
    }
    if next_id != cur_id {
        s.switches += 1;
    }
    s.current = next_id;
    let next = s.find(next_id).expect("hilo listo inexistente");
    next.state = State::Running;
    let root = match &next.cell {
        Some(c) => {
            crate::arch::set_kernel_stack(next.kstack_top);
            c.root()
        }
        None => crate::mm::kernel_root(),
    };
    // SAFETY: `root` es el espacio del kernel o el de una cell viva (una
    // cell que termina pone su raíz en 0 antes de liberarla).
    unsafe { crate::arch::paging::switch_to(root) };
    next.frame
}

/// Marca el hilo actual como bloqueado. No cambia de contexto: el llamador
/// suelta sus locks y luego llama a `arch::yield_now()`. Si entre medio llega
/// un `wake`, el hilo simplemente sigue corriendo (no se pierde el aviso).
pub fn block_current() {
    let mut g = SCHED.lock();
    let s = g.as_mut().expect("scheduler sin inicializar");
    let cur = s.current;
    s.find(cur).unwrap().state = State::Blocked;
}

/// Despierta un hilo bloqueado (no hace nada si no lo está o ya no existe).
pub fn wake(id: ThreadId) {
    let mut g = SCHED.lock();
    let Some(s) = g.as_mut() else { return };
    let current = s.current;
    if let Some(t) = s.find(id) {
        if t.state == State::Blocked {
            if id == current {
                t.state = State::Running;
            } else {
                t.state = State::Ready;
                s.ready.push_back(id);
            }
        }
    }
}

/// Cell del hilo actual.
pub fn current_cell() -> Option<Arc<Cell>> {
    let mut g = SCHED.lock();
    let s = g.as_mut()?;
    let cur = s.current;
    s.find(cur).and_then(|t| t.cell.clone())
}

/// Desde la ruta de interrupción: termina el hilo actual y elige otro.
pub fn exit_from_trap(frame: *mut TrapFrame) -> *mut TrapFrame {
    {
        let mut g = SCHED.lock();
        let s = g.as_mut().expect("scheduler sin inicializar");
        let cur = s.current;
        s.find(cur).unwrap().state = State::Dead;
    }
    schedule(frame)
}

/// Hilos vivos sin contar el idle.
pub fn live_threads() -> usize {
    let g = SCHED.lock();
    g.as_ref().map(|s| s.threads.len() - 1).unwrap_or(0)
}

/// Cambios de contexto realizados.
pub fn switches() -> u64 {
    SCHED.lock().as_ref().map(|s| s.switches).unwrap_or(0)
}

/// Id del hilo actual.
pub fn current() -> ThreadId {
    SCHED.lock().as_ref().map(|s| s.current).unwrap_or(0)
}

/// Desde el hilo idle: espera (con `hlt`) a que no quede ningún otro hilo.
pub fn wait_all() {
    while live_threads() > 0 {
        crate::arch::cpu::wait_for_interrupt();
    }
}
