//! Lógica de la syscall `wait` (hito 12) que no toca hardware: qué señales
//! puede emitir cada tipo de objeto, cuándo un ítem es válido, qué señales
//! están activas en un canal o una cell, y el plazo.
//!
//! El protocolo de espera (marcar bloqueado, registrar y mirar cada ítem,
//! ceder la CPU) está modelado en `verification/lean/Nsos/Wait.lean`; esto
//! es la parte que el modelo abstrae como "una señal es un booleano". Las
//! pruebas de Kani están al final (ver `verification/kani.md`).

use nsos_abi::cap::signals;
use nsos_abi::syscall::WaitItem;
use nsos_abi::{Error, ObjectType};

/// Ítems máximos por llamada a `wait`.
pub const WAIT_MAX: usize = 16;

/// Señales que puede emitir un objeto de tipo `t`. Pedir otra es un error
/// del llamador (`InvalidArgs`): nunca se activaría.
pub const fn allowed(t: ObjectType) -> u32 {
    match t {
        ObjectType::Channel => signals::READABLE | signals::WRITABLE | signals::PEER_CLOSED,
        ObjectType::Cell => signals::CELL_EXITED,
        ObjectType::Irq => signals::IRQ_PENDING,
        ObjectType::Memory | ObjectType::IoPorts | ObjectType::Image => 0,
    }
}

/// Valida un ítem de `wait` contra el tipo del objeto al que apunta su
/// handle: `reserved` en 0, al menos una señal pedida y todas emitibles por
/// ese tipo. (Que el handle tenga `WAIT` lo comprueba la CSpace.)
pub fn check_item(item: &WaitItem, t: ObjectType) -> Result<(), Error> {
    if item.reserved != 0 || item.wanted == 0 || item.wanted & !allowed(t) != 0 {
        return Err(Error::InvalidArgs);
    }
    Ok(())
}

/// Estado de un extremo de canal visto por quien tiene ese extremo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelState {
    /// Mensajes en la cola de este extremo (los que le llegan).
    pub queued: usize,
    /// El otro extremo ya no tiene dueños.
    pub peer_closed: bool,
    /// Mensajes en la cola del otro extremo (adonde envía este).
    pub peer_queued: usize,
    /// Tope de cada cola.
    pub limit: usize,
}

/// Señales activas de un extremo de canal. Coinciden con lo que harían
/// `recv` y `send` sin `NONBLOCK`:
/// * `READABLE`: hay un mensaje (recv no bloquearía con un mensaje).
/// * `PEER_CLOSED`: el otro extremo cerró (recv con la cola vacía y send
///   fallan con `PeerClosed`).
/// * `WRITABLE`: el otro extremo está abierto y su cola tiene lugar (send
///   no bloquearía ni fallaría por la cola).
pub fn channel_signals(s: ChannelState) -> u32 {
    let mut r = 0;
    if s.queued > 0 {
        r |= signals::READABLE;
    }
    if s.peer_closed {
        r |= signals::PEER_CLOSED;
    } else if s.peer_queued < s.limit {
        r |= signals::WRITABLE;
    }
    r
}

/// Señales activas de una cell supervisada.
pub fn cell_signals(exited: bool) -> u32 {
    if exited {
        signals::CELL_EXITED
    } else {
        0
    }
}

/// Plazo de una espera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deadline {
    /// `timeout == 0`: mirar y volver enseguida.
    Poll,
    /// `timeout == WAIT_FOREVER`: sin plazo.
    Never,
    /// Instante absoluto (ns de `clock_monotonic`) en que vence.
    At(u64),
}

/// Plazo para un `timeout` relativo dado en `now`. Nunca desborda: un plazo
/// que no entra en 64 bits es "sin plazo".
pub fn deadline(now: u64, timeout: u64) -> Deadline {
    match timeout {
        0 => Deadline::Poll,
        nsos_abi::syscall::WAIT_FOREVER => Deadline::Never,
        t => match now.checked_add(t) {
            Some(d) => Deadline::At(d),
            None => Deadline::Never,
        },
    }
}

/// ¿Ya venció el plazo en `now`?
pub fn expired(d: Deadline, now: u64) -> bool {
    match d {
        Deadline::Poll => true,
        Deadline::Never => false,
        Deadline::At(t) => now >= t,
    }
}

