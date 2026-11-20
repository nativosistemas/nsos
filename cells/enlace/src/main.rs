//! Cell `enlace`: une dos máquinas por la red (virtio-net) con un canal
//! autenticado y lleva por él la migración de un servicio.
//!
//! Modelos: `verification/lean/Nsos/Migration.lean` (la migración) y
//! `verification/lean/Nsos/Link.lean` (el enlace: nada falsificado, nada
//! repetido, todo en orden). La parte criptográfica está en `crates/link`.
//!
//! Capacidades de arranque:
//! 1. Canal con `virtnet` (el driver de red): `SEND`/`RECV` de paquetes.
//! 2. `Memory` de solo lectura con la configuración que puso el kernel: la
//!    clave compartida (32 bytes), el rol de esta máquina (byte 32: 0 =
//!    origen, 1 = destino) y una semilla para el nonce (bytes 33..49), que
//!    se usa solo si la CPU no da números aleatorios a la cell (ARM64).
//! 3. Canal de control con el supervisor de esta máquina.
//!
//! Al arrancar, negocia una sesión con la otra punta (`HELLO` con un nonce
//! aleatorio de cada lado, confirmación autenticada). Si no lo logra en
//! `HANDSHAKE_NS`, no hay migración: del lado de origen contesta
//! `UPGRADE_REJECT` a cada pedido (el supervisor hace rollback y el servicio
//! sigue local); del lado de destino termina con error. Una trama que no
//! verifica se descarta y se cuenta.
//!
//! Por la red solo viajan **bytes**: una copia del estado, los pedidos del
//! cliente y sus respuestas. Nunca capacidades.
//!
//! Para el supervisor de origen, el enlace *es* la versión nueva: recibe
//! `UPGRADE_RESTORE [mem, ep]` (con el nombre de la versión que debe
//! arrancar en la otra máquina) y contesta `UPGRADE_ACCEPT` o
//! `UPGRADE_REJECT [mem, ep]` según decida la otra punta. Si aceptó, se queda
//! con `ep` y reenvía cada pedido del cliente (uno por vez). Del lado de
//! destino hace lo simétrico con un `Memory` y un canal locales.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_link::{Role, Session, NONCE_LEN};
use nsos_rt::abi::msg::labels::{UPGRADE_ACCEPT, UPGRADE_REJECT, UPGRADE_RESTORE};
use nsos_rt::abi::msg::MAX_DATA;
use nsos_rt::abi::syscall::flags::NONBLOCK;
use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    bootstrap, cap_drop, channel_create, channel_recv, channel_send, clock_ns, mem_create, mem_map,
    println, random_u64, yield_now,
};

/// Etiquetas del protocolo con `virtnet`.
const NET_SEND: u64 = 1;
const NET_RECV: u64 = 2;

// Tipos de mensaje dentro del enlace (primer byte de los datos).
const T_MIGRATE: u8 = 1;
const T_ACCEPT: u8 = 2;
const T_REJECT: u8 = 3;
const T_REQ: u8 = 4;
const T_RESP: u8 = 5;
const T_CLOSE: u8 = 6;

/// Bytes del estado que se copian (formatos `CNT1`/`CNT2` del contador).
const STATE_LEN: usize = 32;
const NAME_MAX: usize = 32;
const CONFIG_VADDR: u64 = 0x2000_0000;
/// Cada intento mapea el estado en una dirección nueva.
const STATE_VADDR: u64 = 0x3000_0000;
const STATE_STRIDE: u64 = 0x10_0000;
const MAX_ATTEMPTS: u64 = 8;
const HANDSHAKE_NS: u64 = 10_000_000_000;
const HELLO_EVERY_NS: u64 = 50_000_000;
/// Lo más largo que entra en un paquete (un mensaje del canal con `virtnet`).
const PACKET_MAX: usize = MAX_DATA;
/// Lo más largo que puede llevar un mensaje del enlace.
const DATA_MAX: usize = PACKET_MAX - nsos_link::DATA_OVERHEAD;

nsos_rt::entry!(main);

struct Link {
    net: CapHandle,
    role: Role,
    psk: [u8; 32],
    nonce: [u8; NONCE_LEN],
    peer_nonce: Option<[u8; NONCE_LEN]>,
    session: Option<Session>,
    /// La otra punta probó que tiene la misma clave de sesión.
    established: bool,
    /// Un mensaje que llegó antes de terminar la negociación.
    pending: Option<([u8; DATA_MAX], usize)>,
    dropped: u64,
}

