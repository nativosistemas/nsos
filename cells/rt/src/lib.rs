//! Runtime de las cells de nsos.
//!
//! Es el único código de usuario con `unsafe`: la instrucción `syscall` y el
//! punto de entrada. Las cells se escriben con `#![deny(unsafe_code)]` y usan
//! estas funciones seguras.
//!
//! Capacidades de arranque: el kernel coloca las capacidades iniciales de la
//! cell en los slots 1, 2, 3... con generación 0; [`bootstrap`] las nombra.

#![no_std]

pub use nsos_abi as abi;

use abi::cap::CapInfo;
use abi::syscall::Syscall;
use abi::{error::decode_ret, CapHandle, Error, Message, Rights, SysResult};
use core::arch::asm;
use core::fmt::{self, Write};

/// Syscall cruda con hasta 4 argumentos.
#[inline(always)]
fn syscall4(n: Syscall, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    raw_syscall(n as u64, a0, a1, a2, a3)
}

/// Capacidad de arranque número `i` (1-based).
pub fn bootstrap(i: u32) -> CapHandle {
    CapHandle::new(i, 0).unwrap_or(CapHandle::INVALID)
}

/// Escribe texto en la consola de depuración del kernel.
pub fn debug_log(s: &str) {
    syscall4(Syscall::DebugLog, s.as_ptr() as u64, s.len() as u64, 0, 0);
}

/// Cede la CPU.
pub fn yield_now() {
    syscall4(Syscall::Yield, 0, 0, 0, 0);
}

/// Termina la cell con un código.
pub fn exit(code: i32) -> ! {
    syscall4(Syscall::Exit, code as u32 as u64, 0, 0, 0);
    // El kernel nunca vuelve de `exit`; si lo hiciera, no seguimos.
    #[allow(clippy::empty_loop)]
    loop {}
}

/// Crea un canal y devuelve sus dos extremos.
pub fn channel_create() -> SysResult<(CapHandle, CapHandle)> {
    let mut out = [CapHandle::INVALID; 2];
    decode_ret(syscall4(
        Syscall::ChannelCreate,
        out.as_mut_ptr() as u64,
        0,
        0,
        0,
    ))?;
    Ok((out[0], out[1]))
}

/// Envía un mensaje. Las capacidades adjuntas se mueven al receptor.
pub fn channel_send(h: CapHandle, m: &Message, flags: u64) -> SysResult<()> {
    decode_ret(syscall4(
        Syscall::ChannelSend,
        h.0 as u64,
        m as *const _ as u64,
        flags,
        0,
    ))
    .map(|_| ())
}

/// Recibe un mensaje (bloquea salvo `NONBLOCK`).
pub fn channel_recv(h: CapHandle, m: &mut Message, flags: u64) -> SysResult<()> {
    decode_ret(syscall4(
        Syscall::ChannelRecv,
        h.0 as u64,
        m as *mut _ as u64,
        flags,
        0,
    ))
    .map(|_| ())
}

/// Espera a que algún ítem tenga activa una de sus señales pedidas
/// (`wanted`) y devuelve cuántos lo están; en `observed` quedan las señales
/// activas de cada uno. `timeout_ns` es relativo: `0` solo mira y
/// [`abi::syscall::WAIT_FOREVER`] no vence. Si vence: `TimedOut`.
pub fn wait(items: &mut [abi::syscall::WaitItem], timeout_ns: u64) -> SysResult<usize> {
    decode_ret(syscall4(
        Syscall::Wait,
        items.as_mut_ptr() as u64,
        items.len() as u64,
        timeout_ns,
        0,
    ))
    .map(|n| n as usize)
}

/// Un ítem de [`wait`] para `h` con las señales `wanted`.
pub fn wait_item(h: CapHandle, wanted: u32) -> abi::syscall::WaitItem {
    abi::syscall::WaitItem {
        handle: h.0,
        wanted,
        observed: 0,
        reserved: 0,
    }
}

/// `irq_ack`: el driver ya atendió el dispositivo; borra `IRQ_PENDING` y
/// desenmascara la línea (requiere `CONTROL`).
pub fn irq_ack(h: CapHandle) -> SysResult<()> {
    decode_ret(syscall4(Syscall::IrqAck, h.0 as u64, 0, 0, 0)).map(|_| ())
}

