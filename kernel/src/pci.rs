//! Enumeración PCI mínima (mecanismo de configuración #1, puertos CF8/CFC).
//!
//! El kernel solo *descubre* dispositivos y entrega sus recursos (puertos,
//! memoria) como capacidades a la cell driver correspondiente; no los maneja.

use crate::arch::port::Port;
use crate::sync::IrqSpinLock;

static CONFIG: IrqSpinLock<()> = IrqSpinLock::new(());

fn config_read(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    let _g = CONFIG.lock();
    let addr = 0x8000_0000u32
        | (bus as u32) << 16
        | (dev as u32) << 11
        | (func as u32) << 8
        | (off as u32 & 0xFC);
    // SAFETY: CF8/CFC son los puertos de configuración PCI, usados solo aquí
    // y serializados por `CONFIG`.
    unsafe {
        Port::<u32>::new(0xCF8).write(addr);
        Port::<u32>::new(0xCFC).read()
    }
}

fn config_write(bus: u8, dev: u8, func: u8, off: u8, v: u32) {
    let _g = CONFIG.lock();
    let addr = 0x8000_0000u32
        | (bus as u32) << 16
        | (dev as u32) << 11
        | (func as u32) << 8
        | (off as u32 & 0xFC);
    // SAFETY: ídem `config_read`.
    unsafe {
        Port::<u32>::new(0xCF8).write(addr);
        Port::<u32>::new(0xCFC).write(v);
    }
}

/// Dispositivo encontrado.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl Device {
    /// BAR `i` si es de E/S: (base, tamaño).
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

    /// Habilita decodificación de E/S y bus master (DMA).
    pub fn enable_io_and_dma(&self) {
        let cmd = config_read(self.bus, self.dev, self.func, 0x04);
        config_write(self.bus, self.dev, self.func, 0x04, cmd | 0x1 | 0x4);
    }
}

/// Busca el primer dispositivo con `vendor:device` en el bus 0..=255.
pub fn find(vendor: u16, device: u16) -> Option<Device> {
    for bus in 0..=255u8 {
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let id = config_read(bus, dev, func, 0);
                if id & 0xFFFF == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                if id & 0xFFFF == vendor as u32 && id >> 16 == device as u32 {
                    return Some(Device { bus, dev, func });
                }
            }
        }
    }
    None
}
