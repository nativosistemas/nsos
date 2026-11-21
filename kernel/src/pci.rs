//! Enumeración PCI mínima.
//!
//! * x86_64: mecanismo de configuración #1 (puertos CF8/CFC); los BAR los
//!   asignó el firmware.
//! * ARM64: ECAM (`pci-host-ecam-generic`, del device tree). El kernel
//!   asigna él mismo los BAR de E/S, con un asignador verificado
//!   (`nsos_kcore::platform::IoAllocator`: rangos disjuntos dentro de la
//!   ventana), y los accesos de las cells a esos puertos pasan por la
//!   ventana de E/S mapeada (`pio_access`).
//!
//! El kernel solo *descubre* dispositivos y entrega sus recursos (puertos,
//! memoria) como capacidades a la cell driver correspondiente; no los maneja.

pub use backend::init;
#[cfg(target_arch = "aarch64")]
pub use backend::pio_access;
use backend::{config_read, config_write};

#[cfg(target_arch = "x86_64")]
mod backend {
    use crate::arch::port::Port;
    use crate::sync::IrqSpinLock;

    static CONFIG: IrqSpinLock<()> = IrqSpinLock::new(());

    fn addr(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
        0x8000_0000u32
            | (bus as u32) << 16
            | (dev as u32) << 11
            | (func as u32) << 8
            | (off as u32 & 0xFC)
    }

    pub fn config_read(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
        let _g = CONFIG.lock();
        // SAFETY: CF8/CFC son los puertos de configuración PCI, usados solo
        // aquí y serializados por `CONFIG`.
        unsafe {
            Port::<u32>::new(0xCF8).write(addr(bus, dev, func, off));
            Port::<u32>::new(0xCFC).read()
        }
    }

    pub fn config_write(bus: u8, dev: u8, func: u8, off: u8, v: u32) {
        let _g = CONFIG.lock();
        // SAFETY: ídem `config_read`.
        unsafe {
            Port::<u32>::new(0xCF8).write(addr(bus, dev, func, off));
            Port::<u32>::new(0xCFC).write(v);
        }
    }

    /// Buses que se recorren: todos (como siempre en x86_64), y el rango
    /// válido para los secundarios.
    pub fn buses() -> (core::ops::RangeInclusive<u8>, core::ops::RangeInclusive<u8>) {
        (0..=255, 0..=255)
    }

    /// En x86_64 el firmware ya asignó los recursos.
    pub fn init() {}
}

#[cfg(target_arch = "aarch64")]
mod backend {
    use super::Device;
    use crate::sync::IrqSpinLock;
    use alloc::vec::Vec;
    use nsos_kcore::platform::{pio_offset, IoAllocator, PciHost, Region, PAGE};

    struct Ecam {
        host: PciHost,
        /// Ventana de E/S mapeada: (dirección virtual, largo).
        pio: Option<(u64, u64)>,
        /// BAR de E/S asignados por el kernel: (dispositivo, BAR, puertos).
        io_bars: Vec<(Device, u8, Region)>,
    }

    static ECAM: IrqSpinLock<Option<Ecam>> = IrqSpinLock::new(None);

    fn config_virt(bus: u8, dev: u8, func: u8) -> Option<u64> {
        let host = ECAM.lock().as_ref()?.host;
        // `config_addr` cae dentro de la región ECAM (Kani:
        // `ecam_addr_in_region`), que `platform::discover` validó.
        let phys = host.config_addr(bus, dev, func)?;
        Some(crate::mm::map_mmio(phys, PAGE))
    }

    pub fn config_read(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
        let Some(v) = config_virt(bus, dev, func) else {
            return 0xFFFF_FFFF;
        };
        // SAFETY: `v` es la página de configuración de la función, mapeada
        // como dispositivo; `off & 0xFC` es un registro de 32 bits dentro.
        unsafe { ((v + (off as u64 & 0xFC)) as *const u32).read_volatile() }
    }

    pub fn config_write(bus: u8, dev: u8, func: u8, off: u8, val: u32) {
        if let Some(v) = config_virt(bus, dev, func) {
            // SAFETY: ídem `config_read`.
            unsafe { ((v + (off as u64 & 0xFC)) as *mut u32).write_volatile(val) }
        }
    }

    /// Buses que se recorren: el primero del controlador (los demás se
    /// descubren detrás de los puentes), y el rango que decodifica.
    #[allow(clippy::reversed_empty_ranges)]
    pub fn buses() -> (core::ops::RangeInclusive<u8>, core::ops::RangeInclusive<u8>) {
        match ECAM.lock().as_ref() {
            Some(e) => (
                e.host.bus_start..=e.host.bus_start,
                e.host.bus_start..=e.host.bus_end,
            ),
            None => (1..=0, 1..=0),
        }
    }

