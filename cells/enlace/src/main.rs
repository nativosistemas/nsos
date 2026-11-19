//! Cell `enlace`: une dos máquinas por un puerto serie (COM2) y lleva por
//! él la migración de un servicio (modelo:
//! `verification/lean/Nsos/Migration.lean`).
//!
//! Capacidades de arranque:
//! 1. `IoPorts`: los 8 registros del UART 16550 de COM2.
//! 2. Canal de control con el supervisor de esta máquina.
//!
//! Por el cable solo viajan **bytes**: el estado del servicio (una copia),
//! los pedidos del cliente y sus respuestas. Nunca capacidades.
//!
//! Para el supervisor del lado de origen, el enlace *es* la versión nueva:
//! recibe `UPGRADE_RESTORE [mem, ep]` (con el nombre de la versión que debe
//! arrancar en la otra máquina), manda una copia del estado y responde
//! `UPGRADE_ACCEPT` o `UPGRADE_REJECT [mem, ep]` según decida la otra punta.
//! Si aceptó, se queda con `ep` y reenvía cada pedido del cliente por el
//! cable (un pedido por vez) hasta que el cliente cierre.
//!
//! Del lado de destino hace lo simétrico: al llegar la copia crea un objeto
//! `Memory` nuevo con ella y un canal, y se los pasa a su supervisor con
//! `UPGRADE_RESTORE`; con la decisión responde por el cable y, si aceptó,
//! entrega los pedidos que llegan al servicio local.
//!
//! Tramas: `0xA5`, tipo (u8), largo (u16), datos, suma (u8) de tipo, largo
//! y datos. El cable de QEMU es confiable; una suma mala es un error fatal
//! (el cliente ve `PEER_CLOSED`, nunca un valor inventado).

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::labels::{UPGRADE_ACCEPT, UPGRADE_REJECT, UPGRADE_RESTORE};
use nsos_rt::abi::msg::MAX_DATA;
use nsos_rt::abi::syscall::flags::NONBLOCK;
use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    bootstrap, cap_drop, channel_create, channel_recv, channel_send, ioport_read, ioport_write,
    mem_create, mem_map, println, yield_now,
};

// UART 16550 (desplazamientos desde la base de COM2).
const DATA: u16 = 0;
const IER: u16 = 1;
const FCR: u16 = 2;
const LCR: u16 = 3;
const MCR: u16 = 4;
const LSR: u16 = 5;
const LSR_DATA_READY: u32 = 1 << 0;
const LSR_THR_EMPTY: u32 = 1 << 5;
const LSR_IDLE: u32 = 1 << 6;

// Tipos de trama.
const SYNC: u8 = 0xA5;
const F_MIGRATE: u8 = 1;
const F_ACCEPT: u8 = 2;
const F_REJECT: u8 = 3;
const F_REQ: u8 = 4;
const F_RESP: u8 = 5;
const F_CLOSE: u8 = 6;

/// Bytes del estado que se copian (formatos `CNT1`/`CNT2` del contador).
const STATE_LEN: usize = 32;
const NAME_MAX: usize = 32;
/// Cada intento mapea el estado en una dirección nueva (no hay `mem_unmap`).
const STATE_VADDR: u64 = 0x3000_0000;
const STATE_STRIDE: u64 = 0x10_0000;
const MAX_ATTEMPTS: u64 = 8;
const FRAME_MAX: usize = 8 + MAX_DATA;

nsos_rt::entry!(main);

struct Uart(CapHandle);

impl Uart {
    fn init(&self) -> Result<(), Error> {
        ioport_write(self.0, IER, 1, 0)?; // sin interrupciones: se sondea
        ioport_write(self.0, LCR, 1, 0x80)?; // DLAB
        ioport_write(self.0, DATA, 1, 1)?; // divisor 1 (115200)
        ioport_write(self.0, IER, 1, 0)?;
        ioport_write(self.0, LCR, 1, 0x03)?; // 8N1
                                             // Sin FIFO, y sobre todo sin limpiarla: si la otra máquina ya mandó
                                             // algo, el primer byte espera en el registro de recepción y QEMU
                                             // retiene el resto. Limpiar la FIFO aquí lo perdería.
        ioport_write(self.0, FCR, 1, 0)?;
        ioport_write(self.0, MCR, 1, 0x03) // DTR + RTS
    }

    fn put(&self, b: u8) -> Result<(), Error> {
        while ioport_read(self.0, LSR, 1)? & LSR_THR_EMPTY == 0 {
            yield_now();
        }
        ioport_write(self.0, DATA, 1, b as u32)
    }

    fn try_get(&self) -> Result<Option<u8>, Error> {
        if ioport_read(self.0, LSR, 1)? & LSR_DATA_READY == 0 {
            return Ok(None);
        }
        Ok(Some(ioport_read(self.0, DATA, 1)? as u8))
    }

