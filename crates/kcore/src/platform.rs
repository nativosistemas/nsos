//! Recursos de la plataforma que salen del device tree, y las reglas para
//! usarlos sin romper el aislamiento.
//!
//! El device tree dice dónde están los dispositivos, pero es un dato de
//! afuera. Antes de mapear una región o de entregar un rango de E/S a una
//! cell, el kernel exige:
//!
//! 1. **MMIO fuera de la RAM** ([`mmio_allowed`]): una región de
//!    dispositivo (redondeada a páginas, como la mapea el kernel) no toca
//!    ninguna región del mapa de memoria. Si no, un DTB malo podría hacer
//!    que el kernel mapee como "dispositivo" sus propias tablas de páginas o
//!    la memoria de una cell.
//! 2. **Rangos de E/S disjuntos** ([`IoAllocator`]): en ARM64 el kernel
//!    asigna los BAR de E/S de los dispositivos PCI. Cada rango sale de la
//!    ventana de E/S y no se solapa con ningún otro: dos cells driver nunca
//!    comparten registros.
//! 3. **Acceso dentro de la ventana** ([`pio_offset`]): un acceso de una
//!    cell a su rango de E/S cae entero dentro de ese rango y de la ventana
//!    mapeada.
//!
//! Las pruebas de Kani de este módulo demuestran las tres.

use crate::fdt::{read_cells, Fdt};

/// Tamaño de página con el que el kernel mapea MMIO.
pub const PAGE: u64 = 4096;

/// Región física `[base, base + len)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// Inicio.
    pub base: u64,
    /// Largo en bytes.
    pub len: u64,
}

impl Region {
    /// Fin exclusivo, si no desborda.
    pub fn end(&self) -> Option<u64> {
        self.base.checked_add(self.len)
    }

    /// La región redondeada a páginas enteras (lo que mapea el kernel).
    pub fn page_rounded(&self) -> Option<Region> {
        let base = self.base & !(PAGE - 1);
        let end = self.end()?.checked_add(PAGE - 1)? & !(PAGE - 1);
        Some(Region {
            base,
            len: end - base,
        })
    }

    /// ¿Se solapan? (una región vacía no se solapa con nada).
    pub fn overlaps(&self, o: &Region) -> bool {
        match (self.end(), o.end()) {
            (Some(a), Some(b)) => self.len > 0 && o.len > 0 && self.base < b && o.base < a,
            // Una región que desborda se trata como si llegara hasta el final.
            (None, Some(b)) => o.len > 0 && self.base < b,
            (Some(a), None) => self.len > 0 && o.base < a,
            (None, None) => true,
        }
    }
}

/// ¿Puede el kernel mapear `dev` como dispositivo? Tiene que tener largo,
/// no desbordar (ni redondeada a páginas) y no tocar ninguna región de
/// `ram` (todas las entradas del mapa de memoria que no son "reservadas").
pub fn mmio_allowed(dev: Region, ram: &[Region]) -> bool {
    if dev.len == 0 {
        return false;
    }
    let Some(r) = dev.page_rounded() else {
        return false;
    };
    ram.iter().all(|m| !r.overlaps(m))
}

/// Ventana de E/S de un controlador PCI: los puertos `[bus, bus + len)`
/// del bus están en `[cpu, cpu + len)` de la CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoWindow {
    /// Dirección física de la ventana.
    pub cpu: u64,
    /// Primer puerto (dirección de E/S del bus).
    pub bus: u64,
    /// Largo.
    pub len: u64,
}

/// Puertos de E/S que se reparten. Los primeros 4 KiB quedan sin usar (por
/// costumbre: algunos dispositivos tratan el 0 como "sin asignar").
pub const IO_FIRST: u64 = 0x1000;
/// Los puertos son de 16 bits (como en x86, y como los guarda `PortRange`).
pub const IO_LIMIT: u64 = 0x1_0000;

/// Asignador de rangos de E/S: entrega rangos alineados a su tamaño, en
/// orden creciente, sin solaparse nunca y siempre dentro de la ventana.
#[derive(Clone, Copy, Debug)]
pub struct IoAllocator {
    /// Primer puerto libre. Invariante: `IO_FIRST <= next <= IO_LIMIT`.
    next: u64,
    /// Fin de la ventana (como mucho `IO_LIMIT`).
    end: u64,
}

impl IoAllocator {
    /// Reparte los puertos `[IO_FIRST, min(len, IO_LIMIT))` de una ventana
    /// que empieza en el puerto 0 del bus.
    pub fn new(window: &IoWindow) -> Option<IoAllocator> {
        if window.bus != 0 {
            return None;
        }
        Some(IoAllocator {
            next: IO_FIRST,
            end: window.len.min(IO_LIMIT),
        })
    }

