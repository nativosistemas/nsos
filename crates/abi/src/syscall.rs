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
    /// `mem_info(h, out: *mut MemInfo)`: tamaño y, si la capacidad tiene
    /// `CONTROL`, la dirección física (solo para DMA de drivers).
    MemInfo = 43,

    // ---- Cells (5x) ----
    /// `cell_spawn(h_image, caps: *const CapHandle, n) -> h_cell`: crea una
    /// cell a partir de una imagen (requiere `EXEC`) y le *mueve* las
    /// capacidades dadas (cada una requiere `GRANT`), que recibe en los
    /// slots `1..=n`. Devuelve una capacidad a la cell nueva.
    CellSpawn = 50,
    /// `cell_kill(h_cell)`: termina la cell (requiere `CONTROL`). No espera:
    /// la terminación se observa con `cell_status`.
    CellKill = 51,
    /// `cell_status(h_cell, out: *mut CellStatus, flags)`: espera a que la
    /// cell termine y escribe cómo terminó. Con `NONBLOCK` no espera.
    CellStatus = 52,

    // ---- Hardware para drivers (6x) ----
    /// `irq_ack(h_irq)`: rearma la línea de interrupción.
    IrqAck = 60,
    /// `ioport_read(h_ports, off, width) -> valor` (`off` relativo al rango).
    IoPortRead = 61,
    /// `ioport_write(h_ports, off, width, valor)` (`off` relativo al rango).
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
            43 => MemInfo,
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

/// Salida de `mem_info`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct MemInfo {
    /// Tamaño en bytes (múltiplo de 4096).
    pub size: u64,
    /// Dirección física del primer byte (contiguo), o 0 sin `CONTROL`.
    pub phys: u64,
}

/// Estado de una cell (`CellStatus::state`).
pub mod cell_state {
    /// Sigue corriendo.
    pub const RUNNING: u32 = 0;
    /// Llamó a `exit(code)`.
    pub const EXITED: u32 = 1;
    /// El kernel la terminó por una excepción de CPU.
    pub const FAULTED: u32 = 2;
    /// La terminó `cell_kill`.
    pub const KILLED: u32 = 3;
}

/// Salida de `cell_status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct CellStatus {
    /// Ver [`cell_state`].
    pub state: u32,
    /// Código de salida (`EXITED`).
    pub code: i32,
    /// Vector de la excepción (`FAULTED`).
    pub vector: u64,
    /// Dirección que falló, si aplica (`FAULTED` por #PF).
    pub addr: u64,
}

impl CellStatus {
    /// Tamaño serializado.
    pub const SIZE: usize = 24;

    /// Serializa en little-endian.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.state.to_le_bytes());
        b[4..8].copy_from_slice(&self.code.to_le_bytes());
        b[8..16].copy_from_slice(&self.vector.to_le_bytes());
        b[16..24].copy_from_slice(&self.addr.to_le_bytes());
        b
    }

    /// Deserializa; `None` si el estado es desconocido.
    pub fn from_bytes(b: &[u8; Self::SIZE]) -> Option<CellStatus> {
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        let s = CellStatus {
            state: u32_at(0),
            code: u32_at(4) as i32,
            vector: u64_at(8),
            addr: u64_at(16),
        };
        (s.state <= cell_state::KILLED).then_some(s)
    }
}

/// Flags de `channel_send` / `channel_recv` / `cell_status`.
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
    fn cell_status_roundtrip() {
        let s = CellStatus {
            state: cell_state::FAULTED,
            code: -1,
            vector: 14,
            addr: 0xdead_b000,
        };
        assert_eq!(CellStatus::from_bytes(&s.to_bytes()), Some(s));
        let mut bad = s.to_bytes();
        bad[0] = 9;
        assert_eq!(CellStatus::from_bytes(&bad), None);
    }

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
