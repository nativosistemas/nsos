//! Lógica de las líneas de interrupción para drivers (hito 12) que no toca
//! hardware: el registro de líneas reclamadas (cada una tiene a lo sumo un
//! dueño), la entrada del IOAPIC que programa el kernel en x86_64 y la tabla
//! `interrupt-map` de PCI del device tree en ARM64.
//!
//! El protocolo (enmascarar al entregar, `irq_ack` del dueño) está modelado
//! en `verification/lean/Nsos/Irq.lean`; las pruebas de Kani están al final
//! (ver `verification/kani.md`).

use crate::fdt::read_cells;

/// Líneas que puede manejar el registro (GSI del IOAPIC o INTID del GIC).
pub const MAX_LINES: u32 = 1024;

/// Líneas reclamadas: una línea se entrega a un solo driver
/// (`IrqModel.claim_exclusive`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IrqClaims {
    bits: [u64; (MAX_LINES / 64) as usize],
}

impl IrqClaims {
    /// Registro vacío.
    pub const fn new() -> IrqClaims {
        IrqClaims {
            bits: [0; (MAX_LINES / 64) as usize],
        }
    }

    /// ¿Está reclamada?
    pub fn is_claimed(&self, line: u32) -> bool {
        line < MAX_LINES && self.bits[(line / 64) as usize] & (1 << (line % 64)) != 0
    }

    /// Reclama `line`. `false` si ya tiene dueño o está fuera de rango.
    pub fn claim(&mut self, line: u32) -> bool {
        if line >= MAX_LINES || self.is_claimed(line) {
            return false;
        }
        self.bits[(line / 64) as usize] |= 1 << (line % 64);
        true
    }

    /// Suelta `line` (no hace nada si no estaba reclamada).
    pub fn release(&mut self, line: u32) {
        if line < MAX_LINES {
            self.bits[(line / 64) as usize] &= !(1 << (line % 64));
        }
    }
}

/// Entrada de redirección del IOAPIC para una interrupción PCI: entrega
/// fija al APIC `dest` con `vector`, **por nivel** y activa en alto (como la
/// describe QEMU), enmascarada si `masked`. Devuelve (palabra baja, alta).
pub fn ioapic_entry(vector: u8, dest: u8, masked: bool) -> (u32, u32) {
    const LEVEL: u32 = 1 << 15;
    const MASKED: u32 = 1 << 16;
    let mut lo = vector as u32 | LEVEL;
    if masked {
        lo |= MASKED;
    }
    (lo, (dest as u32) << 24)
}

/// Entradas máximas de `interrupt-map` que se guardan (QEMU `virt` usa 16).
pub const INTX_MAX: usize = 32;

/// Una entrada de `interrupt-map` ya traducida a un INTID del GIC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntxEntry {
    /// Primera celda de la dirección PCI del hijo (bus, dispositivo, función).
    pub addr_hi: u32,
    /// Pin INTx (1 = INTA ... 4 = INTD).
    pub pin: u32,
    /// INTID del GIC (una SPI: 32 a 1019).
    pub intid: u32,
}

/// `interrupt-map` de un controlador PCI hacia el GIC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntxMap {
    /// `interrupt-map-mask` sobre `addr_hi` y sobre el pin.
    pub mask_hi: u32,
    /// Ídem sobre el pin.
    pub mask_pin: u32,
    /// Entradas válidas (las primeras `len`).
    pub entries: [IntxEntry; INTX_MAX],
    /// Cuántas.
    pub len: usize,
}

/// Formato de las entradas de `interrupt-map`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntxLayout {
    /// `#address-cells` del controlador PCI (3).
    pub child_addr: u32,
    /// `#interrupt-cells` del controlador PCI (1: el pin).
    pub child_int: u32,
    /// `#address-cells` del GIC.
    pub parent_addr: u32,
    /// `#interrupt-cells` del GIC (3: tipo, número, flags).
    pub parent_int: u32,
}

/// Primer INTID de las SPI del GIC.
const SPI_BASE: u32 = 32;
/// Último INTID válido.
const INTID_MAX: u32 = 1019;

/// Lee `interrupt-map` (`map`) y `interrupt-map-mask` (`mask`). Solo acepta
/// entradas hacia el GIC (`gic_phandle`) que sean SPI; las demás se ignoran.
/// `None` si el formato no es el esperado.
pub fn parse_intx_map(map: &[u8], mask: &[u8], l: IntxLayout, gic_phandle: u32) -> Option<IntxMap> {
    if l.child_addr != 3 || l.child_int != 1 || l.parent_int != 3 || l.parent_addr > 3 {
        return None;
    }
    let cells = (l.child_addr + l.child_int + 1 + l.parent_addr + l.parent_int) as usize;
    let at =
        |b: &[u8], i: usize| -> Option<u32> { read_cells(b.get(4 * i..)?, 1).map(|v| v as u32) };
    let mut m = IntxMap {
        mask_hi: at(mask, 0)?,
        mask_pin: at(mask, 3)?,
        entries: [IntxEntry::default(); INTX_MAX],
        len: 0,
    };
    for e in map.chunks_exact(4 * cells) {
        if m.len == INTX_MAX {
            break;
        }
        let parent = 5 + l.parent_addr as usize;
        if at(e, 4)? != gic_phandle || at(e, parent)? != 0 {
            continue;
        }
        let spi = at(e, parent + 1)?;
        if spi > INTID_MAX - SPI_BASE {
            continue;
        }
        m.entries[m.len] = IntxEntry {
            addr_hi: at(e, 0)?,
            pin: at(e, 3)?,
            intid: SPI_BASE + spi,
        };
        m.len += 1;
    }
    Some(m)
}

