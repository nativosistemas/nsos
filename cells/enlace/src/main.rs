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
//! Contratos (`nsos_contract::protocols`): el flujo `net` con `virtnet`;
//! `restore` con el supervisor (del lado de origen el enlace *es* la
//! versión nueva: recibe `Restore` con el estado de solo lectura, el extremo
//! del cliente y el nombre de la versión que debe arrancar en la otra
//! máquina, y contesta `Accept` o `Reject` según decida la otra punta; del
//! lado de destino es el cliente y le pasa a `destino` un estado y un canal
//! locales); y `counter` con el cliente (origen) o con la versión nueva
//! (destino). Si la otra punta aceptó, el enlace de origen atiende el
//! extremo del cliente y reenvía cada pedido por la red (uno por vez); el
//! de destino los entrega por el canal local.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_link::{Role, Session, NONCE_LEN};
use nsos_rt::abi::msg::MAX_DATA;
use nsos_rt::abi::{Error, Rights};
use nsos_rt::contract::protocols::{counter, net, restore};
use nsos_rt::contract::{Bytes, Chan, Mem, Msg, RecvError, RW};
use nsos_rt::{bootstrap, clock_ns, mem_create, mem_map, println, random_u64, yield_now, Sys};

/// El flujo con `virtnet`.
type Net = Chan<Sys, net::client::Open>;

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
    /// `None` solo un instante, mientras se manda o se recibe.
    net: Option<Net>,
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
    /// Manda un paquete. Puede bloquear si la cola de `virtnet` está llena:
    /// `virtnet` nunca bloquea al entregar (descarta), así que siempre la
    /// termina vaciando.
    fn send_packet(&mut self, p: &[u8]) -> Result<(), Error> {
        let n = self.net.take().ok_or(Error::PeerClosed)?;
        let packet = Bytes::new(p).ok_or(Error::InvalidArgs)?;
        self.net = Some(n.send(net::Send { packet })?);
        Ok(())
    }

    fn send_hello(&mut self) -> Result<(), Error> {
        let mut buf = [0u8; nsos_link::HELLO_LEN];
        let n =
            nsos_link::hello(self.role, &self.nonce, &mut buf).map_err(|_| Error::BadMessage)?;
        self.send_packet(&buf[..n])
    }

    fn send_confirm(&mut self) -> Result<(), Error> {
        let mut buf = [0u8; nsos_link::DATA_OVERHEAD];
        let n = match &self.session {
            Some(s) => s.confirm(&mut buf).map_err(|_| Error::BadMessage)?,
            None => return Ok(()),
        };
        self.send_packet(&buf[..n])
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
        let n = self.net.take().ok_or(Error::PeerClosed)?;
        match n.try_recv() {
            Ok(net::client::recv::Open::Recv(n, p)) => {
                self.net = Some(n);
                self.process(p.packet.as_slice(), out)
            }
            Err(RecvError::Violation { chan, .. }) => {
                self.net = Some(chan);
                Err(Error::BadMessage)
            }
            Err(RecvError::WouldBlock(n)) => {
                self.net = Some(n);
                Ok(None)
            }
            Err(RecvError::Failed(e)) => Err(e),
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

/// El servicio que se reenvía es el contador (`counter`). Por el cable va
/// genérico: `T_REQ [etiqueta u64 | datos]` y `T_RESP [etiqueta u64 | datos]`.
fn encode_wire<M: Msg>(m: M, out: &mut [u8; DATA_MAX]) -> Result<usize, Error> {
    let mut msg = nsos_rt::abi::Message::default();
    m.encode(&mut msg);
    let p = msg.payload();
    out[..8].copy_from_slice(&M::LABEL.to_le_bytes());
    out.get_mut(8..8 + p.len())
        .ok_or(Error::InvalidArgs)?
        .copy_from_slice(p);
    Ok(8 + p.len())
}

/// Decodifica lo que llegó por el cable (`[etiqueta u64 | datos]`) como `M`.
fn decode_wire<M: Msg>(b: &[u8]) -> Result<M, Error> {
    let (label, data) = b.split_at_checked(8).ok_or(Error::BadMessage)?;
    let label = u64::from_le_bytes(label.try_into().map_err(|_| Error::BadMessage)?);
    let msg = nsos_rt::abi::Message::new(label, data)?;
    M::decode(Sys, &msg).ok_or(Error::BadMessage)
}

/// Origen, servicio aceptado en la otra punta: el enlace atiende el extremo
/// del cliente (`counter`) y cada pedido cruza la red; la respuesta vuelve
/// por el cable y por el mismo extremo.
fn proxy_out(link: &mut Link, mut ep: Chan<Sys, counter::server::Serving>) -> Result<u64, Error> {
    let mut buf = [0u8; DATA_MAX];
    let mut n = 0;
    loop {
        let waiting = match ep.recv() {
            Ok(counter::server::recv::Serving::Inc(w, inc)) => {
                let len = encode_wire(inc, &mut buf)?;
                link.send(T_REQ, &[&buf[..len]])?;
                w
            }
            Err(RecvError::Violation { chan, label }) => {
                println!("enlace: pedido fuera de protocolo del cliente ({})", label);
                ep = chan;
                continue;
            }
            Err(RecvError::WouldBlock(c)) => {
                ep = c;
                continue;
            }
            Err(RecvError::Failed(Error::PeerClosed)) => {
                link.send(T_CLOSE, &[])?;
                return Ok(n);
            }
            Err(RecvError::Failed(e)) => return Err(e),
        };
        let (kind, len) = link.recv(&mut buf)?;
        if kind != T_RESP {
            return Err(Error::BadMessage);
        }
        let count: counter::Count = decode_wire(&buf[1..len])?;
        ep = waiting.send(count)?;
        n += 1;
    }
}

/// Origen: el supervisor entregó estado (solo lectura) y extremo del
/// cliente. Manda la copia del estado y espera la decisión. Devuelve el
/// canal de control otra vez en `Start` si la otra punta rechazó, o `None`
/// si aceptó y el servicio ya terminó.
fn migrate_out(
    link: &mut Link,
    deciding: Chan<Sys, restore::server::Deciding>,
    r: restore::Restore,
    attempt: u64,
) -> Result<Option<Chan<Sys, restore::server::Start>>, Error> {
    let name = r.mode.as_slice();
    if name.is_empty() || name.len() > NAME_MAX {
        return Err(Error::BadMessage);
    }
    // Solo lectura (`Mem<RO>`): si la otra punta rechaza, el estado vuelve
    // intacto.
    let state = mem_map(
        r.state.handle(),
        STATE_VADDR + attempt * STATE_STRIDE,
        Rights::READ,
    )?;
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
            deciding.send(restore::Accept {})?.close();
            r.state.release(Sys);
            let n = proxy_out(link, Chan::attach(Sys, r.service))?;
            println!(
                "enlace: el cliente cerró; reenvié {} pedidos ({} tramas descartadas)",
                n, link.dropped
            );
            Ok(None)
        }
        T_REJECT => {
            println!("enlace: la otra máquina rechazó; devuelvo estado y cliente");
            let start = deciding.send(restore::Reject {
                state: r.state,
                service: r.service,
            })?;
            Ok(Some(start))
        }
        _ => Err(Error::BadMessage),
    }
}

