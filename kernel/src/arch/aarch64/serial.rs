//! UART PL011 (consola de depuración).
//!
//! El UART es un registro MMIO que el HHDM de Limine no mapea. Hasta que
//! `mm` lo mapea ([`set_base`]) el texto se guarda en un búfer y se vuelca
//! en la primera escritura posterior.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

/// Dirección virtual de los registros (0: todavía sin mapear).
static BASE: AtomicU64 = AtomicU64::new(0);

const DR: u64 = 0x00;
const FR: u64 = 0x18;
const FR_TXFF: u32 = 1 << 5;

/// Fija la dirección virtual del UART (ya mapeado como dispositivo).
pub fn set_base(virt: u64) {
    BASE.store(virt, Ordering::Release);
}

/// Consola serie con búfer de arranque.
pub struct Serial {
    early: [u8; 4096],
    len: usize,
}

impl Serial {
    /// La consola del sistema.
    pub const fn console() -> Self {
        Serial {
            early: [0; 4096],
            len: 0,
        }
    }

    /// El firmware ya dejó el UART configurado.
    pub fn init(&self) {}

    fn put(base: u64, b: u8) {
        // SAFETY: `base` es el PL011 mapeado como Device-nGnRnE por
        // `set_base`; FR y DR son registros de 32 bits del UART.
        unsafe {
            while ((base + FR) as *const u32).read_volatile() & FR_TXFF != 0 {
                core::hint::spin_loop();
            }
            ((base + DR) as *mut u32).write_volatile(b as u32);
        }
    }

    fn write_byte(&mut self, b: u8) {
        let base = BASE.load(Ordering::Acquire);
        if base == 0 {
            if self.len < self.early.len() {
                self.early[self.len] = b;
                self.len += 1;
            }
            return;
        }
        let n = core::mem::take(&mut self.len);
        for &c in &self.early[..n] {
            if c == b'\n' {
                Self::put(base, b'\r');
            }
            Self::put(base, c);
        }
        if b == b'\n' {
            Self::put(base, b'\r');
        }
        Self::put(base, b);
    }
}

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.write_byte(b);
        }
        Ok(())
    }
}
