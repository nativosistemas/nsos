//! Tests en el host con un kernel simulado.

extern crate std;

use core::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::vec::Vec;

use nsos_abi::cap::CapInfo;
use nsos_abi::{CapHandle, Error, Message, ObjectType, Rights};

use crate::protocols::{counter, echo, handover, note, restore, store_obj, store_root};
use crate::{
    check, labels_distinct_per_state, Bytes, Chan, CheckError, Dir, Mem, Msg, RecvError,
    Transition, Transport, RO, RW,
};

/// Un objeto del kernel simulado.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Obj {
    /// Extremo `n`; su peer es `n ^ 1`.
    End(usize),
    /// Cualquier otra cosa (para probar capacidades del tipo equivocado).
    Mem,
}

/// Un mensaje en una cola, con las capacidades que viajan en él.
type Queued = (Message, Vec<(Obj, Rights)>);

/// Kernel simulado: una sola cell con todas las capacidades. Mover una
/// capacidad por un canal la saca de la tabla y le da un handle nuevo al
/// recibirla, como el kernel real.
#[derive(Default)]
struct Kernel {
    caps: BTreeMap<u32, (Obj, Rights)>,
    next: u32,
    queues: Vec<VecDeque<Queued>>,
}

impl Kernel {
    fn insert(&mut self, o: Obj, r: Rights) -> CapHandle {
        self.next += 1;
        self.caps.insert(self.next, (o, r));
        CapHandle(self.next)
    }

    fn live(&self, h: CapHandle) -> bool {
        self.caps.contains_key(&h.0)
    }

    fn endpoint_alive(&self, e: usize) -> bool {
        self.caps.values().any(|(o, _)| *o == Obj::End(e))
            || self.queues.iter().any(|q| {
                q.iter()
                    .any(|(_, cs)| cs.iter().any(|(o, _)| *o == Obj::End(e)))
            })
    }
}

const ALL: Rights = Rights::ALL;

#[derive(Clone, Copy)]
struct Sim<'a>(&'a RefCell<Kernel>);

impl Transport for Sim<'_> {
    fn send(self, h: CapHandle, m: &Message) -> Result<(), Error> {
        let mut k = self.0.borrow_mut();
        let Some(&(Obj::End(e), _)) = k.caps.get(&h.0) else {
            return Err(Error::BadHandle);
        };
        m.validate()?;
        if !k.endpoint_alive(e ^ 1) {
            return Err(Error::PeerClosed);
        }
        let mut moved = Vec::new();
        for c in m.cap_slice() {
            moved.push(*k.caps.get(&c.0).ok_or(Error::BadHandle)?);
        }
        for c in m.cap_slice() {
            k.caps.remove(&c.0);
        }
        k.queues[e ^ 1].push_back((*m, moved));
        Ok(())
    }

    fn recv(self, h: CapHandle, m: &mut Message, nonblock: bool) -> Result<(), Error> {
        let mut k = self.0.borrow_mut();
        let Some(&(Obj::End(e), _)) = k.caps.get(&h.0) else {
            return Err(Error::BadHandle);
        };
        let Some((msg, caps)) = k.queues[e].pop_front() else {
            return Err(if !k.endpoint_alive(e ^ 1) {
                Error::PeerClosed
            } else if nonblock {
                Error::WouldBlock
            } else {
                // un solo hilo: bloquear sería colgarse
                panic!("recv bloqueante con la cola vacía")
            });
        };
        *m = msg;
        for (i, (o, r)) in caps.into_iter().enumerate() {
            m.caps[i] = k.insert(o, r);
        }
        Ok(())
    }

    fn cap_info(self, h: CapHandle) -> Result<CapInfo, Error> {
        let k = self.0.borrow();
        let &(o, r) = k.caps.get(&h.0).ok_or(Error::BadHandle)?;
        let object_type = match o {
            Obj::End(_) => ObjectType::Channel,
            Obj::Mem => ObjectType::Memory,
        } as u32;
        Ok(CapInfo {
            object_type,
            rights: r.0,
            koid: 0,
        })
    }

    fn cap_drop(self, h: CapHandle) {
        self.0.borrow_mut().caps.remove(&h.0);
    }

    fn channel_create(self) -> Result<(CapHandle, CapHandle), Error> {
        let mut k = self.0.borrow_mut();
        let e = k.queues.len();
        k.queues.push(VecDeque::new());
        k.queues.push(VecDeque::new());
        Ok((k.insert(Obj::End(e), ALL), k.insert(Obj::End(e + 1), ALL)))
    }

    fn cap_dup(self, h: CapHandle, mask: Rights) -> Result<CapHandle, Error> {
        let mut k = self.0.borrow_mut();
        let &(o, r) = k.caps.get(&h.0).ok_or(Error::BadHandle)?;
        let r = r.attenuate(mask).ok_or(Error::AccessDenied)?;
        Ok(k.insert(o, r))
    }

    /// Un solo hilo: la respuesta tiene que estar ya en la cola (el kernel
    /// también devuelve el próximo mensaje que llega a `h`).
    fn call(self, h: CapHandle, m: &mut Message) -> Result<(), Error> {
        self.send(h, m)?;
        self.recv(h, m, false)
    }
}

