//! Extremos de canal con el estado del protocolo en el tipo.

use core::marker::PhantomData;

use nsos_abi::cap::CapInfo;
use nsos_abi::{CapHandle, Error, Message, ObjectType, Rights};

use crate::wire::Msg;

/// Las syscalls que usa un canal tipado. En una cell la implementa
/// `nsos_rt::Sys`; en los tests del host, un kernel simulado.
pub trait Transport: Copy {
    /// `channel_send` (bloqueante).
    fn send(self, h: CapHandle, m: &Message) -> Result<(), Error>;
    /// `channel_recv`; con `nonblock`, `WouldBlock` si la cola está vacía.
    fn recv(self, h: CapHandle, m: &mut Message, nonblock: bool) -> Result<(), Error>;
    /// `cap_info`.
    fn cap_info(self, h: CapHandle) -> Result<CapInfo, Error>;
    /// `cap_drop` (los errores se ignoran: el handle deja de usarse igual).
    fn cap_drop(self, h: CapHandle);
    /// `channel_create`.
    fn channel_create(self) -> Result<(CapHandle, CapHandle), Error>;
}

/// Un estado de un lado de un contrato (lo genera [`contract!`](crate::contract)).
pub trait State: Sized {
    /// Número del estado en la tabla del contrato.
    const ID: u16;
}

/// En este estado se puede mandar `M`, y se pasa a `Next`.
pub trait Sends<M: Msg>: State {
    /// Estado después de mandar `M`.
    type Next: State;
}

/// En este estado se recibe: `Branch` tiene una variante por mensaje
/// posible, cada una con el canal en el estado siguiente.
pub trait Receives: State {
    /// Lo que devuelve `recv`.
    type Branch<T: Transport>;
    /// Decodifica `m` según este estado. `None` si no tiene transición o
    /// su forma no es la del contrato; en ese caso no toma nada de `m`.
    fn decode<T: Transport>(t: T, h: CapHandle, m: &Message) -> Option<Self::Branch<T>>;
}

/// Extremo de canal en el estado `S`, sin transporte: es lo que viaja
/// dentro de un mensaje. Se convierte en [`Chan`] con [`Chan::attach`].
///
/// No suelta el handle al destruirse (no tiene con qué): un `Endpoint`
/// descartado ocupa su slot hasta que la cell termina.
#[must_use = "un Endpoint descartado deja el handle ocupado; usá Chan::attach"]
#[derive(Debug)]
pub struct Endpoint<S: State> {
    h: CapHandle,
    _s: PhantomData<S>,
}

impl<S: State> Endpoint<S> {
    /// Envuelve un handle sin comprobarlo (lo usan el decodificador, que ya
    /// lo comprobó, y [`Chan::into_endpoint`]).
    pub(crate) fn from_raw(h: CapHandle) -> Endpoint<S> {
        Endpoint { h, _s: PhantomData }
    }

    /// El handle (para `wait` o para depurar).
    pub fn handle(&self) -> CapHandle {
        self.h
    }

    pub(crate) fn into_raw(self) -> CapHandle {
        self.h
    }
}

/// Derechos que tiene que tener un extremo para seguir un contrato.
pub(crate) const CHAN_RIGHTS: Rights = Rights(Rights::SEND.0 | Rights::RECV.0);

/// `true` si `h` es un extremo de canal con `SEND` y `RECV`.
pub(crate) fn is_channel<T: Transport>(t: T, h: CapHandle) -> bool {
    match t.cap_info(h) {
        Ok(i) => {
            i.object_type == ObjectType::Channel as u32 && Rights(i.rights).contains(CHAN_RIGHTS)
        }
        Err(_) => false,
    }
}

/// Extremo de canal en el estado `S` del protocolo, con su transporte.
///
/// Al destruirse suelta el handle, y la otra punta ve `PeerClosed`.
#[must_use = "un Chan descartado cierra el canal"]
pub struct Chan<T: Transport, S: State> {
    t: T,
    h: CapHandle,
    _s: PhantomData<S>,
}

