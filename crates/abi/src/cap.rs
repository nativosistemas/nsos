//! Modelo de capacidades: handles, tipos de objeto, derechos y señales.
//!
//! Una cell no puede nombrar ningún recurso del kernel salvo a través de un
//! `CapHandle`, que es un índice en su propia tabla de capacidades (CSpace).
//! Los handles son locales a la cell: el mismo número en dos cells no tiene
//! relación alguna. No hay espacio de nombres global ni punteros compartidos.

use core::fmt;
use core::ops::{BitAnd, BitOr, Not};

/// Bits del handle dedicados al índice de slot.
pub const SLOT_BITS: u32 = 20;
/// Número máximo de slots por CSpace.
pub const MAX_SLOTS: u32 = 1 << SLOT_BITS;
/// Bits de generación (anti-reuso de handles).
pub const GEN_BITS: u32 = 32 - SLOT_BITS;
/// Cantidad de generaciones distintas que puede tener un slot.
pub const GEN_LIMIT: u32 = 1 << GEN_BITS;
const SLOT_MASK: u32 = MAX_SLOTS - 1;
const GEN_MASK: u32 = (1 << GEN_BITS) - 1;

/// Referencia opaca a una capacidad dentro del CSpace de una cell.
///
/// Formato (32 bits): `[ generación:12 | slot:20 ]`. El slot 0 nunca se usa,
/// así que `CapHandle(0)` es siempre inválido. La generación se incrementa
/// cada vez que un slot se libera, de modo que un handle viejo (use-after-free
/// de capacidades) se detecta y devuelve `Error::BadHandle`. Un slot que agotó
/// sus `GEN_LIMIT` generaciones se retira para siempre: la generación nunca da
/// la vuelta, así que un handle viejo no puede volver a ser válido.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CapHandle(pub u32);

impl CapHandle {
    /// Handle nulo / inválido.
    pub const INVALID: CapHandle = CapHandle(0);

    /// Construye un handle a partir de slot y generación. `None` si el slot es
    /// 0 o se sale de rango.
    pub const fn new(slot: u32, generation: u32) -> Option<CapHandle> {
        if slot == 0 || slot > SLOT_MASK {
            return None;
        }
        Some(CapHandle(((generation & GEN_MASK) << SLOT_BITS) | slot))
    }

    /// Índice de slot en el CSpace.
    pub const fn slot(self) -> u32 {
        self.0 & SLOT_MASK
    }

    /// Generación del slot cuando se emitió el handle.
    pub const fn generation(self) -> u32 {
        self.0 >> SLOT_BITS
    }

    /// `true` si el handle tiene forma válida (no garantiza que exista).
    pub const fn is_valid(self) -> bool {
        self.slot() != 0
    }

    /// Siguiente generación (con vuelta) para un slot recién liberado.
    pub const fn next_generation(generation: u32) -> u32 {
        (generation + 1) & GEN_MASK
    }
}

impl fmt::Debug for CapHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cap(slot={}, gen={})", self.slot(), self.generation())
    }
}

/// Tipo de objeto del kernel al que apunta una capacidad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ObjectType {
    /// Un extremo de un canal IPC bidireccional.
    Channel = 1,
    /// Un objeto de memoria (conjunto de marcos físicos) mapeable.
    Memory = 2,
    /// Control de una cell hija (matar, esperar, actualizar).
    Cell = 3,
    /// Una línea de interrupción entregada a un driver en user-space.
    Irq = 4,
    /// Un rango de puertos de E/S x86 entregado a un driver.
    IoPorts = 5,
    /// Imagen ejecutable (ELF validado) a partir de la cual crear cells.
    Image = 6,
}

impl ObjectType {
    /// Decodifica desde el valor numérico del ABI.
    pub const fn from_raw(v: u32) -> Option<ObjectType> {
        Some(match v {
            1 => ObjectType::Channel,
            2 => ObjectType::Memory,
            3 => ObjectType::Cell,
            4 => ObjectType::Irq,
            5 => ObjectType::IoPorts,
            6 => ObjectType::Image,
            _ => return None,
        })
    }