/// Canal de control del lado de destino (el enlace es el cliente).
type DestCtrl = Chan<Sys, restore::client::Start>;
/// Punta del enlace del canal de servicio local, del lado de destino.
type Proxy = Chan<Sys, counter::client::Serving>;

/// Destino: llegó una copia del estado. Arma estado (de solo lectura) y un
/// canal de servicio locales, se los pasa al supervisor y contesta su
/// decisión. Devuelve el canal de control para el próximo intento y, si
/// aceptó, la punta del enlace del canal de servicio.
fn migrate_in(
    link: &mut Link,
    ctrl: DestCtrl,
    data: &[u8],
    attempt: u64,
) -> Result<(Option<DestCtrl>, Option<Proxy>), Error> {
    let n = *data.first().ok_or(Error::BadMessage)? as usize;
    if n == 0 || n > NAME_MAX || data.len() != 1 + n + STATE_LEN {
        return Err(Error::BadMessage);
    }
    let (name, copy) = data[1..].split_at(n);
    let mem = Mem::<RW>::open(Sys, mem_create(4096)?)?;
    let state = mem_map(
        mem.handle(),
        STATE_VADDR + attempt * STATE_STRIDE,
        Rights::READ | Rights::WRITE,
    )?;
    state.write_bytes(0, copy);
    // Al supervisor le llega de solo lectura, como exige `restore`; la
    // escribible se suelta.
    let ro = mem.read_only(Sys)?;
    mem.release(Sys);
    let (proxy, srv) = counter::pair(Sys)?;
    let deciding = ctrl.send(restore::Restore {
        state: ro,
        service: srv.into_endpoint(),
        mode: Bytes::new(name).ok_or(Error::BadMessage)?,
    })?;
    match deciding.recv() {
        Ok(restore::client::recv::Deciding::Accept(end, _)) => {
            end.close();
            link.send(T_ACCEPT, &[])?;
            Ok((None, Some(proxy)))
        }
        Ok(restore::client::recv::Deciding::Reject(start, back)) => {
            // Son copias locales: se sueltan.
            back.state.release(Sys);
            back.service.release(Sys);
            drop(proxy);
            link.send(T_REJECT, &[])?;
            Ok((Some(start), None))
        }
        Err(_) => Err(Error::BadMessage),
    }
}