impl Link {
    fn send_packet(&self, p: &[u8]) -> Result<(), Error> {
        channel_send(self.net, &Message::new(NET_SEND, p)?, 0)
    }

    fn send_hello(&self) -> Result<(), Error> {
        let mut buf = [0u8; nsos_link::HELLO_LEN];
        let n =
            nsos_link::hello(self.role, &self.nonce, &mut buf).map_err(|_| Error::BadMessage)?;
        self.send_packet(&buf[..n])
    }

    fn send_confirm(&self) -> Result<(), Error> {
        if let Some(s) = &self.session {
            let mut buf = [0u8; nsos_link::DATA_OVERHEAD];
            let n = s.confirm(&mut buf).map_err(|_| Error::BadMessage)?;
            self.send_packet(&buf[..n])?;
        }
        Ok(())
    }

    fn drop_frame(&mut self, why: &str) {
        self.dropped += 1;
        if self.dropped <= 3 {
            println!("enlace: descarto una trama ({})", why);
        }
    }

    /// Procesa un paquete recibido. Devuelve los datos si era un mensaje
    /// aceptado (`Link.Step.deliver`).
    fn process(&mut self, p: &[u8], out: &mut [u8; DATA_MAX]) -> Result<Option<usize>, Error> {
        if let Some((role, n)) = nsos_link::parse_hello(p) {
            if role != self.role.peer() {
                self.drop_frame("HELLO con mi propio rol");
                return Ok(None);
            }
            if self.peer_nonce != Some(n) {
                if self.established {
                    // La otra punta reinició (o alguien inventa HELLOs): no
                    // se mezclan sesiones.
                    println!("enlace: la otra punta cambió de sesión");
                    return Err(Error::PeerClosed);
                }
                let (o, d) = match self.role {
                    Role::Origin => (self.nonce, n),
                    Role::Dest => (n, self.nonce),
                };
                self.session = Some(Session::new(&self.psk, self.role, &o, &d));
                self.peer_nonce = Some(n);
            }
            self.send_confirm()?;
            return Ok(None);
        }
        let Some(s) = self.session.as_mut() else {
            self.drop_frame("sin sesión todavía");
            return Ok(None);
        };
        if s.is_confirm(p) {
            self.established = true;
            return Ok(None);
        }
        match s.open(p) {
            Ok(data) if data.len() <= DATA_MAX => {
                // Un mensaje válido también prueba la clave.
                self.established = true;
                out[..data.len()].copy_from_slice(data);
                Ok(Some(data.len()))
            }
            Ok(_) => Err(Error::BadMessage),
            Err(e) => {
                self.drop_frame(match e {
                    nsos_link::LinkError::BadTag => "etiqueta inválida",
                    nsos_link::LinkError::BadSequence => "secuencia repetida o fuera de orden",
                    nsos_link::LinkError::WrongDirection => "dirección equivocada",
                    _ => "mal formada",
                });
                Ok(None)
            }
        }
    }

