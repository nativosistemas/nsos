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
use abi::{error::decode_ret, CapHandle, Message, Rights, SysResult};
use core::arch::asm;
use core::fmt::{self, Write};

/// Syscall cruda con hasta 4 argumentos.
#[inline(always)]
fn syscall4(n: Syscall, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: la convención está definida en `nsos_abi::syscall`: el kernel
    // solo modifica rax, rcx y r11, y valida todos los punteros que recibe.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") n as u64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
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

/// Llamada cruda para tests de aislamiento (argumentos arbitrarios).
pub fn raw_syscall(n: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: ídem `syscall4`; el kernel debe rechazar cualquier argumento.
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
