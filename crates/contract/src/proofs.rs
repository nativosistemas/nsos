//! Pruebas de Kani (ver `verification/kani.md`).

use core::cell::Cell;

use nsos_abi::cap::CapInfo;
use nsos_abi::msg::{MsgHeader, MAX_CAPS, MAX_DATA};
use nsos_abi::{CapHandle, Error, Message, ObjectType, Rights};

use crate::protocols::{echo, restore, store_obj};
use crate::{check, Bytes, Chan, Dir, Msg, Receives, RecvError, Transition, Transport};

/// Transporte cuyo `recv` entrega un mensaje cualquiera y cuyo `cap_info`
/// contesta cualquier cosa. Cuenta los `cap_drop`.
#[derive(Clone, Copy)]
struct Any<'a> {
    msg: &'a Message,
    drops: &'a Cell<usize>,
    /// Lo que contestó `cap_info` para cada handle: `(handle, tipo, derechos)`.
    seen: &'a Cell<[(u32, u32, u32); 4]>,
    n_seen: &'a Cell<usize>,
}

impl Any<'_> {
    /// Lo que se contestó para `h` (`None` si no se preguntó o falló).
    fn answered(&self, h: CapHandle) -> Option<(u32, u32)> {
        let seen = self.seen.get();
        seen[..self.n_seen.get().min(4)]
            .iter()
            .rev()
            .find(|e| e.0 == h.0)
            .map(|e| (e.1, e.2))
    }
}

impl Transport for Any<'_> {
    fn send(self, _: CapHandle, _: &Message) -> Result<(), Error> {
        Ok(())
    }
    fn try_send(self, _: CapHandle, _: &Message) -> Result<(), Error> {
        match kani::any::<u8>() % 3 {
            0 => Ok(()),
            1 => Err(Error::WouldBlock),
            _ => Err(Error::PeerClosed),
        }
    }
    fn recv(self, _: CapHandle, m: &mut Message, _: bool) -> Result<(), Error> {
        *m = *self.msg;
        Ok(())
    }
    fn cap_info(self, h: CapHandle) -> Result<CapInfo, Error> {
        if kani::any() {
            let (object_type, rights): (u32, u32) = (kani::any(), kani::any());
            let i = self.n_seen.get();
            if i < 4 {
                let mut seen = self.seen.get();
                seen[i] = (h.0, object_type, rights);
                self.seen.set(seen);
                self.n_seen.set(i + 1);
            }
            Ok(CapInfo {
                object_type,
                rights,
                koid: 0,
            })
        } else {
            Err(Error::BadHandle)
        }
    }
    fn cap_drop(self, _: CapHandle) {
        self.drops.set(self.drops.get() + 1);
    }
    fn channel_create(self) -> Result<(CapHandle, CapHandle), Error> {
        Err(Error::NoMemory)
    }
    fn cap_dup(self, _: CapHandle, _: Rights) -> Result<CapHandle, Error> {
        Err(Error::NoMemory)
    }
    fn call(self, h: CapHandle, m: &mut Message) -> Result<(), Error> {
        self.recv(h, m, false)
    }
}

/// Un mensaje cualquiera que cumple las reglas de `Message::validate` (lo
/// que el kernel deja pasar), con datos y handles cualesquiera.
fn any_valid_message() -> Message {
    let m = Message {
        header: MsgHeader {
            label: kani::any(),
            data_len: kani::any(),
            cap_count: kani::any(),
            flags: 0,
            reserved: 0,
        },
        caps: [
            CapHandle(kani::any()),
            CapHandle(kani::any()),
            CapHandle(kani::any()),
            CapHandle(kani::any()),
        ],
        data: kani::any(),
    };
    kani::assume(m.validate().is_ok());
    m
}

