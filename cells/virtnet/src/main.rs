//! Cell `virtnet`: driver de red virtio (interfaz PCI legacy) en user-space.
//!
//! Capacidades de arranque (las entrega el kernel tras encontrar el
//! dispositivo `1af4:1000` en PCI):
//! 1. `IoPorts`: BAR0 del dispositivo (registros virtio legacy).
//! 2. `Memory` con `CONTROL`: 8 páginas contiguas para las dos virtqueues
//!    (recepción en las primeras 4, transmisión en las últimas 4).
//! 3. `Memory` con `CONTROL`: búferes de datos (`RX_BUFS` de recepción y uno
//!    de transmisión).
//! 4. Canal con el cliente (la cell `enlace`).
//!
//! Protocolo con el cliente: `SEND` con un paquete → el driver lo manda en
//! una trama Ethernet de difusión con EtherType `nsos_link::ETHERTYPE`
//! (precedido por su largo, porque Ethernet rellena las tramas cortas). Cada
//! trama recibida con ese EtherType llega al cliente como `RECV`. El resto
//! del tráfico se descarta. El driver no autentica nada: eso es trabajo del
//! enlace, y el driver podría estar comprometido sin que el enlace acepte
//! una trama falsa.
//!
//! Sin interrupciones: sondea el anillo "used" de recepción y el canal,
//! cediendo la CPU entre vueltas. Termina con 0 cuando el cliente cierra.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::msg::MAX_DATA;
use nsos_rt::abi::syscall::flags::NONBLOCK;
use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    bootstrap, channel_recv, channel_send, fence, ioport_read, ioport_write, mem_info, mem_map,
    println, yield_now, Mapped,
};

const SEND: u64 = 1;
const RECV: u64 = 2;
const ETHERTYPE: u16 = 0x88B5;

// Registros virtio legacy (desplazamientos desde BAR0).
const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_PFN: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
/// Configuración del dispositivo (sin MSI-X): la MAC.
const REG_MAC: u16 = 0x14;

const STATUS_ACK: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FAILED: u32 = 128;

const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;
const DESC_WRITE: u16 = 2;
const RING_VADDR: u64 = 0x1000_0000;
const BUF_VADDR: u64 = 0x2000_0000;
const PAGE: usize = 4096;
/// Cada cola ocupa como mucho 4 páginas del objeto de anillos.
const QUEUE_SPAN: usize = 4 * PAGE;
const RX_BUFS: usize = 8;
const BUF_SIZE: usize = 2048;
/// Cabecera virtio-net legacy (sin `MRG_RXBUF`).
const NET_HDR: usize = 10;
const ETH_HDR: usize = 14;
const MAX_POLLS: u32 = 1_000_000;

struct Queue {
    base: usize,
    size: usize,
    avail_off: usize,
    used_off: usize,
    next_avail: u16,
    last_used: u16,
}

struct Device {
    ports: CapHandle,
    ring: Mapped,
    buf: Mapped,
    buf_phys: u64,
    ring_phys: u64,
    mac: [u8; 6],
    rx: Queue,
    tx: Queue,
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

    fn setup_queue(&self, index: u16, base: usize) -> Result<Queue, Error> {
        self.reg_w(REG_QUEUE_SELECT, 2, index as u32)?;
        let n = self.reg_r(REG_QUEUE_SIZE, 2)? as usize;
        let avail_off = 16 * n;
        let used_off = align_up(avail_off + 6 + 2 * n, PAGE);
        let total = used_off + align_up(6 + 8 * n, PAGE);
        if n == 0 || total > QUEUE_SPAN || base + total > self.ring.len() {
            println!("virtnet: la cola {} de {} entradas no entra", index, n);
            return Err(Error::NoMemory);
        }
        self.reg_w(
            REG_QUEUE_PFN,
            4,
            ((self.ring_phys + base as u64) / PAGE as u64) as u32,
        )?;
        Ok(Queue {
            base,
            size: n,
            avail_off,
            used_off,
            next_avail: 0,
            last_used: 0,
        })
    }