/// Interrupción del dispositivo de un driver: la capacidad `Irq` que el
/// kernel entrega en un slot de arranque, o nada si no pudo entregar la
/// línea (compartida con otro dispositivo); entonces el driver sondea.
///
/// Uso: después de mirar el anillo "used" sin encontrar lo que espera, el
/// driver llama a [`DevIrq::wait`]; si devuelve `true`, lee el registro ISR
/// del dispositivo (eso apaga la línea) y llama a [`DevIrq::ack`]. Si el
/// dispositivo terminó entre la mirada y la espera, `IRQ_PENDING` ya está
/// activa y `wait` vuelve enseguida.
pub struct DevIrq {
    h: Option<CapHandle>,
    /// Interrupciones atendidas.
    pub count: u64,
}

impl DevIrq {
    /// La capacidad del slot de arranque `i`, si es una línea de interrupción.
    pub fn from_bootstrap(i: u32) -> DevIrq {
        let h = bootstrap(i);
        let is_irq = cap_info(h).is_ok_and(|c| c.object_type == abi::ObjectType::Irq as u32);
        DevIrq {
            h: is_irq.then_some(h),
            count: 0,
        }
    }

    /// ¿El kernel entregó la línea?
    pub fn present(&self) -> bool {
        self.h.is_some()
    }

    /// La capacidad, si la hay.
    pub fn handle(&self) -> Option<CapHandle> {
        self.h
    }

    /// Duerme hasta la próxima interrupción, como mucho `timeout_ns` (una red
    /// de seguridad: el driver vuelve a mirar igual). Sin línea, cede la CPU.
    /// Devuelve `true` si hay una interrupción para atender.
    pub fn wait(&mut self, timeout_ns: u64) -> bool {
        let Some(h) = self.h else {
            yield_now();
            return false;
        };
        let mut it = [wait_item(h, abi::cap::signals::IRQ_PENDING)];
        if wait(&mut it, timeout_ns) == Ok(1) {
            self.count += 1;
            return true;
        }
        false
    }

    /// Ítem de [`wait`] para esperar `IRQ_PENDING` junto con otros handles
    /// (solo tiene sentido si [`DevIrq::present`]).
    pub fn item(&self) -> abi::syscall::WaitItem {
        wait_item(
            self.h.unwrap_or(CapHandle::INVALID),
            abi::cap::signals::IRQ_PENDING,
        )
    }

    /// Fin de la atención: desenmascara la línea.
    pub fn ack(&self) {
        if let Some(h) = self.h {
            let _ = irq_ack(h);
        }
    }
}

/// Duplica una capacidad con derechos atenuados.
pub fn cap_dup(h: CapHandle, mask: Rights) -> SysResult<CapHandle> {
    decode_ret(syscall4(
        Syscall::CapDup,
        h.0 as u64,
        mask.bits() as u64,
        0,
        0,
    ))
    .map(|v| CapHandle(v as u32))
}

/// Libera una capacidad.
pub fn cap_drop(h: CapHandle) -> SysResult<()> {
    decode_ret(syscall4(Syscall::CapDrop, h.0 as u64, 0, 0, 0)).map(|_| ())
}

/// Consulta una capacidad.
pub fn cap_info(h: CapHandle) -> SysResult<CapInfo> {
    let mut info = CapInfo {
        object_type: 0,
        rights: 0,
        koid: 0,
    };
    decode_ret(syscall4(
        Syscall::CapInfo,
        h.0 as u64,
        &mut info as *mut _ as u64,
        0,
        0,
    ))?;
    Ok(info)
}

/// Crea una cell a partir de la imagen `image` y le *mueve* `caps` (las
/// recibe en los slots `1..=n`). Devuelve una capacidad a la cell nueva.
pub fn cell_spawn(image: CapHandle, caps: &[CapHandle]) -> SysResult<CapHandle> {
    decode_ret(syscall4(
        Syscall::CellSpawn,
        image.0 as u64,
        caps.as_ptr() as u64,
        caps.len() as u64,
        0,
    ))
    .map(|v| CapHandle(v as u32))
}

/// Pide terminar una cell (requiere `CONTROL`). No espera.
pub fn cell_kill(h: CapHandle) -> SysResult<()> {
    decode_ret(syscall4(Syscall::CellKill, h.0 as u64, 0, 0, 0)).map(|_| ())
}