    /// Toma el controlador del device tree, mapea la ventana de E/S y
    /// asigna los BAR de E/S de todos los dispositivos.
    pub fn init() {
        let Some(host) = crate::arch::platform::get().pci else {
            crate::kprintln!("pci: no hay controlador ECAM en el device tree");
            return;
        };
        let pio = host.io.map(|w| (crate::mm::map_mmio(w.cpu, w.len), w.len));
        *ECAM.lock() = Some(Ecam {
            host,
            pio,
            io_bars: Vec::new(),
        });
        let mut alloc = host.io.and_then(|w| IoAllocator::new(&w));
        let mut assigned = Vec::new();
        super::for_each_function(|d| {
            let cmd = config_read(d.bus, d.dev, d.func, 0x04);
            // Sin decodificar E/S mientras se mide y se mueve el BAR.
            config_write(d.bus, d.dev, d.func, 0x04, cmd & !0x1);
            let mut i = 0u8;
            while i < 6 {
                let off = 0x10 + 4 * i;
                let bar = config_read(d.bus, d.dev, d.func, off);
                if bar & 1 == 0 {
                    // Memoria: los de 64 bits ocupan dos.
                    i += if bar & 0b110 == 0b100 { 2 } else { 1 };
                    continue;
                }
                config_write(d.bus, d.dev, d.func, off, 0xFFFF_FFFF);
                let mask = config_read(d.bus, d.dev, d.func, off) & 0xFFFF_FFFC;
                let size = (!mask).wrapping_add(1) as u64 & 0xFFFF;
                let r = alloc.as_mut().and_then(|a| a.alloc(size));
                config_write(d.bus, d.dev, d.func, off, r.map_or(0, |r| r.base as u32));
                match r {
                    Some(r)
                        if config_read(d.bus, d.dev, d.func, off) & 0xFFFF_FFFC
                            == r.base as u32 =>
                    {
                        assigned.push((d, i, r))
                    }
                    _ => crate::kprintln!(
                        "pci: {:02x}:{:02x}.{} BAR{} de E/S sin asignar",
                        d.bus,
                        d.dev,
                        d.func,
                        i
                    ),
                }
                i += 1;
            }
            config_write(d.bus, d.dev, d.func, 0x04, cmd & !0x1);
        });
        let n = assigned.len();
        if let Some(e) = ECAM.lock().as_mut() {
            e.io_bars = assigned;
        }
        crate::kprintln!(
            "pci: ECAM en {:#x} (buses {}-{}), E/S {}; {} BAR de E/S asignados",
            host.ecam.base,
            host.bus_start,
            host.bus_end,
            match host.io {
                Some(w) => alloc::format!("en {:#x}+{:#x}", w.cpu, w.len),
                None => alloc::string::String::from("sin ventana"),
            },
            n
        );
    }

    /// BAR `i` de E/S, si el kernel lo asignó.
    pub fn io_bar(d: &Device, i: u8) -> Option<(u16, u16)> {
        let g = ECAM.lock();
        let (_, _, r) = g
            .as_ref()?
            .io_bars
            .iter()
            .find(|(x, b, _)| (x.bus, x.dev, x.func, *b) == (d.bus, d.dev, d.func, i))?;
        Some((r.base as u16, r.len as u16))
    }

    /// Acceso de una cell a sus puertos: `[base, base + len)` es el rango
    /// de su capacidad `IoPorts`. Pasa por la ventana de E/S mapeada.
    pub fn pio_access(
        base: u64,
        len: u64,
        off: u64,
        width: u64,
        value: Option<u64>,
    ) -> Option<u64> {
        let (virt, wlen) = ECAM.lock().as_ref()?.pio?;
        // Kani (`pio_access_in_range`): el acceso cae entero en el rango y
        // en la ventana.
        let at = virt + pio_offset(base, len, off, width, wlen)?;
        // SAFETY: `at..at + width` está dentro de la ventana de E/S del
        // controlador PCI (mapeada como dispositivo en `init`) y del rango
        // de puertos que el kernel asignó a este dispositivo y entregó a la
        // cell; el kernel no usa esos puertos.
        unsafe {
            Some(match (width, value) {
                (1, None) => (at as *const u8).read_volatile() as u64,
                (2, None) => (at as *const u16).read_volatile() as u64,
                (4, None) => (at as *const u32).read_volatile() as u64,
                (1, Some(v)) => {
                    (at as *mut u8).write_volatile(v as u8);
                    0
                }
                (2, Some(v)) => {
                    (at as *mut u16).write_volatile(v as u16);
                    0
                }
                (_, Some(v)) => {
                    (at as *mut u32).write_volatile(v as u32);
                    0
                }
                _ => return None,
            })
        }
    }
}

