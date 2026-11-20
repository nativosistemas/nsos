//! UART 16550 (COM1) para la consola de depuración del kernel.
//!
//! Este es el único "driver" que vive en el kernel: hace falta para depurar
//! antes de que existan cells. El driver de consola definitivo será una cell
//! virtio (hito 5).

use super::port::Port;
use core::fmt;

const COM1: u16 = 0x3F8;

/// Puerto serie 16550.
pub struct Serial {
    data: Port<u8>,
    int_en: Port<u8>,
    fifo: Port<u8>,
    line_ctrl: Port<u8>,
    modem_ctrl: Port<u8>,
    line_status: Port<u8>,
}

impl Serial {
    /// La consola del sistema (COM1).
    pub const fn console() -> Self {
        Self::com1()
    }

    /// COM1 estándar.
    pub const fn com1() -> Self {
        // SAFETY: 0x3F8..=0x3FD son los registros de COM1 en todo PC/QEMU y
        // solo este módulo los usa (el acceso está serializado por el Mutex
        // de `console`).
        unsafe {
            Self {
                data: Port::new(COM1),
                int_en: Port::new(COM1 + 1),
                fifo: Port::new(COM1 + 2),
                line_ctrl: Port::new(COM1 + 3),
                modem_ctrl: Port::new(COM1 + 4),
                line_status: Port::new(COM1 + 5),
            }
        }
    }

    /// Configura 115200 8N1, FIFO activada, sin interrupciones.
    pub fn init(&self) {
        self.int_en.write(0x00);
        self.line_ctrl.write(0x80); // DLAB
        self.data.write(0x01); // divisor bajo: 115200 baudios
        self.int_en.write(0x00); // divisor alto
        self.line_ctrl.write(0x03); // 8N1
        self.fifo.write(0xC7);
        self.modem_ctrl.write(0x0B);
    }

    fn write_byte(&self, b: u8) {
        // Esperar a que el registro de transmisión esté vacío.
        while self.line_status.read() & 0x20 == 0 {
            core::hint::spin_loop();
        }
        self.data.write(b);
    }
}

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(b);
        }
        Ok(())
    }
}