/// Error de [`Chan::recv`] y [`Chan::try_recv`].
pub enum RecvError<C> {
    /// Llegó un mensaje que el contrato no permite en este estado, o con
    /// otra forma. Se descartó (y se soltaron sus capacidades); el canal
    /// vuelve en el mismo estado.
    Violation {
        /// El canal, en el mismo estado.
        chan: C,
        /// Etiqueta del mensaje descartado.
        label: u64,
    },
    /// Cola vacía (solo `try_recv`); el canal vuelve.
    WouldBlock(C),
    /// Falló la syscall (por ejemplo `PeerClosed`); el canal se cerró.
    Failed(Error),
}

impl<C> core::fmt::Debug for RecvError<C> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RecvError::Violation { label, .. } => write!(f, "Violation({label})"),
            RecvError::WouldBlock(_) => f.write_str("WouldBlock"),
            RecvError::Failed(e) => write!(f, "Failed({e:?})"),
        }
    }
}

impl<T: Transport, S: State> Chan<T, S> {
    #[doc(hidden)]
    pub fn __from_parts(t: T, h: CapHandle) -> Chan<T, S> {
        Chan {
            t,
            h,
            _s: PhantomData,
        }
    }

    /// Toma un handle crudo (por ejemplo una capacidad de arranque) como
    /// extremo en el estado `S`. Comprueba que sea un canal con `SEND` y
    /// `RECV`; que la otra punta siga el mismo contrato no se puede
    /// comprobar: lo garantiza quien repartió el canal.
    pub fn open(t: T, h: CapHandle) -> Result<Chan<T, S>, Error> {
        if is_channel(t, h) {
            Ok(Chan::__from_parts(t, h))
        } else {
            Err(Error::WrongType)
        }
    }

    /// Un extremo recibido en un mensaje, con este transporte.
    pub fn attach(t: T, ep: Endpoint<S>) -> Chan<T, S> {
        Chan::__from_parts(t, ep.into_raw())
    }

    /// Lo convierte en [`Endpoint`] para mandarlo en un mensaje.
    pub fn into_endpoint(self) -> Endpoint<S> {
        let h = self.h;
        core::mem::forget(self);
        Endpoint::from_raw(h)
    }

    /// El handle (para `wait`).
    pub fn handle(&self) -> CapHandle {
        self.h
    }

    /// Cierra el canal (lo mismo que destruirlo).
    pub fn close(self) {}

    /// Manda `msg` y pasa al estado siguiente. Si la syscall falla, el
    /// canal se cierra y las capacidades del mensaje se sueltan (el kernel
    /// no las movió).
    pub fn send<M: Msg>(self, msg: M) -> Result<Chan<T, S::Next>, Error>
    where
        S: Sends<M>,
    {
        let mut m = Message::default();
        msg.encode(&mut m);
        match self.t.send(self.h, &m) {
            Ok(()) => {
                let (t, h) = (self.t, self.h);
                core::mem::forget(self);
                Ok(Chan::__from_parts(t, h))
            }
            Err(e) => {
                crate::discard(self.t, &m);
                Err(e)
            }
        }
    }
}

impl<T: Transport, S: Receives> Chan<T, S> {
    /// Espera un mensaje y lo decodifica según el estado.
    pub fn recv(self) -> Result<S::Branch<T>, RecvError<Self>> {
        self.recv_with(false)
    }

    /// Como `recv`, pero sin bloquear: con la cola vacía devuelve
    /// [`RecvError::WouldBlock`] con el canal.
    pub fn try_recv(self) -> Result<S::Branch<T>, RecvError<Self>> {
        self.recv_with(true)
    }

    fn recv_with(self, nonblock: bool) -> Result<S::Branch<T>, RecvError<Self>> {
        let mut m = Message::default();
        match self.t.recv(self.h, &mut m, nonblock) {
            Ok(()) => {}
            Err(Error::WouldBlock) if nonblock => return Err(RecvError::WouldBlock(self)),
            Err(e) => return Err(RecvError::Failed(e)),
        }
        match S::decode(self.t, self.h, &m) {
            Some(b) => {
                core::mem::forget(self);
                Ok(b)
            }
            None => {
                crate::discard(self.t, &m);
                Err(RecvError::Violation {
                    label: m.header.label,
                    chan: self,
                })
            }
        }
    }
}

impl<T: Transport, S: State> Drop for Chan<T, S> {
    fn drop(&mut self) {
        self.t.cap_drop(self.h);
    }
}

impl<T: Transport, S: State> core::fmt::Debug for Chan<T, S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Chan({:?}, estado {})", self.h, S::ID)
    }
}