    fn get(&self) -> Result<u8, Error> {
        loop {
            if let Some(b) = self.try_get()? {
                return Ok(b);
            }
            yield_now();
        }
    }

    /// Espera a que el UART haya sacado todo (antes de terminar).
    fn drain(&self) -> Result<(), Error> {
        while ioport_read(self.0, LSR, 1)? & LSR_IDLE == 0 {
            yield_now();
        }
        Ok(())
    }

    fn send(&self, kind: u8, parts: &[&[u8]]) -> Result<(), Error> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        if len > FRAME_MAX {
            return Err(Error::BadMessage);
        }
        let hdr = [kind, len as u8, (len >> 8) as u8];
        let mut sum = 0u8;
        self.put(SYNC)?;
        for &b in hdr.iter().chain(parts.iter().flat_map(|p| p.iter())) {
            sum = sum.wrapping_add(b);
            self.put(b)?;
        }
        self.put(sum)
    }

    /// Recibe una trama completa; si `first` ya trae el byte de sincronía,
    /// no lo vuelve a esperar.
    fn recv<'a>(
        &self,
        buf: &'a mut [u8; FRAME_MAX],
        first: Option<u8>,
    ) -> Result<(u8, &'a [u8]), Error> {
        let sync = match first {
            Some(b) => b,
            None => self.get()?,
        };
        if sync != SYNC {
            println!("enlace: byte de sincronía inválido {:#x}", sync);
            return Err(Error::BadMessage);
        }
        let kind = self.get()?;
        let (lo, hi) = (self.get()?, self.get()?);
        let len = u16::from_le_bytes([lo, hi]) as usize;
        if len > FRAME_MAX {
            return Err(Error::BadMessage);
        }
        let mut sum = kind.wrapping_add(lo).wrapping_add(hi);
        for b in buf.iter_mut().take(len) {
            *b = self.get()?;
            sum = sum.wrapping_add(*b);
        }
        if self.get()? != sum {
            println!("enlace: suma de control inválida");
            return Err(Error::BadMessage);
        }
        Ok((kind, &buf[..len]))
    }
}

