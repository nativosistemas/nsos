//! Cell `virtcon`: driver de consola virtio (interfaz PCI legacy) en
//! user-space.
//!
//! Capacidades de arranque (las entrega el kernel tras encontrar el
//! dispositivo en PCI):
//! 1. `IoPorts`: BAR0 del dispositivo (registros virtio legacy).
//! 2. `Memory` con `CONTROL`: memoria contigua para la virtqueue (DMA).
//! 3. `Memory` con `CONTROL`: búfer de datos (DMA).
//! 4. Canal hacia el cliente.
//! 5. `Irq` de su línea de interrupción, si el kernel la pudo entregar.
//!
//! Protocolo con el cliente: `WRITE` con texto → el driver lo transmite por
//! la cola 1 (transmitq del puerto 0) y responde `WRITTEN` con la cantidad
//! de bytes. Cuando el cliente cierra el canal, el driver termina con 0.
//!
//! Espera la finalización durmiendo en `IRQ_PENDING` (`wait`): al llegar la
//! interrupción lee el registro ISR (eso apaga la línea), mira el anillo
//! "used" y hace `irq_ack`. Sin línea (compartida con otro dispositivo),
//! sondea el anillo cediendo la CPU y le pide al dispositivo que no
//! interrumpa.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    bootstrap, cap_dup, cap_info, channel_recv, channel_send, fence, ioport_read, ioport_write,
    mem_info, mem_map, println, DevIrq, Mapped,
};

const WRITE: u64 = 1;
const WRITTEN: u64 = 2;

// Registros virtio legacy (desplazamientos desde BAR0).
const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_PFN: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
/// Registro ISR: leerlo apaga la línea de interrupción.
const REG_ISR: u16 = 0x13;
/// `VRING_AVAIL_F_NO_INTERRUPT`.
const AVAIL_NO_INTERRUPT: u16 = 1;
/// Red de seguridad de cada espera (el driver vuelve a mirar el anillo).
const IRQ_TIMEOUT_NS: u64 = 50_000_000;

const STATUS_ACK: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FAILED: u32 = 128;

const TX_QUEUE: u32 = 1;
const RING_VADDR: u64 = 0x1000_0000;
const BUF_VADDR: u64 = 0x2000_0000;
const PAGE: usize = 4096;
const MAX_POLLS: u32 = 1_000_000;

struct Device {
    ports: CapHandle,
    ring: Mapped,
    buf: Mapped,
    buf_phys: u64,
    size: usize,
    avail_off: usize,
    used_off: usize,
    next_avail: u16,
    irq: DevIrq,
}

fn align_up(v: usize, a: usize) -> usize {
    v.div_ceil(a) * a
}

impl Device {
    fn reg_w(&self, off: u16, width: u8, v: u32) -> Result<(), Error> {
        ioport_write(self.ports, off, width, v)
    }
    fn reg_r(&self, off: u16, width: u8) -> Result<u32, Error> {
        ioport_read(self.ports, off, width)
    }

    fn init(
        ports: CapHandle,
        ring_cap: CapHandle,
        buf_cap: CapHandle,
        irq: DevIrq,
    ) -> Result<Device, Error> {
        let ring = mem_map(ring_cap, RING_VADDR, Rights::READ | Rights::WRITE)?;
        let buf = mem_map(buf_cap, BUF_VADDR, Rights::READ | Rights::WRITE)?;
        let ring_phys = mem_info(ring_cap)?.phys;
        let buf_phys = mem_info(buf_cap)?.phys;
        if ring_phys == 0 || buf_phys == 0 {
            return Err(Error::AccessDenied); // sin CONTROL no hay DMA
        }
        let mut d = Device {
            ports,
            ring,
            buf,
            buf_phys,
            size: 0,
            avail_off: 0,
            used_off: 0,
            next_avail: 0,
            irq,
        };
        // Secuencia de inicialización legacy: reset, ACK, DRIVER.
        d.reg_w(REG_STATUS, 1, 0)?;
        d.reg_w(REG_STATUS, 1, STATUS_ACK)?;
        d.reg_w(REG_STATUS, 1, STATUS_ACK | STATUS_DRIVER)?;
        let features = d.reg_r(REG_DEVICE_FEATURES, 4)?;
        d.reg_w(REG_GUEST_FEATURES, 4, 0)?; // sin features opcionales
                                            // Cola de transmisión del puerto 0.
        d.reg_w(REG_QUEUE_SELECT, 2, TX_QUEUE)?;
        let n = d.reg_r(REG_QUEUE_SIZE, 2)? as usize;
        let avail_off = 16 * n;
        let used_off = align_up(avail_off + 6 + 2 * n, PAGE);
        let total = used_off + align_up(6 + 8 * n, PAGE);
        if n == 0 || total > d.ring.len() {
            d.reg_w(REG_STATUS, 1, STATUS_FAILED)?;
            println!(
                "virtcon: cola de tamaño {} no entra en {} bytes",
                n,
                d.ring.len()
            );
            return Err(Error::NoMemory);
        }
        d.size = n;
        d.avail_off = avail_off;
        d.used_off = used_off;
        if !d.irq.present() {
            d.ring.write_u16(avail_off, AVAIL_NO_INTERRUPT);
        }
        d.reg_w(REG_QUEUE_PFN, 4, (ring_phys / PAGE as u64) as u32)?;
        d.reg_w(REG_STATUS, 1, STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK)?;
        println!(
            "virtcon: dispositivo listo (features {:#x}, cola tx de {} entradas, {})",
            features,
            n,
            if d.irq.present() {
                "con interrupciones"
            } else {
                "sondeando"
            }
        );
        Ok(d)
    }