/// Destino, servicio aceptado: entrega cada pedido que llega por la red.
fn proxy_in(link: &mut Link, mut srv: Proxy) -> Result<u64, Error> {
    let mut buf = [0u8; DATA_MAX];
    let mut out = [0u8; DATA_MAX];
    let mut n = 0;
    loop {
        let (kind, len) = link.recv(&mut buf)?;
        match kind {
            T_REQ => {
                let inc: counter::Inc = decode_wire(&buf[1..len])?;
                let count = match srv.call(inc) {
                    Ok(counter::client::recv::Waiting::Count(c, m)) => {
                        srv = c;
                        m
                    }
                    Err(_) => return Err(Error::PeerClosed),
                };
                let len = encode_wire(count, &mut out)?;
                link.send(T_RESP, &[&out[..len]])?;
                n += 1;
            }
            T_CLOSE => return Ok(n),
            _ => return Err(Error::BadMessage),
        }
    }
}

fn run_origin(
    link: &mut Link,
    mut ctrl: Chan<Sys, restore::server::Start>,
    authenticated: bool,
) -> Result<(), Error> {
    for attempt in 0..MAX_ATTEMPTS {
        let (deciding, r) = match ctrl.recv() {
            Ok(restore::server::recv::Start::Restore(d, r)) => (d, r),
            Err(RecvError::Failed(Error::PeerClosed)) if authenticated => return Ok(()),
            Err(RecvError::Failed(e)) => return Err(e),
            Err(_) => return Err(Error::BadMessage),
        };
        if !authenticated {
            println!("enlace: sin enlace autenticado; rechazo la migración");
            ctrl = deciding.send(restore::Reject {
                state: r.state,
                service: r.service,
            })?;
            continue;
        }
        match migrate_out(link, deciding, r, attempt)? {
            Some(start) => ctrl = start,
            None => return Ok(()),
        }
    }
    Err(Error::TimedOut)
}

fn run_dest(link: &mut Link, mut ctrl: DestCtrl) -> Result<(), Error> {
    let mut buf = [0u8; DATA_MAX];
    for attempt in 0..MAX_ATTEMPTS {
        let (kind, len) = link.recv(&mut buf)?;
        if kind != T_MIGRATE {
            return Err(Error::BadMessage);
        }
        println!("enlace: llegó un estado autenticado de la otra máquina");
        match migrate_in(link, ctrl, &buf[1..len], attempt)? {
            (_, Some(srv)) => {
                let n = proxy_in(link, srv)?;
                println!(
                    "enlace: el cliente remoto cerró; atendí {} pedidos ({} tramas descartadas)",
                    n, link.dropped
                );
                return Ok(());
            }
            (Some(start), None) => ctrl = start,
            (None, None) => return Err(Error::BadMessage),
        }
    }
    Err(Error::TimedOut)
}

fn run() -> Result<(), Error> {
    let net = Net::open(Sys, bootstrap(1))?;
    let config = mem_map(bootstrap(2), CONFIG_VADDR, Rights::READ)?;
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
        net: Some(net),
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
    // El canal de control: servidor de `restore` del lado de origen (el
    // enlace hace de versión nueva) y cliente del lado de destino.
    match role {
        Role::Origin => run_origin(&mut link, Chan::open(Sys, bootstrap(3))?, ok)?,
        Role::Dest if ok => run_dest(&mut link, Chan::open(Sys, bootstrap(3))?)?,
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