    /// Derechos que tienen sentido para este tipo de objeto. Cualquier otro
    /// bit en una capacidad de este tipo es un bug del kernel.
    pub const fn valid_rights(self) -> Rights {
        let common = Rights::GRANT.0 | Rights::DUPLICATE.0 | Rights::WAIT.0;
        Rights(
            common
                | match self {
                    ObjectType::Channel => Rights::SEND.0 | Rights::RECV.0,
                    ObjectType::Memory => Rights::READ.0 | Rights::WRITE.0 | Rights::EXEC.0,
                    ObjectType::Cell => Rights::CONTROL.0,
                    ObjectType::Irq => Rights::CONTROL.0,
                    ObjectType::IoPorts => Rights::READ.0 | Rights::WRITE.0,
                    ObjectType::Image => Rights::EXEC.0,
                },
        )
    }
}

/// Conjunto de derechos de una capacidad (bitflags).
///
/// Los derechos solo pueden reducirse (atenuación), nunca ampliarse: no existe
/// ninguna syscall que agregue bits a una capacidad existente.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct Rights(pub u32);

impl Rights {
    /// Ningún derecho.
    pub const NONE: Rights = Rights(0);
    /// Enviar mensajes por un canal.
    pub const SEND: Rights = Rights(1 << 0);
    /// Recibir mensajes de un canal.
    pub const RECV: Rights = Rights(1 << 1);
    /// Leer (mapear lectura / leer puertos).
    pub const READ: Rights = Rights(1 << 2);
    /// Escribir (mapear escritura / escribir puertos).
    pub const WRITE: Rights = Rights(1 << 3);
    /// Ejecutar (mapear ejecutable / instanciar imagen).
    pub const EXEC: Rights = Rights(1 << 4);
    /// Transferir la capacidad a otra cell por IPC.
    pub const GRANT: Rights = Rights(1 << 5);
    /// Duplicarla (con derechos iguales o menores).
    pub const DUPLICATE: Rights = Rights(1 << 6);
    /// Operaciones de control (matar/actualizar cell, ack de IRQ).
    pub const CONTROL: Rights = Rights(1 << 7);
    /// Esperar señales del objeto con `wait`.
    pub const WAIT: Rights = Rights(1 << 8);

    /// Todos los bits definidos.
    pub const ALL: Rights = Rights((1 << 9) - 1);

    /// Bits crudos.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Construye desde bits crudos; `None` si hay bits no definidos.
    pub const fn from_bits(bits: u32) -> Option<Rights> {
        if bits & !Rights::ALL.0 != 0 {
            None
        } else {
            Some(Rights(bits))
        }
    }

    /// `true` si contiene todos los bits de `other`.
    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }

    /// `true` si `self` es subconjunto de `other`.
    pub const fn is_subset_of(self, other: Rights) -> bool {
        other.contains(self)
    }

    /// Atenúa derechos: devuelve `self ∩ mask` si `self` permite duplicar.
    /// Es la única forma de derivar una capacidad nueva de otra.
    pub const fn attenuate(self, mask: Rights) -> Option<Rights> {
        if !self.contains(Rights::DUPLICATE) {
            return None;
        }
        Some(Rights(self.0 & mask.0))
    }
}

impl BitOr for Rights {
    type Output = Rights;
    fn bitor(self, rhs: Rights) -> Rights {
        Rights(self.0 | rhs.0)
    }
}

impl BitAnd for Rights {
    type Output = Rights;
    fn bitand(self, rhs: Rights) -> Rights {
        Rights(self.0 & rhs.0)
    }
}

impl Not for Rights {
    type Output = Rights;
    fn not(self) -> Rights {
        Rights(!self.0 & Rights::ALL.0)
    }
}