    /// Un paquete de la red, si hay.
    fn poll(&mut self, out: &mut [u8; DATA_MAX]) -> Result<Option<usize>, Error> {
        let mut m = Message::default();
        match channel_recv(self.net, &mut m, NONBLOCK) {
            Ok(()) if m.header.label == NET_RECV => self.process(m.payload(), out),
            Ok(()) => Err(Error::BadMessage),
            Err(Error::WouldBlock) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Negocia la sesión. `Ok(false)` si se venció el plazo.
    fn handshake(&mut self) -> Result<bool, Error> {
        let start = clock_ns();
        let mut last = 0u64;
        let mut buf = [0u8; DATA_MAX];
        while !self.established {
            let now = clock_ns();
            if now - start > HANDSHAKE_NS {
                return Ok(false);
            }
            if last == 0 || now - last > HELLO_EVERY_NS {
                self.send_hello()?;
                self.send_confirm()?;
                last = now;
            }
            match self.poll(&mut buf)? {
                Some(n) => self.pending = Some((buf, n)),
                None => yield_now(),
            }
        }
        // Una confirmación más: la otra punta ya tiene la sesión (nos mandó
        // la suya), así que esta la puede verificar aunque se haya perdido
        // alguna anterior.
        self.send_confirm()?;
        Ok(true)
    }

    /// Manda un mensaje autenticado (`Link.Step.send`).
    fn send(&mut self, kind: u8, parts: &[&[u8]]) -> Result<(), Error> {
        let mut data = [0u8; DATA_MAX];
        data[0] = kind;
        let mut n = 1;
        for p in parts {
            let end = n + p.len();
            if end > DATA_MAX {
                return Err(Error::InvalidArgs);
            }
            data[n..end].copy_from_slice(p);
            n = end;
        }
        let s = self.session.as_mut().ok_or(Error::PeerClosed)?;
        let mut pkt = [0u8; PACKET_MAX];
        let len = s
            .seal(&data[..n], &mut pkt)
            .map_err(|_| Error::InvalidArgs)?;
        self.send_packet(&pkt[..len])
    }

    /// Espera el próximo mensaje autenticado. Devuelve su tipo y largo.
    fn recv(&mut self, out: &mut [u8; DATA_MAX]) -> Result<(u8, usize), Error> {
        if let Some((b, n)) = self.pending.take() {
            *out = b;
            return if n == 0 {
                Err(Error::BadMessage)
            } else {
                Ok((out[0], n))
            };
        }
        loop {
            match self.poll(out)? {
                Some(0) => return Err(Error::BadMessage),
                Some(n) => return Ok((out[0], n)),
                None => yield_now(),
            }
        }
    }
}

/// Origen, servicio aceptado en la otra punta: cada pedido del cliente
/// cruza la red y la respuesta vuelve por `ep`.
fn proxy_out(link: &mut Link, ep: CapHandle) -> Result<u64, Error> {
    let mut m = Message::default();
    let mut buf = [0u8; DATA_MAX];
    let mut n = 0;
    loop {
        match channel_recv(ep, &mut m, 0) {
            Ok(()) => {
                for &c in m.cap_slice() {
                    // Las capacidades no cruzan de máquina.
                    cap_drop(c)?;
                }
                link.send(T_REQ, &[&m.header.label.to_le_bytes(), m.payload()])?;
                let (kind, len) = link.recv(&mut buf)?;
                if kind != T_RESP || len < 9 {
                    return Err(Error::BadMessage);
                }
                let label = u64::from_le_bytes(buf[1..9].try_into().unwrap());
                channel_send(ep, &Message::new(label, &buf[9..len])?, 0)?;
                n += 1;
            }
            Err(Error::PeerClosed) => {
                link.send(T_CLOSE, &[])?;
                return Ok(n);
            }
            Err(e) => return Err(e),
        }
    }
}

fn reject(ctrl: CapHandle, mem: CapHandle, ep: CapHandle) -> Result<(), Error> {
    let mut r = Message::new(UPGRADE_REJECT, b"")?;
    r.push_cap(mem)?;
    r.push_cap(ep)?;
    channel_send(ctrl, &r, 0)
}

/// Origen: el supervisor entregó `[mem, ep]`. Manda la copia del estado y
/// espera la decisión. Devuelve `true` si terminó el servicio.
fn migrate_out(link: &mut Link, ctrl: CapHandle, m: &Message, attempt: u64) -> Result<bool, Error> {
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
    link.send(T_MIGRATE, &[&[name.len() as u8], name, &copy])?;
    let mut buf = [0u8; DATA_MAX];
    match link.recv(&mut buf)?.0 {
        T_ACCEPT => {
            println!("enlace: la otra máquina aceptó; reenvío los pedidos del cliente");
            channel_send(ctrl, &Message::new(UPGRADE_ACCEPT, b"")?, 0)?;
            let n = proxy_out(link, ep)?;
            println!(
                "enlace: el cliente cerró; reenvié {} pedidos ({} tramas descartadas)",
                n, link.dropped
            );
            Ok(true)
        }
        T_REJECT => {
            println!("enlace: la otra máquina rechazó; devuelvo estado y cliente");
            reject(ctrl, mem, ep)?;
            Ok(false)
        }
        _ => Err(Error::BadMessage),
    }
}

/// Destino: llegó una copia del estado. Arma `[mem, ep]` locales, se los
/// pasa al supervisor y contesta su decisión. Devuelve el extremo del
/// servicio si aceptó.
fn migrate_in(
    link: &mut Link,
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
            link.send(T_ACCEPT, &[])?;
            Ok(Some(proxy_end))
        }
        UPGRADE_REJECT => {
            for &c in m.cap_slice() {
                cap_drop(c)?;
            }
            cap_drop(proxy_end)?;
            link.send(T_REJECT, &[])?;
            Ok(None)
        }
        _ => Err(Error::BadMessage),
    }
}