fn bytes(b: &[u8]) -> Bytes<224> {
    Bytes::new(b).unwrap()
}

/// Un mensaje armado a mano, como lo mandaría una cell sin contrato.
fn raw(t: Sim<'_>, h: CapHandle, label: u64, data: &[u8], caps: &[CapHandle]) {
    let mut m = Message::new(label, data).unwrap();
    for &c in caps {
        m.push_cap(c).unwrap();
    }
    t.send(h, &m).unwrap();
}

#[test]
fn echo_roundtrip() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (c, s) = echo::pair(t).unwrap();
    let c = c
        .send(echo::Echo {
            data: bytes(b"hola"),
        })
        .unwrap();
    let s = match s.recv().unwrap() {
        echo::server::recv::Ready::Echo(s, m) => {
            assert_eq!(m.data.as_slice(), b"hola");
            s.send(echo::EchoReply { data: m.data }).unwrap()
        }
        other => panic!("esperaba Echo: {other:?}"),
    };
    let c = match c.recv().unwrap() {
        echo::client::recv::Waiting::EchoReply(c, m) => {
            assert_eq!(m.data.as_slice(), b"hola");
            c
        }
    };
    let _end = c.send(echo::Bye {}).unwrap();
    assert!(matches!(
        s.recv().unwrap(),
        echo::server::recv::Ready::Bye(_, echo::Bye {})
    ));
}

#[test]
fn wire_format_is_the_raw_one() {
    // Una cell sin contrato (como `bench`) habla el mismo protocolo.
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (a, b) = t.channel_create().unwrap();
    let s: Chan<_, echo::server::Ready> = Chan::open(t, b).unwrap();
    raw(t, a, 1, b"x", &[]);
    let s = match s.recv().unwrap() {
        echo::server::recv::Ready::Echo(s, m) => s.send(echo::EchoReply { data: m.data }).unwrap(),
        other => panic!("{other:?}"),
    };
    let mut m = Message::default();
    t.recv(a, &mut m, true).unwrap();
    assert_eq!((m.header.label, m.payload()), (2, &b"x"[..]));
    drop(s);
}

#[test]
fn endpoint_moves() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (c, s) = echo::pair(t).unwrap();
    let (nc, ns) = note::pair(t).unwrap();
    let old = ns.handle();
    let c = c
        .send(echo::NewChannel {
            side: ns.into_endpoint(),
        })
        .unwrap();
    assert!(!k.borrow().live(old), "el emisor perdió el extremo");
    let echo::server::recv::Ready::NewChannel(s, m) = s.recv().unwrap() else {
        panic!("esperaba NewChannel");
    };
    let side = Chan::attach(t, m.side);
    let nc = nc
        .send(note::Note {
            text: bytes(b"nota"),
        })
        .unwrap();
    let note::server::recv::Start::Note(side, n) = side.recv().unwrap();
    assert_eq!(n.text.as_slice(), b"nota");
    side.send(note::Ack { text: bytes(b"ok") }).unwrap().close();
    let note::client::recv::Waiting::Ack(_end, a) = nc.recv().unwrap();
    assert_eq!(a.text.as_slice(), b"ok");
    let _s = s.send(echo::Adopted {}).unwrap();
    let echo::client::recv::Adopting::Adopted(_c, echo::Adopted {}) = c.recv().unwrap();
}

