//! Pruebas de Kani (ver `verification/kani.md`).

use core::cell::Cell;

use nsos_abi::cap::CapInfo;
use nsos_abi::msg::{MsgHeader, MAX_CAPS, MAX_DATA};
use nsos_abi::{CapHandle, Error, Message, ObjectType, Rights};

use crate::protocols::echo;
use crate::{check, Bytes, Chan, Dir, Msg, Receives, RecvError, Transition, Transport};

/// Transporte cuyo `recv` entrega un mensaje cualquiera y cuyo `cap_info`
/// contesta cualquier cosa. Cuenta los `cap_drop`.
#[derive(Clone, Copy)]
struct Any<'a> {
    msg: &'a Message,
    drops: &'a Cell<usize>,
}

impl Transport for Any<'_> {
    fn send(self, _: CapHandle, _: &Message) -> Result<(), Error> {
        Ok(())
    }
    fn recv(self, _: CapHandle, m: &mut Message, _: bool) -> Result<(), Error> {
        *m = *self.msg;
        Ok(())
    }
    fn cap_info(self, _: CapHandle) -> Result<CapInfo, Error> {
        if kani::any() {
            Ok(CapInfo {
                object_type: kani::any(),
                rights: kani::any(),
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
    let drops = Cell::new(0);
    let t = Any {
        msg: &msg,
        drops: &drops,
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
    let drops = Cell::new(0);
    let t = Any {
        msg: &m,
        drops: &drops,
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