    fn init(ports: CapHandle, ring_cap: CapHandle, buf_cap: CapHandle) -> Result<Device, Error> {
        let ring = mem_map(ring_cap, RING_VADDR, Rights::READ | Rights::WRITE)?;
        let buf = mem_map(buf_cap, BUF_VADDR, Rights::READ | Rights::WRITE)?;
        let ring_phys = mem_info(ring_cap)?.phys;
        let buf_phys = mem_info(buf_cap)?.phys;
        if ring_phys == 0 || buf_phys == 0 {
            return Err(Error::AccessDenied); // sin CONTROL no hay DMA
        }
        if buf.len() < (RX_BUFS + 1) * BUF_SIZE || ring.len() < 2 * QUEUE_SPAN {
            return Err(Error::NoMemory);
        }
        let mut d = Device {
            ports,
            ring,
            buf,
            buf_phys,
            ring_phys,
            mac: [0; 6],
            rx: empty_queue(),
            tx: empty_queue(),
        };
        // Secuencia legacy: reset, ACK, DRIVER, colas, DRIVER_OK.
        d.reg_w(REG_STATUS, 1, 0)?;
        d.reg_w(REG_STATUS, 1, STATUS_ACK)?;
        d.reg_w(REG_STATUS, 1, STATUS_ACK | STATUS_DRIVER)?;
        let features = d.reg_r(REG_DEVICE_FEATURES, 4)?;
        d.reg_w(REG_GUEST_FEATURES, 4, 0)?; // sin features opcionales
        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = d.reg_r(REG_MAC + i as u16, 1)? as u8;
        }
        d.mac = mac;
        d.rx = match d.setup_queue(RX_QUEUE, 0) {
            Ok(q) => q,
            Err(e) => {
                d.reg_w(REG_STATUS, 1, STATUS_FAILED)?;
                return Err(e);
            }
        };
        d.tx = match d.setup_queue(TX_QUEUE, QUEUE_SPAN) {
            Ok(q) => q,
            Err(e) => {
                d.reg_w(REG_STATUS, 1, STATUS_FAILED)?;
                return Err(e);
            }
        };
        // Todos los búferes de recepción, a disposición del dispositivo.
        for i in 0..RX_BUFS {
            d.post_rx(i as u16);
        }
        d.reg_w(REG_STATUS, 1, STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK)?;
        d.reg_w(REG_QUEUE_NOTIFY, 2, RX_QUEUE as u32)?;
        let m = d.mac;
        println!(
            "virtnet: listo (features {:#x}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, colas de {} y {})",
            features, m[0], m[1], m[2], m[3], m[4], m[5], d.rx.size, d.tx.size
        );
        Ok(d)
    }

    /// Publica el descriptor `id` (búfer de recepción `id`) en "avail".
    fn post_rx(&mut self, id: u16) {
        let q = &mut self.rx;
        let desc = q.base + 16 * id as usize;
        self.ring
            .write_u64(desc, self.buf_phys + (id as usize * BUF_SIZE) as u64);
        self.ring.write_u32(desc + 8, BUF_SIZE as u32);
        self.ring.write_u16(desc + 12, DESC_WRITE);
        self.ring.write_u16(desc + 14, 0);
        let slot = q.base + q.avail_off + 4 + 2 * (q.next_avail as usize % q.size);
        self.ring.write_u16(slot, id);
        fence();
        q.next_avail = q.next_avail.wrapping_add(1);
        self.ring.write_u16(q.base + q.avail_off + 2, q.next_avail);
        fence();
    }

    /// Manda `payload` en una trama Ethernet de difusión.
    fn transmit(&mut self, payload: &[u8]) -> Result<(), Error> {
        let off = RX_BUFS * BUF_SIZE;
        let total = NET_HDR + ETH_HDR + 2 + payload.len();
        // Ethernet: 60 bytes como mínimo (sin el FCS).
        let frame_len = total.max(NET_HDR + 60);
        if frame_len > BUF_SIZE {
            return Err(Error::InvalidArgs);
        }
        for i in 0..frame_len {
            self.buf.write_u8(off + i, 0);
        }
        let mut hdr = [0u8; ETH_HDR + 2];
        hdr[..6].copy_from_slice(&[0xff; 6]);
        hdr[6..12].copy_from_slice(&self.mac);
        hdr[12..14].copy_from_slice(&ETHERTYPE.to_be_bytes());
        hdr[14..16].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        self.buf.write_bytes(off + NET_HDR, &hdr);
        self.buf.write_bytes(off + NET_HDR + hdr.len(), payload);
        let q = &mut self.tx;
        let desc = q.base;
        self.ring.write_u64(desc, self.buf_phys + off as u64);
        self.ring.write_u32(desc + 8, frame_len as u32);
        self.ring.write_u16(desc + 12, 0);
        self.ring.write_u16(desc + 14, 0);
        let slot = q.base + q.avail_off + 4 + 2 * (q.next_avail as usize % q.size);
        self.ring.write_u16(slot, 0);
        fence();
        q.next_avail = q.next_avail.wrapping_add(1);
        self.ring.write_u16(q.base + q.avail_off + 2, q.next_avail);
        fence();
        let (used, want) = (q.base + q.used_off + 2, q.next_avail);
        self.reg_w(REG_QUEUE_NOTIFY, 2, TX_QUEUE as u32)?;
        for _ in 0..MAX_POLLS {
            fence();
            if self.ring.read_u16(used) == want {
                return Ok(());
            }
            yield_now();
        }
        Err(Error::TimedOut)
    }

    /// Si llegó una trama, la entrega a `deliver` (solo las del enlace) y
    /// devuelve el búfer al dispositivo. `Ok(false)` si no había nada.
    fn poll_rx(
        &mut self,
        mut deliver: impl FnMut(&[u8]) -> Result<(), Error>,
    ) -> Result<bool, Error> {
        fence();
        let q = &self.rx;
        if self.ring.read_u16(q.base + q.used_off + 2) == q.last_used {
            return Ok(false);
        }
        let elem = q.base + q.used_off + 4 + 8 * (q.last_used as usize % q.size);
        let id = self.ring.read_u32(elem) as usize;
        let len = self.ring.read_u32(elem + 4) as usize;
        self.rx.last_used = self.rx.last_used.wrapping_add(1);
        if id >= RX_BUFS {
            return Err(Error::BadMessage);
        }
        let base = id * BUF_SIZE;
        let len = len.min(BUF_SIZE);
        let mut frame = [0u8; BUF_SIZE];
        for (i, b) in frame.iter_mut().take(len).enumerate() {
            *b = self.buf.read_u8(base + i);
        }
        self.post_rx(id as u16);
        self.reg_w(REG_QUEUE_NOTIFY, 2, RX_QUEUE as u32)?;
        let eth = &frame[..len];
        if eth.len() >= NET_HDR + ETH_HDR + 2
            && eth[NET_HDR + 12..NET_HDR + 14] == ETHERTYPE.to_be_bytes()
        {
            let start = NET_HDR + ETH_HDR + 2;
            let n = u16::from_le_bytes([eth[start - 2], eth[start - 1]]) as usize;
            if n <= MAX_DATA && start + n <= eth.len() {
                deliver(&eth[start..start + n])?;
            }
        }
        Ok(true)
    }
}