#[test]
fn violations_are_dropped_and_state_kept() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (a, b) = t.channel_create().unwrap();
    let mut s: Chan<_, echo::server::Ready> = Chan::open(t, b).unwrap();

    // 1. Una etiqueta del contrato pero de la otra dirección.
    raw(t, a, 2, b"falso", &[]);
    // 2. Una etiqueta desconocida.
    raw(t, a, 777, b"", &[]);
    // 3. `Echo` con una capacidad de más: tiene que soltarla.
    let (x, y) = t.channel_create().unwrap();
    raw(t, a, 1, b"con cap", &[y]);
    // 4. `NewChannel` con una capacidad que no es un canal.
    let mem = k.borrow_mut().insert(Obj::Mem, ALL);
    raw(t, a, 100, b"", &[mem]);
    // 5. `NewChannel` sin capacidad.
    raw(t, a, 100, b"", &[]);
    // 6. `Bye` con datos de más.
    raw(t, a, 999, b"sobra", &[]);

    let mut seen = Vec::new();
    for _ in 0..6 {
        match s.recv() {
            Err(RecvError::Violation { chan, label }) => {
                seen.push(label);
                s = chan;
            }
            other => panic!("esperaba una violación: {other:?}"),
        }
    }
    assert_eq!(seen, [2, 777, 1, 100, 100, 999]);
    // Ninguna capacidad recibida quedó ocupando un slot: solo `a`, `b`, `x`.
    assert_eq!(k.borrow().caps.len(), 3);
    assert!(!k.borrow().endpoint_alive(1 + 2), "el extremo `y` se soltó");
    let _ = x;

    // Y el servidor sigue en `Ready`.
    raw(t, a, 1, b"bien", &[]);
    assert!(matches!(
        s.recv().unwrap(),
        echo::server::recv::Ready::Echo(_, ref m) if m.data.as_slice() == b"bien"
    ));
}

#[test]
fn try_recv_gives_the_channel_back() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (_c, s) = echo::pair(t).unwrap();
    let Err(RecvError::WouldBlock(s)) = s.try_recv() else {
        panic!("esperaba WouldBlock");
    };
    drop(s);
}

#[test]
fn drop_closes() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (c, s) = echo::pair(t).unwrap();
    drop(c);
    assert!(matches!(
        s.recv(),
        Err(RecvError::Failed(Error::PeerClosed))
    ));
    assert!(k.borrow().caps.is_empty());
}

#[test]
fn open_checks_the_type() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let mem = k.borrow_mut().insert(Obj::Mem, ALL);
    assert_eq!(
        Chan::<_, echo::server::Ready>::open(t, mem).unwrap_err(),
        Error::WrongType
    );
    let (a, _b) = t.channel_create().unwrap();
    k.borrow_mut().caps.get_mut(&a.0).unwrap().1 = Rights::RECV;
    assert!(Chan::<_, echo::server::Ready>::open(t, a).is_err());
}

#[test]
fn protocol_tables() {
    assert_eq!(echo::BOUND, 1);
    assert_eq!(note::BOUND, 1);
    assert_eq!(echo::START, 0);
    assert_eq!(echo::TRANSITIONS.len(), 5);
}

fn tr(src: u16, ab: bool, label: u64, dst: u16) -> Transition {
    let dir = if ab {
        Dir::ClientToServer
    } else {
        Dir::ServerToClient
    };
    Transition {
        src,
        dir,
        label,
        dst,
    }
}

#[test]
fn check_rejects_what_the_model_rejects() {
    // Los contraejemplos de `Nsos/Contract.lean`.
    let mixed = [tr(0, true, 1, 1), tr(0, false, 2, 2)];
    let nondet = [
        tr(0, true, 1, 1),
        tr(0, true, 1, 2),
        tr(1, false, 5, 0),
        tr(2, true, 7, 0),
    ];
    let flood = [tr(0, true, 1, 0)];
    let echo_old = [
        tr(0, true, 1, 1),
        tr(0, true, 100, 0),
        tr(0, true, 999, 2),
        tr(1, false, 2, 0),
    ];
    assert_eq!(check(&mixed), Err(CheckError::Mixed));
    assert_eq!(check(&nondet), Err(CheckError::Nondeterministic));
    assert_eq!(check(&flood), Err(CheckError::Unbounded));
    assert_eq!(check(&echo_old), Err(CheckError::Unbounded));
    assert_eq!(check(&[]), Ok(0));
    // Tres mensajes seguidos del cliente.
    let three = [tr(0, true, 1, 1), tr(1, true, 2, 2), tr(2, true, 3, 3)];
    assert_eq!(check(&three), Ok(3));
    assert_eq!(check(&[tr(64, true, 1, 0)]), Err(CheckError::TooManyStates));
    // La misma etiqueta en dos estados, sí; en el mismo, no.
    assert!(labels_distinct_per_state(&[
        tr(0, true, 1, 1),
        tr(1, false, 1, 0)
    ]));
    assert!(!labels_distinct_per_state(&[
        tr(0, true, 1, 1),
        tr(0, true, 1, 2)
    ]));
}