/// Dispositivo encontrado.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Device {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl Device {
    /// BAR `i` si es de E/S: (base, tamaño).
    #[cfg(target_arch = "x86_64")]
    pub fn io_bar(&self, i: u8) -> Option<(u16, u16)> {
        let off = 0x10 + 4 * i;
        let bar = config_read(self.bus, self.dev, self.func, off);
        if bar & 1 == 0 {
            return None;
        }
        // Medir el tamaño escribiendo unos y leyendo la máscara.
        config_write(self.bus, self.dev, self.func, off, 0xFFFF_FFFF);
        let mask = config_read(self.bus, self.dev, self.func, off);
        config_write(self.bus, self.dev, self.func, off, bar);
        let size = (!(mask & 0xFFFF_FFFC)).wrapping_add(1) & 0xFFFF;
        Some(((bar & 0xFFFC) as u16, size as u16))
    }

    /// BAR `i` si es de E/S y el kernel lo asignó: (base, tamaño).
    #[cfg(target_arch = "aarch64")]
    pub fn io_bar(&self, i: u8) -> Option<(u16, u16)> {
        backend::io_bar(self, i)
    }

    /// Línea de interrupción INTx del dispositivo, si tiene: en x86_64, el
    /// pin del IOAPIC que anotó el firmware en `Interrupt Line`.
    #[cfg(target_arch = "x86_64")]
    pub fn irq_line(&self) -> Option<u32> {
        let v = config_read(self.bus, self.dev, self.func, 0x3C);
        let (line, pin) = (v & 0xFF, (v >> 8) & 0xFF);
        (pin != 0 && line != 0 && line != 0xFF).then_some(line)
    }

    /// Línea de interrupción INTx del dispositivo, si tiene: en ARM64, el
    /// INTID del GIC según el `interrupt-map` del device tree. Solo para el
    /// bus raíz (detrás de un puente habría que aplicar el swizzle).
    #[cfg(target_arch = "aarch64")]
    pub fn irq_line(&self) -> Option<u32> {
        let pin = (config_read(self.bus, self.dev, self.func, 0x3C) >> 8) & 0xFF;
        let p = crate::arch::platform::get();
        if self.bus != p.pci?.bus_start {
            return None;
        }
        p.intx?.lookup(self.bus, self.dev, self.func, pin as u8)
    }

    /// Capacidad a la línea de interrupción para su driver, si el
    /// dispositivo tiene una y nadie más la reclamó.
    pub fn irq_cap(&self) -> Option<crate::cell::KCap> {
        let line = self.irq_line()?;
        match crate::irq::claim(line) {
            Some(obj) => {
                crate::kprintln!(
                    "pci: {:02x}:{:02x}.{} interrumpe por la línea {}",
                    self.bus,
                    self.dev,
                    self.func,
                    line
                );
                Some(crate::cell::KCap {
                    object: crate::cell::KObject::Irq(obj),
                    rights: crate::cell::IRQ_RIGHTS,
                })
            }
            None => {
                crate::kprintln!("pci: {:02x}:{:02x}.{}: la línea {} no se puede entregar (compartida o inválida); su driver sondea", self.bus, self.dev, self.func, line);
                None
            }
        }
    }

    /// Habilita decodificación de E/S y bus master (DMA).
    pub fn enable_io_and_dma(&self) {
        let cmd = config_read(self.bus, self.dev, self.func, 0x04);
        config_write(self.bus, self.dev, self.func, 0x04, cmd | 0x1 | 0x4);
    }
}

/// Llama a `f` con cada función presente. Recorre los buses iniciales del
/// backend y, detrás de cada puente PCI-PCI, su bus secundario (dentro del
/// rango del controlador).
fn for_each_function(mut f: impl FnMut(Device)) {
    let (first, range) = backend::buses();
    let mut pending = [false; 256];
    for b in first {
        pending[b as usize] = true;
    }
    let mut seen = [false; 256];
    while let Some(bus) = (0..256).find(|&b| pending[b] && !seen[b]) {
        seen[bus] = true;
        let bus = bus as u8;
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let id = config_read(bus, dev, func, 0);
                if id & 0xFFFF == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                f(Device { bus, dev, func });
                let header = (config_read(bus, dev, func, 0x0C) >> 16) as u8;
                if header & 0x7F == 1 {
                    // Puente: el bus secundario está en el byte 1 de 0x18.
                    let secondary = (config_read(bus, dev, func, 0x18) >> 8) as u8;
                    if range.contains(&secondary) {
                        pending[secondary as usize] = true;
                    }
                }
                // Una función 0 sin el bit de multifunción: no hay más.
                if func == 0 && header & 0x80 == 0 {
                    break;
                }
            }
        }
    }
}

/// Busca el primer dispositivo con `vendor:device`.
pub fn find(vendor: u16, device: u16) -> Option<Device> {
    let mut found = None;
    for_each_function(|d| {
        if found.is_none() {
            let id = config_read(d.bus, d.dev, d.func, 0);
            if id & 0xFFFF == vendor as u32 && id >> 16 == device as u32 {
                found = Some(d);
            }
        }
    });
    found
}