/// Cómo terminó una cell. Bloquea hasta que termine, salvo con `NONBLOCK`
/// (entonces puede devolver `cell_state::RUNNING`).
pub fn cell_status(h: CapHandle, flags: u64) -> SysResult<abi::syscall::CellStatus> {
    let mut out = [0u8; abi::syscall::CellStatus::SIZE];
    decode_ret(syscall4(
        Syscall::CellStatus,
        h.0 as u64,
        out.as_mut_ptr() as u64,
        flags,
        0,
    ))?;
    abi::syscall::CellStatus::from_bytes(&out).ok_or(Error::BadMessage)
}

/// Crea un objeto de memoria de `size` bytes (redondeado a páginas), a cero.
pub fn mem_create(size: usize) -> SysResult<CapHandle> {
    decode_ret(syscall4(Syscall::MemCreate, size as u64, 0, 0, 0)).map(|v| CapHandle(v as u32))
}

/// Mapea un objeto de memoria en `vaddr` con `rights` (READ obligatorio).
pub fn mem_map(h: CapHandle, vaddr: u64, rights: Rights) -> SysResult<Mapped> {
    let info = mem_info(h)?;
    decode_ret(syscall4(
        Syscall::MemMap,
        h.0 as u64,
        vaddr,
        rights.bits() as u64,
        0,
    ))?;
    Ok(Mapped {
        base: vaddr,
        len: info.size as usize,
    })
}

/// Tamaño y dirección física (si la capacidad tiene CONTROL).
pub fn mem_info(h: CapHandle) -> SysResult<abi::syscall::MemInfo> {
    let mut out = abi::syscall::MemInfo::default();
    decode_ret(syscall4(
        Syscall::MemInfo,
        h.0 as u64,
        &mut out as *mut _ as u64,
        0,
        0,
    ))?;
    Ok(out)
}

/// Lee el puerto `off` (relativo al inicio del rango concedido); `width` =
/// 1, 2 o 4 bytes.
pub fn ioport_read(h: CapHandle, off: u16, width: u8) -> SysResult<u32> {
    decode_ret(syscall4(
        Syscall::IoPortRead,
        h.0 as u64,
        off as u64,
        width as u64,
        0,
    ))
    .map(|v| v as u32)
}

/// Escribe el puerto `off` (relativo al inicio del rango concedido).
pub fn ioport_write(h: CapHandle, off: u16, width: u8, value: u32) -> SysResult<()> {
    decode_ret(syscall4(
        Syscall::IoPortWrite,
        h.0 as u64,
        off as u64,
        width as u64,
        value as u64,
    ))
    .map(|_| ())
}

/// Región de memoria mapeada con accesos volátiles y con control de rango.
/// Es la forma segura de que un driver comparta estructuras con un
/// dispositivo (DMA) sin escribir `unsafe`.
#[derive(Clone, Copy)]
pub struct Mapped {
    base: u64,
    len: usize,
}

macro_rules! accessors {
    ($($r:ident $w:ident $t:ty),*) => {$(
        /// Lectura volátil (pánico si se sale del rango o no está alineada).
        pub fn $r(&self, off: usize) -> $t {
            let p = self.ptr::<$t>(off);
            // SAFETY: `ptr` comprobó rango y alineación dentro de la región
            // que el kernel mapeó para esta cell.
            unsafe { core::ptr::read_volatile(p) }
        }
        /// Escritura volátil (pánico si se sale del rango o no está alineada).
        pub fn $w(&self, off: usize, v: $t) {
            let p = self.ptr::<$t>(off) as *mut $t;
            // SAFETY: ídem.
            unsafe { core::ptr::write_volatile(p, v) }
        }
    )*};
}

impl Mapped {
    /// Tamaño en bytes.
    pub fn len(&self) -> usize {
        self.len
    }
    /// `true` si está vacía.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn ptr<T>(&self, off: usize) -> *const T {
        let n = core::mem::size_of::<T>();
        assert!(
            off.checked_add(n).is_some_and(|e| e <= self.len),
            "acceso fuera de la región"
        );
        assert!(off.is_multiple_of(n), "acceso desalineado");
        (self.base as usize + off) as *const T
    }
    accessors!(read_u8 write_u8 u8, read_u16 write_u16 u16, read_u32 write_u32 u32, read_u64 write_u64 u64);

    /// Copia bytes a la región.
    pub fn write_bytes(&self, off: usize, data: &[u8]) {
        for (i, b) in data.iter().enumerate() {
            self.write_u8(off + i, *b);
        }
    }
}