fn mem(k: &RefCell<Kernel>, r: Rights) -> CapHandle {
    k.borrow_mut().insert(Obj::Mem, r)
}

fn rights(k: &RefCell<Kernel>, h: CapHandle) -> Rights {
    k.borrow().caps[&h.0].1
}

#[test]
fn read_only_is_read_only() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let h = mem(&k, ALL);
    let rw: Mem<RW> = Mem::open(t, h).unwrap();
    let ro = rw.read_only(t).unwrap();
    assert!(!rights(&k, ro.handle()).contains(Rights::WRITE));
    assert!(rights(&k, ro.handle()).contains(Rights::READ | Rights::GRANT));
    // Un escribible no pasa por solo lectura, ni al revés.
    assert_eq!(Mem::<RO>::open(t, h).unwrap_err(), Error::WrongType);
    assert_eq!(
        Mem::<RW>::open(t, ro.handle()).unwrap_err(),
        Error::WrongType
    );
    // Un canal no es memoria.
    let (a, _b) = t.channel_create().unwrap();
    assert!(Mem::<RO>::open(t, a).is_err());
}

#[test]
fn restore_rejects_a_writable_state() {
    // Alguien sin contrato le manda a la versión nueva el estado
    // escribible: la versión nueva lo descarta sin quedarse con nada.
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (sup, new) = t.channel_create().unwrap();
    let new: Chan<_, restore::server::Start> = Chan::open(t, new).unwrap();
    let state = mem(&k, ALL);
    let (_c, s) = counter::pair(t).unwrap();
    raw(
        t,
        sup,
        <restore::Restore as Msg>::LABEL,
        b"",
        &[state, s.into_raw()],
    );
    let before = k.borrow().caps.len();
    let Err(RecvError::Violation { chan, .. }) = new.recv() else {
        panic!("esperaba una violación");
    };
    // Las dos capacidades viajaban en la cola: al recibirlas se insertaron
    // y el decodificador las soltó. No queda ninguna nueva ni la memoria.
    assert_eq!(k.borrow().caps.len(), before, "soltó las dos capacidades");
    assert!(!k.borrow().caps.values().any(|(o, _)| *o == Obj::Mem));
    drop(chan);
}

/// La actualización completa, con las tres puntas en un hilo: rechazo con
/// rollback y después aceptación con commit.
#[test]
fn upgrade_reject_then_accept() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (cli, srv) = counter::pair(t).unwrap();
    let (sup_old, old) = handover::pair(t).unwrap();
    let state: Mem<RW> = Mem::open(t, mem(&k, ALL)).unwrap();

    // El cliente deja un pedido encolado: viaja con el extremo.
    let cli = match cli.send(counter::Inc {}) {
        Ok(c) => c,
        Err(e) => panic!("{e:?}"),
    };

    let mut sup_old = sup_old;
    let mut old = old;
    let mut state = state;
    let mut srv = srv;
    for accept in [false, true] {
        // supervisor → vieja: Prepare; vieja → supervisor: State.
        let sup_h = sup_old.send(handover::Prepare {}).unwrap();
        let handover::server::recv::Serving::Prepare(old_h, _) = old.recv().unwrap();
        let old_d = old_h
            .send(handover::State {
                state,
                service: srv.into_endpoint(),
            })
            .unwrap();
        let handover::client::recv::Handing::State(sup_d, st) = sup_h.recv().unwrap();

        // supervisor → nueva: Restore con una copia de solo lectura.
        let (sup_new, new) = restore::pair(t).unwrap();
        let ro = st.state.read_only(t).unwrap();
        let backup = st.service.backup(t).unwrap();
        let sup_new = sup_new
            .send(restore::Restore {
                state: ro,
                service: st.service,
                mode: Bytes::new(b"").unwrap(),
            })
            .unwrap();
        let restore::server::recv::Start::Restore(new, r) = new.recv().unwrap();
        assert!(!rights(&k, r.state.handle()).contains(Rights::WRITE));

        if accept {
            new.send(restore::Accept {}).unwrap().close();
            let restore::client::recv::Deciding::Accept(_end, _) = sup_new.recv().unwrap() else {
                panic!("esperaba Accept");
            };
            backup.release(t);
            st.state.release(t);
            sup_d.send(handover::Commit {}).unwrap().close();
            let handover::server::recv::Deciding::Commit(_end, _) = old_d.recv().unwrap() else {
                panic!("esperaba Commit");
            };
            r.state.release(t);
            // La nueva atiende el pedido que estaba encolado.
            let srv: Chan<_, counter::server::Serving> = Chan::attach(t, r.service);
            let counter::server::recv::Serving::Inc(w, _) = srv.recv().unwrap();
            let _srv = w
                .send(counter::Count {
                    count: 1,
                    version: 2,
                })
                .unwrap();
            let counter::client::recv::Waiting::Count(_cli, c) = cli.recv().unwrap();
            assert_eq!((c.count, c.version), (1, 2));
            break;
        }
        let _start = new
            .send(restore::Reject {
                state: r.state,
                service: r.service,
            })
            .unwrap();
        let restore::client::recv::Deciding::Reject(_s, back) = sup_new.recv().unwrap() else {
            panic!("esperaba Reject");
        };
        back.state.release(t);
        backup.release(t);
        sup_old = sup_d
            .send(handover::Rollback {
                state: st.state,
                service: back.service,
            })
            .unwrap();
        let handover::server::recv::Deciding::Rollback(o, rb) = old_d.recv().unwrap() else {
            panic!("esperaba Rollback");
        };
        old = o;
        state = rb.state;
        srv = Chan::attach(t, rb.service);
    }
}