    /// Un rango de `size` puertos (potencia de dos, al menos 4), alineado a
    /// su tamaño (como exige un BAR). `None` si no queda lugar.
    pub fn alloc(&mut self, size: u64) -> Option<Region> {
        if !size.is_power_of_two() || size < 4 {
            return None;
        }
        let base = self.next.checked_add(size - 1)? & !(size - 1);
        let end = base.checked_add(size)?;
        if end > self.end {
            return None;
        }
        self.next = end;
        Some(Region { base, len: size })
    }

    /// Primer puerto todavía libre.
    pub fn next_free(&self) -> u64 {
        self.next
    }
}

/// Desplazamiento dentro de la ventana de E/S para un acceso de `width`
/// bytes en `off` de un rango `[base, base + len)` que se entregó a una
/// cell. `None` si el rango no cabe en la ventana o si el acceso no cae
/// entero en el rango.
pub fn pio_offset(base: u64, len: u64, off: u64, width: u64, window_len: u64) -> Option<u64> {
    if !matches!(width, 1 | 2 | 4) || base.checked_add(len)? > window_len {
        return None;
    }
    let end = off.checked_add(width)?;
    if end > len {
        return None;
    }
    base.checked_add(off)
}

/// Controlador PCI genérico con ECAM (`pci-host-ecam-generic`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciHost {
    /// Región ECAM (1 MiB por bus).
    pub ecam: Region,
    /// Primer bus que decodifica la región.
    pub bus_start: u8,
    /// Último bus.
    pub bus_end: u8,
    /// Ventana de E/S, si tiene.
    pub io: Option<IoWindow>,
}

impl PciHost {
    /// Dirección física del espacio de configuración de una función, si
    /// cae dentro de la región ECAM.
    pub fn config_addr(&self, bus: u8, dev: u8, func: u8) -> Option<u64> {
        if bus < self.bus_start || bus > self.bus_end || dev >= 32 || func >= 8 {
            return None;
        }
        let off = (u64::from(bus - self.bus_start) << 20)
            | (u64::from(dev) << 15)
            | (u64::from(func) << 12);
        if off.checked_add(PAGE)? > self.ecam.len {
            return None;
        }
        self.ecam.base.checked_add(off)
    }
}

/// Lo que el kernel necesita del device tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platform {
    /// UART PL011 de la consola.
    pub uart: Option<Region>,
    /// GICv2: distribuidor.
    pub gicd: Option<Region>,
    /// GICv2: interfaz de CPU.
    pub gicc: Option<Region>,
    /// Controlador PCI.
    pub pci: Option<PciHost>,
    /// Semilla aleatoria del firmware (`/chosen/rng-seed`), hasta 64 bytes.
    pub rng_seed: [u8; 64],
    /// Cuántos bytes de `rng_seed` vinieron del firmware.
    pub rng_seed_len: usize,
}

/// Controladores GICv2 que entiende el kernel.
const GIC_COMPATIBLE: [&str; 3] = ["arm,gic-400", "arm,cortex-a15-gic", "arm,cortex-a7-gic"];

impl Platform {
    /// Lee el device tree. Lo que falta o no se entiende queda en `None`;
    /// validar las regiones es trabajo de quien las mapea
    /// ([`mmio_allowed`]).
    pub fn from_fdt(fdt: &Fdt<'_>) -> Platform {
        let mut p = Platform {
            uart: None,
            gicd: None,
            gicc: None,
            pci: None,
            rng_seed: [0; 64],
            rng_seed_len: 0,
        };
        let region = |r: (u64, u64)| Region {
            base: r.0,
            len: r.1,
        };
        let chosen = fdt.find_path(b"/chosen");
        // Consola: la de `stdout-path` si es un PL011; si no, el primero.
        let stdout = chosen
            .and_then(|c| c.prop(b"stdout-path"))
            .and_then(|v| v.split(|&c| c == 0).next())
            .and_then(|path| fdt.find_path(path))
            .filter(|n| n.is_compatible("arm,pl011"));
        p.uart = stdout
            .or_else(|| fdt.find_compatible("arm,pl011"))
            .and_then(|n| n.reg(0))
            .map(region);
        if let Some(gic) = GIC_COMPATIBLE.iter().find_map(|c| fdt.find_compatible(c)) {
            p.gicd = gic.reg(0).map(region);
            p.gicc = gic.reg(1).map(region);
        }
        p.pci = fdt
            .find_compatible("pci-host-ecam-generic")
            .and_then(|n| pci_host(&n));
        if let Some(seed) = chosen.and_then(|c| c.prop(b"rng-seed")) {
            let n = seed.len().min(p.rng_seed.len());
            p.rng_seed[..n].copy_from_slice(&seed[..n]);
            p.rng_seed_len = n;
        }
        p
    }
}