    /// Transmite `data` y espera a que el dispositivo lo consuma.
    fn transmit(&mut self, data: &[u8]) -> Result<(), Error> {
        let len = data.len().min(self.buf.len());
        self.buf.write_bytes(0, &data[..len]);
        // Descriptor 0: el búfer completo, solo lectura para el dispositivo.
        self.ring.write_u64(0, self.buf_phys);
        self.ring.write_u32(8, len as u32);
        self.ring.write_u16(12, 0);
        self.ring.write_u16(14, 0);
        // Publicarlo en el anillo "avail".
        let idx = self.next_avail;
        let slot = self.avail_off + 4 + 2 * (idx as usize % self.size);
        self.ring.write_u16(slot, 0);
        fence();
        self.ring.write_u16(self.avail_off + 2, idx.wrapping_add(1));
        fence();
        self.next_avail = idx.wrapping_add(1);
        self.reg_w(REG_QUEUE_NOTIFY, 2, TX_QUEUE)?;
        // Esperar a que aparezca en el anillo "used".
        for _ in 0..MAX_POLLS {
            fence();
            if self.ring.read_u16(self.used_off + 2) == self.next_avail {
                return Ok(());
            }
            if self.irq.wait(IRQ_TIMEOUT_NS) {
                self.reg_r(REG_ISR, 1)?;
                self.irq.ack();
            }
        }
        Err(Error::TimedOut)
    }
}

/// La línea de interrupción no sale de este driver: la capacidad no tiene
/// `DUPLICATE` ni `GRANT`, así que ni se copia ni viaja en un mensaje (y
/// sigue siendo nuestra después del intento).
fn irq_stays_here(h: CapHandle, client: CapHandle) -> bool {
    let dup = cap_dup(h, Rights::WAIT);
    let mut msg = Message::new(0, b"").unwrap();
    let sent = msg.push_cap(h).and_then(|_| channel_send(client, &msg, 0));
    let still = cap_info(h).is_ok();
    let ok = dup == Err(Error::AccessDenied) && sent == Err(Error::AccessDenied) && still;
    println!(
        "virtcon: la línea de interrupción no se copia ni se pasa a otra cell: {}",
        if ok { "ok" } else { "¡FALLÓ!" }
    );
    ok
}

nsos_rt::entry!(main);

fn main() -> i32 {
    let client = bootstrap(4);
    let mut m = Message::default();
    let irq = DevIrq::from_bootstrap(5);
    let mut dev = match Device::init(bootstrap(1), bootstrap(2), bootstrap(3), irq) {
        Ok(d) => d,
        Err(e) => {
            println!("virtcon: inicialización falló: {:?}", e);
            return 2;
        }
    };
    if let Some(h) = dev.irq.handle() {
        if !irq_stays_here(h, client) {
            return 5;
        }
    }
    loop {
        match channel_recv(client, &mut m, 0) {
            Ok(()) if m.header.label == WRITE => {
                let n = m.payload().len();
                if let Err(e) = dev.transmit(m.payload()) {
                    println!("virtcon: transmisión falló: {:?}", e);
                    return 3;
                }
                let r = Message::new(WRITTEN, &(n as u32).to_le_bytes()).unwrap();
                let _ = channel_send(client, &r, 0);
            }
            Ok(()) => {
                println!("virtcon: etiqueta desconocida {}", m.header.label);
            }
            Err(Error::PeerClosed) => {
                println!(
                    "virtcon: el cliente cerró el canal; termino ({} interrupciones)",
                    dev.irq.count
                );
                return 0;
            }
            Err(e) => {
                println!("virtcon: recv falló: {:?}", e);
                return 4;
            }
        }
    }
}