/// Reloj monótono en nanosegundos desde el arranque.
pub fn clock_ns() -> u64 {
    syscall4(Syscall::ClockMonotonic, 0, 0, 0, 0).max(0) as u64
}

/// 64 bits aleatorios del generador de la CPU (`rdrand`), o `None` si la CPU
/// no lo tiene o no entregó un valor tras varios intentos.
#[cfg(target_arch = "x86_64")]
pub fn random_u64() -> Option<u64> {
    let (_, _, ecx, _) = {
        let r = core::arch::x86_64::__cpuid(1);
        (r.eax, r.ebx, r.ecx, r.edx)
    };
    if ecx & (1 << 30) == 0 {
        return None;
    }
    for _ in 0..16 {
        let mut v = 0u64;
        // SAFETY: CPUID.1:ECX[30] dice que la CPU tiene `rdrand`; la
        // instrucción solo escribe `v` y no toca memoria compartida.
        if unsafe { rdrand64(&mut v) } == 1 {
            return Some(v);
        }
    }
    None
}

/// 64 bits aleatorios de la CPU. En ARM64 una cell no puede consultar si la
/// CPU tiene `RNDR` (leer `ID_AA64ISAR0_EL1` desde EL0 es una excepción),
/// así que por ahora no hay.
#[cfg(not(target_arch = "x86_64"))]
pub fn random_u64() -> Option<u64> {
    None
}

/// Solo se puede llamar (con `unsafe`) si la CPU tiene `rdrand`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "rdrand")]
fn rdrand64(v: &mut u64) -> i32 {
    core::arch::x86_64::_rdrand64_step(v)
}

/// Barrera de memoria completa (ordena accesos a memoria compartida con un
/// dispositivo).
pub fn fence() {
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
}

/// Llamada cruda (también para tests de aislamiento, con argumentos
/// arbitrarios).
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn raw_syscall(n: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: la convención está definida en `nsos_abi::syscall`: el kernel
    // solo modifica rax, rcx y r11, y valida todos los punteros que recibe.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") n => ret,
            in("rdi") a0, in("rsi") a1, in("rdx") a2, in("r10") a3,
            lateout("rcx") _, lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Llamada cruda (también para tests de aislamiento, con argumentos
/// arbitrarios).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub fn raw_syscall(n: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: la convención está definida en `nsos_abi::syscall`: el kernel
    // solo modifica x0 y valida todos los punteros que recibe.
    unsafe {
        asm!(
            "svc #0",
            in("x8") n,
            inlateout("x0") a0 => ret,
            in("x1") a1, in("x2") a2, in("x3") a3,
            options(nostack),
        );
    }
    ret
}

/// Búfer de formato en la pila para `print!`.
pub struct LineBuf {
    buf: [u8; 256],
    len: usize,
}

impl LineBuf {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self {
            buf: [0; 256],
            len: 0,
        }
    }
    pub fn flush(&mut self) {
        if let Ok(s) = core::str::from_utf8(&self.buf[..self.len]) {
            debug_log(s);
        }
        self.len = 0;
    }
}

impl Write for LineBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.len == self.buf.len() {
                self.flush();
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let mut b = LineBuf::new();
    let _ = b.write_fmt(args);
    b.flush();
}

/// `println!` de las cells (va a la consola de depuración del kernel).
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => { $crate::_print(format_args!("{}\n", format_args!($($arg)*))) };
}

/// Declara la función principal de la cell: `fn main() -> i32`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        // Única excepción a `deny(unsafe_code)` en las cells: exportar el
        // símbolo de entrada que salta el kernel.
        #[allow(unsafe_code)]
        #[no_mangle]
        pub extern "C" fn _start() -> ! {
            let f: fn() -> i32 = $main;
            $crate::exit(f())
        }
    };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    _print(format_args!("panic en cell: {}\n", info));
    exit(101)
}