/// Origen, servicio aceptado en la otra punta: cada pedido del cliente
/// cruza el cable y la respuesta vuelve por `ep`.
fn proxy_out(uart: &Uart, ep: CapHandle) -> Result<u64, Error> {
    let mut m = Message::default();
    let mut buf = [0u8; FRAME_MAX];
    let mut n = 0;
    loop {
        match channel_recv(ep, &mut m, 0) {
            Ok(()) => {
                for &c in m.cap_slice() {
                    // Las capacidades no cruzan el cable.
                    cap_drop(c)?;
                }
                uart.send(F_REQ, &[&m.header.label.to_le_bytes(), m.payload()])?;
                let (kind, data) = uart.recv(&mut buf, None)?;
                if kind != F_RESP || data.len() < 8 {
                    return Err(Error::BadMessage);
                }
                let label = u64::from_le_bytes(data[..8].try_into().unwrap());
                channel_send(ep, &Message::new(label, &data[8..])?, 0)?;
                n += 1;
            }
            Err(Error::PeerClosed) => {
                uart.send(F_CLOSE, &[])?;
                uart.drain()?;
                return Ok(n);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Origen: el supervisor entregó `[mem, ep]`. Manda la copia del estado y
/// espera la decisión. Devuelve `true` si terminó el servicio (aceptado y
/// el cliente cerró).
fn migrate_out(uart: &Uart, ctrl: CapHandle, m: &Message, attempt: u64) -> Result<bool, Error> {
    let &[mem, ep] = m.cap_slice() else {
        return Err(Error::BadMessage);
    };
    let name = m.payload();
    if name.is_empty() || name.len() > NAME_MAX {
        return Err(Error::BadMessage);
    }
    // Solo lectura: si la otra punta rechaza, el estado vuelve intacto.
    let state = mem_map(mem, STATE_VADDR + attempt * STATE_STRIDE, Rights::READ)?;
    let mut copy = [0u8; STATE_LEN];
    for (i, b) in copy.iter_mut().enumerate() {
        *b = state.read_u8(i);
    }
    println!(
        "enlace: mando el estado a la otra máquina ({} bytes, para {})",
        STATE_LEN,
        core::str::from_utf8(name).unwrap_or("?")
    );
    uart.send(F_MIGRATE, &[&[name.len() as u8], name, &copy])?;
    let mut buf = [0u8; FRAME_MAX];
    match uart.recv(&mut buf, None)?.0 {
        F_ACCEPT => {
            println!("enlace: la otra máquina aceptó; reenvío los pedidos del cliente");
            channel_send(ctrl, &Message::new(UPGRADE_ACCEPT, b"")?, 0)?;
            let n = proxy_out(uart, ep)?;
            println!("enlace: el cliente cerró; reenvié {} pedidos", n);
            Ok(true)
        }
        F_REJECT => {
            println!("enlace: la otra máquina rechazó; devuelvo estado y cliente");
            let mut r = Message::new(UPGRADE_REJECT, b"")?;
            r.push_cap(mem)?;
            r.push_cap(ep)?;
            channel_send(ctrl, &r, 0)?;
            Ok(false)
        }
        _ => Err(Error::BadMessage),
    }
}

/// Destino: llegó una copia del estado. Arma `[mem, ep]` locales, se los
/// pasa al supervisor y contesta su decisión por el cable. Devuelve el
/// extremo del servicio si aceptó.
fn migrate_in(
    uart: &Uart,
    ctrl: CapHandle,
    data: &[u8],
    attempt: u64,
) -> Result<Option<CapHandle>, Error> {
    let n = *data.first().ok_or(Error::BadMessage)? as usize;
    if n == 0 || n > NAME_MAX || data.len() != 1 + n + STATE_LEN {
        return Err(Error::BadMessage);
    }
    let (name, copy) = data[1..].split_at(n);
    let mem = mem_create(4096)?;
    let state = mem_map(
        mem,
        STATE_VADDR + attempt * STATE_STRIDE,
        Rights::READ | Rights::WRITE,
    )?;
    state.write_bytes(0, copy);
    let (proxy_end, srv_end) = channel_create()?;
    let mut r = Message::new(UPGRADE_RESTORE, name)?;
    r.push_cap(mem)?;
    r.push_cap(srv_end)?;
    channel_send(ctrl, &r, 0)?;
    let mut m = Message::default();
    channel_recv(ctrl, &mut m, 0)?;
    match m.header.label {
        UPGRADE_ACCEPT => {
            uart.send(F_ACCEPT, &[])?;
            Ok(Some(proxy_end))
        }
        UPGRADE_REJECT => {
            for &c in m.cap_slice() {
                cap_drop(c)?;
            }
            cap_drop(proxy_end)?;
            uart.send(F_REJECT, &[])?;
            Ok(None)
        }
        _ => Err(Error::BadMessage),
    }
}

/// Destino, servicio aceptado: entrega cada pedido que llega por el cable.
fn proxy_in(uart: &Uart, srv: CapHandle) -> Result<u64, Error> {
    let mut buf = [0u8; FRAME_MAX];
    let mut m = Message::default();
    let mut n = 0;
    loop {
        let (kind, data) = uart.recv(&mut buf, None)?;
        match kind {
            F_REQ if data.len() >= 8 => {
                let label = u64::from_le_bytes(data[..8].try_into().unwrap());
                channel_send(srv, &Message::new(label, &data[8..])?, 0)?;
                channel_recv(srv, &mut m, 0)?;
                for &c in m.cap_slice() {
                    cap_drop(c)?;
                }
                uart.send(F_RESP, &[&m.header.label.to_le_bytes(), m.payload()])?;
                n += 1;
            }
            F_CLOSE => {
                cap_drop(srv)?;
                return Ok(n);
            }
            _ => return Err(Error::BadMessage),
        }
    }
}

fn run(uart: &Uart, ctrl: CapHandle) -> Result<(), Error> {
    uart.init()?;
    let mut m = Message::default();
    let mut buf = [0u8; FRAME_MAX];
    for attempt in 0..MAX_ATTEMPTS {
        // Lo que llegue primero define de qué lado estamos.
        loop {
            match channel_recv(ctrl, &mut m, NONBLOCK) {
                Ok(()) if m.header.label == UPGRADE_RESTORE => {
                    if migrate_out(uart, ctrl, &m, attempt)? {
                        return Ok(());
                    }
                    break;
                }
                Ok(()) => return Err(Error::BadMessage),
                Err(Error::WouldBlock) => {}
                Err(Error::PeerClosed) => return Ok(()),
                Err(e) => return Err(e),
            }
            if let Some(b) = uart.try_get()? {
                let (kind, data) = uart.recv(&mut buf, Some(b))?;
                if kind != F_MIGRATE {
                    return Err(Error::BadMessage);
                }
                println!("enlace: llegó un estado de la otra máquina");
                if let Some(srv) = migrate_in(uart, ctrl, data, attempt)? {
                    let n = proxy_in(uart, srv)?;
                    println!("enlace: el cliente remoto cerró; atendí {} pedidos", n);
                    return Ok(());
                }
                break;
            }
            yield_now();
        }
    }
    Err(Error::TimedOut)
}

fn main() -> i32 {
    let uart = Uart(bootstrap(1));
    let ctrl = bootstrap(2);
    match run(&uart, ctrl) {
        Ok(()) => 0,
        Err(e) => {
            println!("enlace: error {:?}", e);
            1
        }
    }
}