fn empty_queue() -> Queue {
    Queue {
        base: 0,
        size: 0,
        avail_off: 0,
        used_off: 0,
        next_avail: 0,
        last_used: 0,
    }
}

fn run(dev: &mut Device, client: CapHandle) -> Result<(), Error> {
    let mut m = Message::default();
    loop {
        let mut idle = true;
        match channel_recv(client, &mut m, NONBLOCK) {
            Ok(()) if m.header.label == SEND => {
                dev.transmit(m.payload())?;
                idle = false;
            }
            Ok(()) => println!("virtnet: etiqueta desconocida {}", m.header.label),
            Err(Error::WouldBlock) => {}
            Err(Error::PeerClosed) => return Ok(()),
            Err(e) => return Err(e),
        }
        while dev.poll_rx(|p| channel_send(client, &Message::new(RECV, p)?, 0))? {
            idle = false;
        }
        if idle {
            yield_now();
        }
    }
}

nsos_rt::entry!(main);

fn main() -> i32 {
    let client = bootstrap(4);
    let mut dev = match Device::init(bootstrap(1), bootstrap(2), bootstrap(3)) {
        Ok(d) => d,
        Err(e) => {
            println!("virtnet: inicialización falló: {:?}", e);
            return 2;
        }
    };
    match run(&mut dev, client) {
        Ok(()) => 0,
        Err(e) => {
            println!("virtnet: {:?}", e);
            3
        }
    }
}
