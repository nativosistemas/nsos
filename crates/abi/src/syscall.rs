//! Números de syscall y convención de llamada.
//!
//! Convención (x86_64): instrucción `syscall`; número en `rax`; argumentos en
//! `rdi, rsi, rdx, r10, r8, r9`; resultado en `rax` (ver [`crate::error`]).
//! El kernel preserva todos los registros salvo `rax`, `rcx` y `r11`.

/// Identificadores de syscall. Agrupados por decenas según el subsistema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Syscall {
    // ---- Básicas (0x) ----
    /// `debug_log(ptr, len)`: escribe texto en la consola de depuración.
    DebugLog = 0,
    /// `yield()`: cede la CPU.
    Yield = 1,
    /// `exit(code) -> !`: termina la cell actual.
    Exit = 2,
    /// `clock_monotonic() -> ns`.
    ClockMonotonic = 3,

    // ---- Capacidades (1x) ----
    /// `cap_dup(h, rights_mask) -> h2`: copia atenuada (requiere `DUPLICATE`).
    CapDup = 10,
    /// `cap_drop(h)`: libera el handle (el objeto muere con su última ref).
    CapDrop = 11,
    /// `cap_info(h, out: *mut CapInfo)`.
    CapInfo = 12,

    // ---- Canales IPC (2x) ----
    /// `channel_create(out: *mut [CapHandle; 2])`: par de extremos conectados.
    ChannelCreate = 20,
    /// `channel_send(h, msg: *const Message, flags)`.
    ChannelSend = 21,
    /// `channel_recv(h, msg: *mut Message, flags)`.
    ChannelRecv = 22,
    /// `channel_call(h, msg: *mut Message, timeout_ns)`: send + recv de la
    /// respuesta en el mismo canal (camino rápido para RPC síncrono).
    ChannelCall = 23,

    // ---- Espera (3x) ----
    /// `wait(items: *mut WaitItem, n, timeout_ns) -> n_ready`. Base del
    /// executor async de las cells.
    Wait = 30,

    // ---- Memoria (4x) ----
    /// `mem_create(size, flags) -> h`: objeto de memoria con marcos nuevos.
    MemCreate = 40,
    /// `mem_map(h, vaddr_hint, rights) -> vaddr`.
    MemMap = 41,
    /// `mem_unmap(vaddr, len)`.
    MemUnmap = 42,

    // ---- Cells (5x) ----
    /// `cell_spawn(image, caps: *const CapHandle, n, out_ctrl) -> h_cell`:
    /// crea una cell nueva a la que se *mueven* las capacidades dadas.
    CellSpawn = 50,
    /// `cell_kill(h_cell)`: termina la cell (requiere `CONTROL`).
    CellKill = 51,
    /// `cell_status(h_cell, out)`: estado y código de salida/fallo.
    CellStatus = 52,

    // ---- Hardware para drivers (6x) ----
    /// `irq_ack(h_irq)`: rearma la línea de interrupción.
    IrqAck = 60,
    /// `ioport_read(h_ports, port, width) -> valor`.
    IoPortRead = 61,
    /// `ioport_write(h_ports, port, width, valor)`.
    IoPortWrite = 62,
}

impl Syscall {
    /// Decodifica el número de `rax`. `None` → el kernel devuelve `NoSys`.
    pub const fn from_raw(n: u64) -> Option<Syscall> {
        use Syscall::*;
        Some(match n {
            0 => DebugLog,
            1 => Yield,
            2 => Exit,
            3 => ClockMonotonic,
            10 => CapDup,
            11 => CapDrop,
            12 => CapInfo,
            20 => ChannelCreate,
            21 => ChannelSend,
            22 => ChannelRecv,
            23 => ChannelCall,
            30 => Wait,
            40 => MemCreate,
            41 => MemMap,
            42 => MemUnmap,
            50 => CellSpawn,
            51 => CellKill,
            52 => CellStatus,
            60 => IrqAck,
            61 => IoPortRead,
            62 => IoPortWrite,
            _ => return None,
        })
    }
}

/// Flags de `channel_send` / `channel_recv`.
pub mod flags {
    /// No bloquear: devolver `WouldBlock` si la operación tendría que esperar.
    pub const NONBLOCK: u64 = 1 << 0;
}

/// Entrada de la syscall `wait`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct WaitItem {
    /// Handle a observar (requiere derecho `WAIT`).
    pub handle: u32,
    /// Señales que interesan (ver [`crate::cap::signals`]).
    pub wanted: u32,
    /// Salida: señales activas al volver.
    pub observed: u32,
    /// Reservado, debe ser 0.
    pub reserved: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_roundtrip() {
        for n in 0..100u64 {
            if let Some(s) = Syscall::from_raw(n) {
                assert_eq!(s as u64, n);
            }
        }
        assert_eq!(Syscall::from_raw(9999), None);
    }

    #[test]
    fn wait_item_layout() {
        assert_eq!(core::mem::size_of::<WaitItem>(), 16);
    }
}