/// Espacio de un rango PCI (bits 24..=25 de la primera celda).
const PCI_SPACE_IO: u32 = 1;

fn pci_host(node: &crate::fdt::Node<'_>) -> Option<PciHost> {
    let ecam = node.reg(0)?;
    let (bus_start, bus_end) = match node.prop(b"bus-range") {
        Some(v) => (read_cells(v, 1)?, read_cells(v.get(4..)?, 1)?),
        None => (0, 255),
    };
    if bus_start > bus_end || bus_end > 255 {
        return None;
    }
    let (cac, csc) = node.own_cells();
    let (pac, _) = node.parent_cells();
    if cac != 3 {
        return None;
    }
    let mut io = None;
    if let Some(ranges) = node.prop(b"ranges") {
        let entry = 4 * (cac as usize + pac as usize + csc as usize);
        for e in ranges.chunks_exact(entry) {
            let hi = read_cells(e, 1)? as u32;
            if (hi >> 24) & 3 != PCI_SPACE_IO {
                continue;
            }
            let bus = read_cells(e.get(4..)?, 2)?;
            let cpu = read_cells(e.get(12..)?, pac)?;
            let len = read_cells(e.get(12 + 4 * pac as usize..)?, csc)?;
            io = Some(IoWindow { cpu, bus, len });
            break;
        }
    }
    Some(PciHost {
        ecam: Region {
            base: ecam.0,
            len: ecam.1,
        },
        bus_start: bus_start as u8,
        bus_end: bus_end as u8,
        io,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_virt_platform() {
        let b = include_bytes!("../tests/data/qemu-virt.dtb");
        let p = Platform::from_fdt(&Fdt::new(b).unwrap());
        assert_eq!(
            p.uart,
            Some(Region {
                base: 0x0900_0000,
                len: 0x1000
            })
        );
        assert_eq!(p.gicd.map(|r| r.base), Some(0x0800_0000));
        assert_eq!(p.gicc.map(|r| r.base), Some(0x0801_0000));
        let pci = p.pci.unwrap();
        assert_eq!(pci.ecam.base, 0x40_1000_0000);
        assert_eq!((pci.bus_start, pci.bus_end), (0, 255));
        assert_eq!(
            pci.io,
            Some(IoWindow {
                cpu: 0x3eff_0000,
                bus: 0,
                len: 0x1_0000
            })
        );
        assert_eq!(pci.config_addr(0, 1, 0), Some(0x40_1000_8000));
        assert_eq!(pci.config_addr(0, 32, 0), None);
        assert_eq!(p.rng_seed_len, 32);
        // Ninguna región se solapa con la RAM de QEMU (256 MiB en 1 GiB).
        let ram = [Region {
            base: 0x4000_0000,
            len: 0x1000_0000,
        }];
        for r in [p.uart, p.gicd, p.gicc, Some(pci.ecam)] {
            assert!(mmio_allowed(r.unwrap(), &ram));
        }
    }

    #[test]
    fn mmio_rules() {
        let ram = [Region {
            base: 0x4000_0000,
            len: 0x1000,
        }];
        let r = |base, len| Region { base, len };
        assert!(mmio_allowed(r(0x3fff_f000, 0x1000), &ram));
        // Termina en el medio de la página anterior a la RAM: redondeada,
        // toca la RAM? No: llega justo hasta 0x4000_0000.
        assert!(mmio_allowed(r(0x3fff_ff00, 0x100), &ram));
        // Un byte de más ya toca la RAM.
        assert!(!mmio_allowed(r(0x3fff_ff00, 0x101), &ram));
        // Empieza en la página de la RAM aunque sea al final.
        assert!(!mmio_allowed(r(0x4000_0fff, 1), &ram));
        assert!(!mmio_allowed(r(0x5000_0000, 0), &ram));
        assert!(!mmio_allowed(r(u64::MAX - 10, 5), &ram));
        assert!(mmio_allowed(r(0x4000_1000, 0x10), &ram));
    }

    #[test]
    fn io_allocator() {
        let w = IoWindow {
            cpu: 0x3eff_0000,
            bus: 0,
            len: 0x1_0000,
        };
        let mut a = IoAllocator::new(&w).unwrap();
        assert_eq!(
            a.alloc(0x40),
            Some(Region {
                base: 0x1000,
                len: 0x40
            })
        );
        assert_eq!(
            a.alloc(0x100),
            Some(Region {
                base: 0x1100,
                len: 0x100
            })
        );
        assert_eq!(
            a.alloc(0x20),
            Some(Region {
                base: 0x1200,
                len: 0x20
            })
        );
        assert_eq!(a.alloc(3), None);
        assert_eq!(a.alloc(0x1_0000), None);
        assert!(IoAllocator::new(&IoWindow { bus: 0x1000, ..w }).is_none());
        assert_eq!(pio_offset(0x1000, 0x40, 0x3c, 4, 0x1_0000), Some(0x103c));
        assert_eq!(pio_offset(0x1000, 0x40, 0x3d, 4, 0x1_0000), None);
        assert_eq!(pio_offset(0xfffe, 0x40, 0, 4, 0x1_0000), None);
        assert_eq!(pio_offset(0x1000, 0x40, 0, 8, 0x1_0000), None);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn any_region() -> Region {
        Region {
            base: kani::any(),
            len: kani::any(),
        }
    }

    /// Si `mmio_allowed` acepta una región, ningún byte de las páginas que
    /// mapea el kernel cae en la RAM.
    #[kani::proof]
    fn mmio_never_touches_ram() {
        let dev = any_region();
        let ram = [any_region(), any_region()];
        if mmio_allowed(dev, &ram) {
            let r = dev.page_rounded().unwrap();
            // Las páginas cubren el dispositivo entero.
            assert!(r.base <= dev.base && dev.end().unwrap() <= r.end().unwrap());
            let x: u64 = kani::any();
            kani::assume(x >= r.base && x - r.base < r.len);
            for m in ram {
                assert!(!(x >= m.base && x - m.base < m.len));
            }
        }
    }

    /// Un paso del asignador de E/S, desde cualquier estado alcanzable y
    /// con cualquier tamaño: el rango entregado empieza en o después de
    /// `next`, termina justo en el `next` nuevo, está alineado a su tamaño
    /// y cabe en la ventana y en 16 bits. Por inducción, todos los rangos
    /// entregados son disjuntos: cada uno queda por debajo de `next`, y el
    /// siguiente empieza en o después de él.
    #[kani::proof]
    fn io_ranges_disjoint() {
        let w = IoWindow {
            cpu: kani::any(),
            bus: 0,
            len: kani::any(),
        };
        let Some(mut a) = IoAllocator::new(&w) else {
            return;
        };
        // Invariante (lo mantienen `new` y `alloc`):
        // IO_FIRST <= next <= IO_LIMIT.
        let next: u64 = kani::any();
        kani::assume(next >= IO_FIRST && next <= IO_LIMIT);
        a.next = next;
        let size: u64 = kani::any();
        match a.alloc(size) {
            Some(r) => {
                let end = r.end().unwrap();
                assert!(r.base >= next && end == a.next_free());
                assert!(r.len == size && r.base & (size - 1) == 0);
                assert!(end <= w.len && end <= IO_LIMIT);
            }
            None => assert!(a.next_free() == next),
        }
    }

    /// Dos rangos seguidos (caso concreto del argumento de inducción, con
    /// tamaños cualesquiera de hasta 16 bits) no se solapan.
    #[kani::proof]
    fn io_two_ranges_disjoint() {
        let mut a = IoAllocator {
            next: IO_FIRST,
            end: IO_LIMIT,
        };
        let (s1, s2): (u64, u64) = (kani::any(), kani::any());
        kani::assume(s1 <= IO_LIMIT && s2 <= IO_LIMIT);
        if let (Some(x), Some(y)) = (a.alloc(s1), a.alloc(s2)) {
            assert!(!x.overlaps(&y));
        }
    }

    /// Un acceso que acepta `pio_offset` cae entero en el rango y en la
    /// ventana.
    #[kani::proof]
    fn pio_access_in_range() {
        let (base, len, off, width, wlen): (u64, u64, u64, u64, u64) = (
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
        );
        if let Some(at) = pio_offset(base, len, off, width, wlen) {
            assert!(at >= base && at + width <= base + len);
            assert!(at + width <= wlen);
        }
    }

    /// La dirección de configuración de cualquier función cae, con su
    /// página entera, dentro de la región ECAM.
    #[kani::proof]
    fn ecam_addr_in_region() {
        let h = PciHost {
            ecam: any_region(),
            bus_start: kani::any(),
            bus_end: kani::any(),
            io: None,
        };
        let (b, d, f): (u8, u8, u8) = (kani::any(), kani::any(), kani::any());
        if let Some(a) = h.config_addr(b, d, f) {
            assert!(a >= h.ecam.base && a - h.ecam.base + PAGE <= h.ecam.len);
        }
    }
}