#[test]
fn call_is_send_then_recv() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (cli, srv) = t.channel_create().unwrap();
    let cli: Chan<_, counter::client::Serving> = Chan::open(t, cli).unwrap();
    // La respuesta ya está en la cola (ver `Sim::call`).
    let mut d = [0u8; 12];
    d[..8].copy_from_slice(&7u64.to_le_bytes());
    d[8..].copy_from_slice(&1u32.to_le_bytes());
    raw(t, srv, 2, &d, &[]);
    let counter::client::recv::Waiting::Count(_c, m) = cli.call(counter::Inc {}).unwrap();
    assert_eq!((m.count, m.version), (7, 1));
    // Y el pedido llegó.
    let mut m = Message::default();
    t.recv(srv, &mut m, true).unwrap();
    assert_eq!(m.header.label, 1);
}

/// El formato en el cable es el de `nsos_store::proto`: `OPENED` es
/// `[versión u64 | largo u32 | escribe u8]` con el handle adjunto.
#[test]
fn store_wire_format() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (cli, srv) = t.channel_create().unwrap();
    let cli: Chan<_, store_root::client::Ready> = Chan::open(t, cli).unwrap();
    let opening = cli
        .send(store_root::Open {
            flags: 1,
            name: Bytes::new(b"contador").unwrap(),
        })
        .unwrap();
    let mut m = Message::default();
    t.recv(srv, &mut m, true).unwrap();
    assert_eq!((m.header.label, m.payload()), (10, &b"\x01contador"[..]));
    // El almacén contesta a mano, como hoy.
    let (_mine, theirs) = t.channel_create().unwrap();
    let mut p = [0u8; 13];
    p[..8].copy_from_slice(&3u64.to_le_bytes());
    p[8..12].copy_from_slice(&16u32.to_le_bytes());
    p[12] = 1;
    raw(t, srv, 11, &p, &[theirs]);
    let store_root::client::recv::Opening::Opened(_ready, o) = opening.recv().unwrap() else {
        panic!("esperaba Opened");
    };
    assert_eq!((o.version, o.len, o.write), (3, 16, true));
    let h: Chan<_, store_obj::client::Idle> = Chan::attach(t, o.handle);
    drop(h);
}

/// Un pedido con búfer: el error lo devuelve; una respuesta sin búfer no es
/// del contrato y el cliente sigue esperando en `Reading`.
#[test]
fn store_buffer_always_comes_back() {
    let k = RefCell::new(Kernel::default());
    let t = Sim(&k);
    let (cli, srv) = t.channel_create().unwrap();
    let cli: Chan<_, store_obj::client::Idle> = Chan::open(t, cli).unwrap();
    let buf: Mem<RW> = Mem::open(t, mem(&k, ALL)).unwrap();
    let reading = cli.send(store_obj::Read { buf }).unwrap();
    let mut m = Message::default();
    t.recv(srv, &mut m, true).unwrap();
    let back = m.caps[0];
    // Un error sin el búfer: violación (el cliente no lo pierde de vista).
    raw(t, srv, 99, &[0; 12], &[]);
    let Err(RecvError::Violation { chan: reading, .. }) = reading.recv() else {
        panic!("esperaba una violación");
    };
    // El error con el búfer: `FailMem`.
    raw(t, srv, 99, &[0; 12], &[back]);
    let store_obj::client::recv::Reading::FailMem(_idle, f) = reading.recv().unwrap() else {
        panic!("esperaba FailMem");
    };
    assert!(Mem::<RW>::open(t, f.buf.handle()).is_ok());
}