/// Un ítem está listo si alguna señal pedida está activa.
pub fn ready(item: &WaitItem) -> bool {
    item.wanted & item.observed != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use signals::*;

    fn item(wanted: u32) -> WaitItem {
        WaitItem {
            handle: 1,
            wanted,
            observed: 0,
            reserved: 0,
        }
    }

    #[test]
    fn items() {
        assert!(check_item(&item(READABLE | PEER_CLOSED), ObjectType::Channel).is_ok());
        assert!(check_item(&item(CELL_EXITED), ObjectType::Cell).is_ok());
        assert!(check_item(&item(IRQ_PENDING), ObjectType::Irq).is_ok());
        assert!(check_item(&item(0), ObjectType::Channel).is_err());
        assert!(check_item(&item(CELL_EXITED), ObjectType::Channel).is_err());
        assert!(check_item(&item(READABLE), ObjectType::Memory).is_err());
        assert!(check_item(&item(1 << 9), ObjectType::Channel).is_err());
        let mut it = item(READABLE);
        it.reserved = 1;
        assert!(check_item(&it, ObjectType::Channel).is_err());
    }

    #[test]
    fn channel() {
        let s = ChannelState {
            queued: 0,
            peer_closed: false,
            peer_queued: 0,
            limit: 64,
        };
        assert_eq!(channel_signals(s), WRITABLE);
        assert_eq!(
            channel_signals(ChannelState { queued: 3, ..s }),
            READABLE | WRITABLE
        );
        assert_eq!(
            channel_signals(ChannelState {
                peer_queued: 64,
                ..s
            }),
            0
        );
        assert_eq!(
            channel_signals(ChannelState {
                peer_closed: true,
                ..s
            }),
            PEER_CLOSED
        );
    }

    #[test]
    fn deadlines() {
        assert_eq!(deadline(5, 0), Deadline::Poll);
        assert_eq!(deadline(5, u64::MAX), Deadline::Never);
        assert_eq!(deadline(5, 10), Deadline::At(15));
        assert_eq!(deadline(u64::MAX - 1, 10), Deadline::Never);
        assert!(expired(Deadline::Poll, 0));
        assert!(!expired(Deadline::Never, u64::MAX));
        assert!(!expired(Deadline::At(15), 14));
        assert!(expired(Deadline::At(15), 15));
    }
}

/// Pruebas de Kani (ver `verification/kani.md`).
#[cfg(kani)]
mod proofs {
    use super::*;

    fn any_type() -> ObjectType {
        let v: u32 = kani::any();
        kani::assume(v >= 1 && v <= 6);
        ObjectType::from_raw(v).unwrap()
    }

    /// Un ítem aceptado pide al menos una señal, todas emitibles por el tipo
    /// del objeto, y no trae nada en `reserved`.
    #[kani::proof]
    fn check_item_any() {
        let it = WaitItem {
            handle: kani::any(),
            wanted: kani::any(),
            observed: kani::any(),
            reserved: kani::any(),
        };
        let t = any_type();
        if check_item(&it, t).is_ok() {
            assert!(it.reserved == 0);
            assert!(it.wanted != 0);
            assert!(it.wanted & !allowed(t) == 0);
            // Memoria, puertos e imágenes no se pueden esperar.
            assert!(matches!(
                t,
                ObjectType::Channel | ObjectType::Cell | ObjectType::Irq
            ));
        }
    }

    /// Las señales de un canal son las que el tipo puede emitir y dicen
    /// exactamente lo que harían `recv` y `send`.
    #[kani::proof]
    fn channel_signals_any() {
        let s = ChannelState {
            queued: kani::any(),
            peer_closed: kani::any(),
            peer_queued: kani::any(),
            limit: kani::any(),
        };
        let r = channel_signals(s);
        assert!(r & !allowed(ObjectType::Channel) == 0);
        assert_eq!(r & signals::READABLE != 0, s.queued > 0);
        assert_eq!(r & signals::PEER_CLOSED != 0, s.peer_closed);
        assert_eq!(
            r & signals::WRITABLE != 0,
            !s.peer_closed && s.peer_queued < s.limit
        );
    }

    /// El plazo nunca desborda ni vence antes de tiempo: con `At(d)`,
    /// `d = now + timeout` exacto, y no vence antes de `d`.
    #[kani::proof]
    fn deadline_any() {
        let now: u64 = kani::any();
        let timeout: u64 = kani::any();
        let later: u64 = kani::any();
        match deadline(now, timeout) {
            Deadline::Poll => assert!(timeout == 0),
            Deadline::Never => {
                assert!(timeout == u64::MAX || now.checked_add(timeout).is_none())
            }
            Deadline::At(d) => {
                assert!(d as u128 == now as u128 + timeout as u128);
                if later < d {
                    assert!(!expired(Deadline::At(d), later));
                }
            }
        }
    }
}