/// Destino, servicio aceptado: entrega cada pedido que llega por la red.
fn proxy_in(link: &mut Link, srv: CapHandle) -> Result<u64, Error> {
    let mut buf = [0u8; DATA_MAX];
    let mut m = Message::default();
    let mut n = 0;
    loop {
        let (kind, len) = link.recv(&mut buf)?;
        match kind {
            T_REQ if len >= 9 => {
                let label = u64::from_le_bytes(buf[1..9].try_into().unwrap());
                channel_send(srv, &Message::new(label, &buf[9..len])?, 0)?;
                channel_recv(srv, &mut m, 0)?;
                for &c in m.cap_slice() {
                    cap_drop(c)?;
                }
                link.send(T_RESP, &[&m.header.label.to_le_bytes(), m.payload()])?;
                n += 1;
            }
            T_CLOSE => {
                cap_drop(srv)?;
                return Ok(n);
            }
            _ => return Err(Error::BadMessage),
        }
    }
}

fn run_origin(link: &mut Link, ctrl: CapHandle, authenticated: bool) -> Result<(), Error> {
    let mut m = Message::default();
    for attempt in 0..MAX_ATTEMPTS {
        match channel_recv(ctrl, &mut m, 0) {
            Ok(()) if m.header.label == UPGRADE_RESTORE => {
                if !authenticated {
                    println!("enlace: sin enlace autenticado; rechazo la migración");
                    let &[mem, ep] = m.cap_slice() else {
                        return Err(Error::BadMessage);
                    };
                    reject(ctrl, mem, ep)?;
                    continue;
                }
                if migrate_out(link, ctrl, &m, attempt)? {
                    return Ok(());
                }
            }
            Ok(()) => return Err(Error::BadMessage),
            Err(Error::PeerClosed) if authenticated => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Err(Error::TimedOut)
}

fn run_dest(link: &mut Link, ctrl: CapHandle) -> Result<(), Error> {
    let mut buf = [0u8; DATA_MAX];
    for attempt in 0..MAX_ATTEMPTS {
        let (kind, len) = link.recv(&mut buf)?;
        if kind != T_MIGRATE {
            return Err(Error::BadMessage);
        }
        println!("enlace: llegó un estado autenticado de la otra máquina");
        if let Some(srv) = migrate_in(link, ctrl, &buf[1..len], attempt)? {
            let n = proxy_in(link, srv)?;
            println!(
                "enlace: el cliente remoto cerró; atendí {} pedidos ({} tramas descartadas)",
                n, link.dropped
            );
            return Ok(());
        }
    }
    Err(Error::TimedOut)
}

fn run() -> Result<(), Error> {
    let net = bootstrap(1);
    let config = mem_map(bootstrap(2), CONFIG_VADDR, Rights::READ)?;
    let ctrl = bootstrap(3);
    let mut psk = [0u8; 32];
    for (i, b) in psk.iter_mut().enumerate() {
        *b = config.read_u8(i);
    }
    let role = match config.read_u8(32) {
        0 => Role::Origin,
        1 => Role::Dest,
        _ => return Err(Error::InvalidArgs),
    };
    // Nonce: de la CPU (`rdrand`) o, si la cell no tiene cómo pedirlo (ARM64),
    // la semilla que dejó el kernel en la configuración (bytes 33..49).
    let mut nonce = [0u8; NONCE_LEN];
    for (i, half) in nonce.chunks_mut(8).enumerate() {
        let r = match random_u64() {
            Some(r) => r.to_le_bytes(),
            None => core::array::from_fn(|j| config.read_u8(33 + 8 * i + j)),
        };
        half.copy_from_slice(&r);
    }
    if nonce == [0; NONCE_LEN] {
        return Err(Error::Unsupported);
    }
    let mut link = Link {
        net,
        role,
        psk,
        nonce,
        peer_nonce: None,
        session: None,
        established: false,
        pending: None,
        dropped: 0,
    };
    let ok = link.handshake()?;
    if ok {
        println!("enlace: sesión autenticada con la otra máquina");
    } else {
        println!(
            "enlace: no se pudo autenticar a la otra máquina ({} tramas descartadas)",
            link.dropped
        );
    }
    match role {
        Role::Origin => run_origin(&mut link, ctrl, ok)?,
        Role::Dest if ok => run_dest(&mut link, ctrl)?,
        Role::Dest => return Err(Error::AccessDenied),
    }
    if ok {
        Ok(())
    } else {
        Err(Error::AccessDenied)
    }
}

fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            println!("enlace: error {:?}", e);
            1
        }
    }
}