/// Con cualquier mensaje que llegue al servidor de `echo` en `Ready`: sin
/// pánicos; si se acepta, la etiqueta es de un mensaje que el cliente puede
/// mandar ahí y se usaron exactamente sus datos y capacidades; si no, el
/// canal vuelve en el mismo estado y se soltaron **todas** las capacidades
/// recibidas (ninguna queda ocupando un slot).
#[kani::proof]
#[kani::unwind(230)]
fn decode_any() {
    let msg = any_valid_message();
    let (drops, seen, n_seen) = (Cell::new(0), Cell::new([(0, 0, 0); 4]), Cell::new(0));
    let t = Any {
        msg: &msg,
        drops: &drops,
        seen: &seen,
        n_seen: &n_seen,
    };
    let s: Chan<_, echo::server::Ready> = Chan::__from_parts(t, CapHandle(1));
    let n = msg.cap_slice().len();
    match s.recv() {
        Ok(b) => {
            assert_eq!(drops.get(), 0);
            match b {
                echo::server::recv::Ready::Echo(c, m) => {
                    assert_eq!(msg.header.label, 1);
                    assert_eq!(n, 0);
                    assert_eq!(m.data.as_slice(), msg.payload());
                    core::mem::forget(c);
                }
                echo::server::recv::Ready::NewChannel(c, m) => {
                    assert_eq!(msg.header.label, 100);
                    assert_eq!(n, 1);
                    assert!(msg.payload().is_empty());
                    assert_eq!(m.side.handle(), msg.caps[0]);
                    core::mem::forget(c);
                }
                echo::server::recv::Ready::Bye(c, _) => {
                    assert_eq!(msg.header.label, 999);
                    assert!(n == 0 && msg.payload().is_empty());
                    core::mem::forget(c);
                }
            }
        }
        Err(RecvError::Violation { chan, label }) => {
            assert_eq!(label, msg.header.label);
            assert_eq!(drops.get(), n);
            assert_eq!(chan.handle(), CapHandle(1));
            core::mem::forget(chan);
        }
        Err(_) => unreachable!(),
    };
}

/// Lo que se codifica se decodifica igual y el kernel lo acepta
/// (`validate`): `Echo` con cualquier texto y `NewChannel` con cualquier
/// handle válido. Nada se trunca.
#[kani::proof]
#[kani::unwind(230)]
fn roundtrip_echo() {
    let len: usize = kani::any();
    kani::assume(len <= MAX_DATA);
    let buf: [u8; MAX_DATA] = kani::any();
    let data = Bytes::<224>::new(&buf[..len]).unwrap();
    let mut m = Message::default();
    echo::Echo { data }.encode(&mut m);
    assert!(m.validate().is_ok());
    let (drops, seen, n_seen) = (Cell::new(0), Cell::new([(0, 0, 0); 4]), Cell::new(0));
    let t = Any {
        msg: &m,
        drops: &drops,
        seen: &seen,
        n_seen: &n_seen,
    };
    let back = echo::Echo::decode(t, &m).unwrap();
    assert_eq!(back.data.as_slice(), &buf[..len]);

    // El servidor en `Ready` lo reconoce como `Echo`.
    assert!(matches!(
        <echo::server::Ready as Receives>::decode(t, CapHandle(1), &m),
        Some(echo::server::recv::Ready::Echo(..))
    ));
}

#[kani::proof]
fn roundtrip_endpoint() {
    let h = CapHandle(kani::any());
    kani::assume(h.is_valid());
    let mut m = Message::default();
    echo::NewChannel {
        side: crate::Endpoint::from_raw(h),
    }
    .encode(&mut m);
    assert!(m.validate().is_ok());
    assert_eq!(m.cap_slice(), &[h]);
    assert_eq!(m.header.label, 100);
    let _ = (MAX_CAPS, ObjectType::Channel, Rights::ALL);
}

fn any_transition() -> Transition {
    let src: u16 = kani::any();
    let dst: u16 = kani::any();
    let label: u64 = kani::any();
    kani::assume(src < 4 && dst < 4 && label < 3);
    Transition {
        src,
        dir: if kani::any() {
            Dir::ClientToServer
        } else {
            Dir::ServerToClient
        },
        label,
        dst,
    }
}

/// Con cualquier tabla de hasta tres transiciones: `check` no entra en
/// pánico, y si acepta, ningún estado mezcla direcciones, cada etiqueta
/// lleva a un solo estado y la cota no supera la cantidad de transiciones.
/// (Que el veredicto sea el del modelo lo compara `contract_model`.)
#[kani::proof]
#[kani::unwind(6)]
fn check_any() {
    let table = [any_transition(), any_transition(), any_transition()];
    let n: usize = kani::any();
    kani::assume(n <= 3);
    let t = &table[..n];
    if let Ok(k) = check(t) {
        assert!(k as usize <= n);
        for a in t {
            for b in t {
                if a.src == b.src {
                    assert_eq!(a.dir, b.dir);
                    if a.label == b.label {
                        assert_eq!(a.dst, b.dst);
                    }
                }
            }
        }
    }
}