impl fmt::Debug for Rights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMES: [&str; 9] = [
            "SEND",
            "RECV",
            "READ",
            "WRITE",
            "EXEC",
            "GRANT",
            "DUPLICATE",
            "CONTROL",
            "WAIT",
        ];
        write!(f, "Rights(")?;
        let mut first = true;
        for (i, name) in NAMES.iter().enumerate() {
            if self.0 & (1 << i) != 0 {
                if !first {
                    write!(f, "|")?;
                }
                write!(f, "{name}")?;
                first = false;
            }
        }
        write!(f, ")")
    }
}

/// Señales observables con la syscall `wait` (bitflags en un `u32`).
pub mod signals {
    /// Hay al menos un mensaje para recibir.
    pub const READABLE: u32 = 1 << 0;
    /// Hay espacio en la cola del peer para enviar.
    pub const WRITABLE: u32 = 1 << 1;
    /// El otro extremo se cerró o su cell murió.
    pub const PEER_CLOSED: u32 = 1 << 2;
    /// La cell controlada terminó (normalmente o por fallo).
    pub const CELL_EXITED: u32 = 1 << 3;
    /// Hay una interrupción pendiente de atender.
    pub const IRQ_PENDING: u32 = 1 << 4;
}

/// Información que devuelve `cap_info`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CapInfo {
    /// Tipo de objeto (`ObjectType` como `u32`).
    pub object_type: u32,
    /// Derechos (`Rights` como `u32`).
    pub rights: u32,
    /// Identificador estable del objeto (solo para depuración; no da acceso).
    pub koid: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_roundtrip() {
        let h = CapHandle::new(17, 5).unwrap();
        assert_eq!(h.slot(), 17);
        assert_eq!(h.generation(), 5);
        assert!(h.is_valid());
    }

    #[test]
    fn handle_bounds() {
        assert!(CapHandle::new(0, 3).is_none());
        assert!(CapHandle::new(MAX_SLOTS, 0).is_none());
        assert!(CapHandle::new(MAX_SLOTS - 1, 0).is_some());
        assert!(!CapHandle::INVALID.is_valid());
    }

    #[test]
    fn generation_wraps() {
        let max = (1 << GEN_BITS) - 1;
        assert_eq!(CapHandle::next_generation(max), 0);
        let h = CapHandle::new(1, max + 1).unwrap(); // se trunca
        assert_eq!(h.generation(), 0);
    }

    #[test]
    fn attenuation_only_removes() {
        let r = Rights::SEND | Rights::RECV | Rights::DUPLICATE;
        let a = r.attenuate(Rights::SEND | Rights::CONTROL).unwrap();
        assert_eq!(a, Rights::SEND);
        assert!(a.is_subset_of(r));
        // Sin DUPLICATE no se puede derivar.
        assert_eq!(Rights::SEND.attenuate(Rights::ALL), None);
    }

    #[test]
    fn exhaustive_attenuation_is_subset() {
        for bits in 0..=Rights::ALL.0 {
            let r = Rights(bits);
            for mask in [0, Rights::ALL.0, 0b1010_1010, 0b0101_0101] {
                if let Some(a) = r.attenuate(Rights(mask)) {
                    assert!(a.is_subset_of(r));
                }
            }
        }
    }

    #[test]
    fn rights_from_bits_rejects_unknown() {
        assert!(Rights::from_bits(1 << 20).is_none());
        assert_eq!(Rights::from_bits(3), Some(Rights::SEND | Rights::RECV));
    }

    #[test]
    fn valid_rights_per_type() {
        assert!(ObjectType::Channel.valid_rights().contains(Rights::SEND));
        assert!(!ObjectType::Channel.valid_rights().contains(Rights::EXEC));
        assert!(!ObjectType::Memory.valid_rights().contains(Rights::SEND));
        for v in 1..=6 {
            let t = ObjectType::from_raw(v).unwrap();
            assert_eq!(t as u32, v);
        }
        assert!(ObjectType::from_raw(0).is_none());
    }
}