impl IntxMap {
    /// INTID de la línea INTx `pin` (1 a 4) de una función del bus raíz.
    pub fn lookup(&self, bus: u8, dev: u8, func: u8, pin: u8) -> Option<u32> {
        if !(1..=4).contains(&pin) || dev >= 32 || func >= 8 {
            return None;
        }
        let hi = (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8;
        let (hi, pin) = (hi & self.mask_hi, pin as u32 & self.mask_pin);
        self.entries[..self.len.min(INTX_MAX)]
            .iter()
            .find(|e| e.addr_hi & self.mask_hi == hi && e.pin & self.mask_pin == pin)
            .map(|e| e.intid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims() {
        let mut c = IrqClaims::new();
        assert!(c.claim(11));
        assert!(!c.claim(11));
        assert!(c.claim(12));
        c.release(11);
        assert!(c.claim(11));
        assert!(!c.claim(MAX_LINES));
        c.release(MAX_LINES); // no hace nada
    }

    #[test]
    fn ioapic() {
        assert_eq!(ioapic_entry(59, 0, true), (59 | 1 << 15 | 1 << 16, 0));
        assert_eq!(ioapic_entry(59, 2, false), (59 | 1 << 15, 2 << 24));
    }

    fn be(v: &[u32]) -> alloc::vec::Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    /// Como lo arma QEMU `virt`: 4 ranuras × 4 pines, SPI = 3 + (ranura +
    /// pin - 1) % 4.
    #[test]
    fn qemu_virt_swizzle() {
        let mut map = alloc::vec::Vec::new();
        for slot in 0..4u32 {
            for pin in 1..=4u32 {
                map.extend([
                    slot << 11,
                    0,
                    0,
                    pin,
                    0x8001,
                    0,
                    0,
                    0,
                    3 + (slot + pin - 1) % 4,
                    4,
                ]);
            }
        }
        let l = IntxLayout {
            child_addr: 3,
            child_int: 1,
            parent_addr: 2,
            parent_int: 3,
        };
        let m = parse_intx_map(&be(&map), &be(&[0x1800, 0, 0, 7]), l, 0x8001).unwrap();
        assert_eq!(m.len, 16);
        // Ranura 1, INTA → SPI 4 → INTID 36; ranura 5 = ranura 1 (máscara).
        assert_eq!(m.lookup(0, 1, 0, 1), Some(36));
        assert_eq!(m.lookup(0, 5, 0, 1), Some(36));
        assert_eq!(m.lookup(0, 2, 0, 2), Some(32 + 3 + 3));
        assert_eq!(m.lookup(0, 1, 0, 0), None);
        assert_eq!(m.lookup(0, 1, 0, 5), None);
        // Otro phandle: nada.
        assert_eq!(
            parse_intx_map(&be(&map), &be(&[0x1800, 0, 0, 7]), l, 9)
                .unwrap()
                .len,
            0
        );
    }
}

/// Pruebas de Kani (ver `verification/kani.md`).
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Una línea reclamada no se vuelve a entregar hasta soltarla, y
    /// reclamar o soltar una línea no cambia otra.
    #[kani::proof]
    fn claims_exclusive() {
        let mut c = IrqClaims::new();
        let a: u32 = kani::any();
        let b: u32 = kani::any();
        let first = c.claim(a);
        assert_eq!(first, a < MAX_LINES);
        assert!(!c.claim(a));
        if a != b {
            let had = c.is_claimed(b);
            assert!(!had);
            let got = c.claim(b);
            assert_eq!(got, b < MAX_LINES);
            c.release(b);
            assert_eq!(c.is_claimed(a), first);
        }
        c.release(a);
        assert!(!c.is_claimed(a));
        assert_eq!(c.claim(a), a < MAX_LINES);
    }

    /// Con cualquier `interrupt-map` de hasta dos entradas y cualquier
    /// máscara: sin pánicos, y todo INTID que sale es una SPI válida.
    #[kani::proof]
    #[kani::unwind(4)]
    fn intx_any() {
        let map: [u8; 80] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= 80);
        let mask: [u8; 16] = kani::any();
        let l = IntxLayout {
            child_addr: 3,
            child_int: 1,
            parent_addr: kani::any(),
            parent_int: 3,
        };
        kani::assume(l.parent_addr <= 2);
        if let Some(m) = parse_intx_map(&map[..n], &mask, l, kani::any()) {
            assert!(m.len <= 2);
            if let Some(id) = m.lookup(kani::any(), kani::any(), kani::any(), kani::any()) {
                assert!((SPI_BASE..=INTID_MAX).contains(&id));
            }
        }
    }
}