/// Con cualquier mensaje que llegue a la versión nueva en `Start` y
/// cualquier respuesta de `cap_info`: sin pánicos; si acepta `Restore`, el
/// estado es un objeto `Memory` con `READ` y **sin** `WRITE` y el extremo
/// es un canal, y se usaron exactamente las dos capacidades; si no, se
/// soltaron todas y el canal sigue en `Start`. Ninguna versión nueva
/// tipada recibe el estado escribible.
#[kani::proof]
#[kani::unwind(230)]
fn restore_decode_any() {
    let msg = any_valid_message();
    let (drops, seen, n_seen) = (Cell::new(0), Cell::new([(0, 0, 0); 4]), Cell::new(0));
    let t = Any {
        msg: &msg,
        drops: &drops,
        seen: &seen,
        n_seen: &n_seen,
    };
    let s: Chan<_, restore::server::Start> = Chan::__from_parts(t, CapHandle(1));
    let n = msg.cap_slice().len();
    match s.recv() {
        Ok(restore::server::recv::Start::Restore(c, r)) => {
            assert_eq!(msg.header.label, nsos_abi::msg::labels::UPGRADE_RESTORE);
            assert_eq!(n, 2);
            assert_eq!(drops.get(), 0);
            assert!(r.mode.as_slice().len() <= 64);
            let (ty, rights) = t.answered(r.state.handle()).unwrap();
            assert_eq!(ty, ObjectType::Memory as u32);
            assert!(Rights(rights).contains(Rights::READ));
            assert!(!Rights(rights).contains(Rights::WRITE));
            let (ty, _) = t.answered(r.service.handle()).unwrap();
            assert_eq!(ty, ObjectType::Channel as u32);
            core::mem::forget(c);
        }
        Err(RecvError::Violation { chan, .. }) => {
            assert_eq!(drops.get(), n);
            core::mem::forget(chan);
        }
        Err(_) => unreachable!(),
    };
}

/// El cliente del almacén esperando la respuesta a `Read`, con cualquier
/// mensaje y cualquier respuesta de `cap_info`: sin pánicos; si acepta, es
/// `Data` o `FailMem` y trae exactamente una capacidad, un objeto `Memory`
/// con `READ` y `WRITE` (el búfer de vuelta); si no, se soltaron todas y el
/// cliente sigue esperando en `Reading`.
#[kani::proof]
#[kani::unwind(230)]
fn store_reading_decode_any() {
    let msg = any_valid_message();
    let (drops, seen, n_seen) = (Cell::new(0), Cell::new([(0, 0, 0); 4]), Cell::new(0));
    let t = Any {
        msg: &msg,
        drops: &drops,
        seen: &seen,
        n_seen: &n_seen,
    };
    let c: Chan<_, store_obj::client::Reading> = Chan::__from_parts(t, CapHandle(1));
    let n = msg.cap_slice().len();
    let check_buf = |h: CapHandle| {
        assert_eq!(n, 1);
        assert_eq!(h, msg.caps[0]);
        let (ty, rights) = t.answered(h).unwrap();
        assert_eq!(ty, ObjectType::Memory as u32);
        assert!(Rights(rights).contains(Rights::READ | Rights::WRITE));
    };
    match c.recv() {
        Ok(store_obj::client::recv::Reading::Data(idle, d)) => {
            assert_eq!(msg.header.label, 15);
            check_buf(d.buf.handle());
            core::mem::forget(idle);
        }
        Ok(store_obj::client::recv::Reading::FailMem(idle, f)) => {
            assert_eq!(msg.header.label, 99);
            check_buf(f.buf.handle());
            core::mem::forget(idle);
        }
        Err(RecvError::Violation { chan, .. }) => {
            assert_eq!(drops.get(), n);
            core::mem::forget(chan);
        }
        Err(_) => unreachable!(),
    };
}

/// `try_send` con cualquier resultado de la syscall: si se mandó, no se
/// soltó nada (las capacidades viajan); si la cola estaba llena, el canal
/// vuelve en el mismo estado y se soltó la capacidad del mensaje; si falló,
/// se soltó también, y el canal se cerró. Nunca queda una capacidad
/// ocupando un slot.
#[kani::proof]
#[kani::unwind(230)]
fn try_send_any() {
    let msg = Message::default();
    let (drops, seen, n_seen) = (Cell::new(0), Cell::new([(0, 0, 0); 4]), Cell::new(0));
    let t = Any {
        msg: &msg,
        drops: &drops,
        seen: &seen,
        n_seen: &n_seen,
    };
    let h = CapHandle(kani::any());
    kani::assume(h.is_valid());
    let c: Chan<_, echo::client::Ready> = Chan::__from_parts(t, CapHandle(1));
    let m = echo::NewChannel {
        side: crate::Endpoint::from_raw(h),
    };
    match c.try_send(m) {
        Ok(next) => {
            assert_eq!(drops.get(), 0);
            core::mem::forget(next);
        }
        Err(crate::TrySendError::Full(same)) => {
            assert_eq!(drops.get(), 1);
            assert_eq!(same.handle(), CapHandle(1));
            core::mem::forget(same);
        }
        Err(crate::TrySendError::Failed(e)) => {
            // La capacidad del mensaje y el canal mismo, que se cerró.
            assert_eq!(e, Error::PeerClosed);
            assert_eq!(drops.get(), 2);
        }
    };
}
