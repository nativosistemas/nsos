//! Cell `virtblk`: driver de disco virtio (virtio-blk, interfaz PCI legacy)
//! en user-space.
//!
//! Capacidades de arranque (las entrega el kernel tras encontrar el
//! dispositivo `1af4:1001` en PCI):
//! 1. `IoPorts`: BAR0 del dispositivo (registros virtio legacy).
//! 2. `Memory` con `CONTROL`: 5 páginas contiguas; las 4 primeras para la
//!    virtqueue y la última para la cabecera y el estado de cada pedido.
//! 3. `Memory` con `CONTROL`: el búfer de datos, **compartido** con el
//!    cliente (el kernel le da al cliente otra capacidad al mismo objeto, sin
//!    `CONTROL`). El dispositivo lee y escribe ahí por DMA.
//! 4. Canal con el cliente.
//!
//! Protocolo (genérico: un bloque de 512 bytes = un sector, como el
//! dispositivo). Sirve igual para el almacén propio (`almacen`) y para una
//! futura cell lectora de FAT32:
//! * `INFO` → `INFO [sectores u64 | flush u8 | búfer en sectores u32]`
//! * `READ [sector u64 | cantidad u32]` → los datos quedan al principio del
//!   búfer compartido; responde `DONE [estado u8]`.
//! * `WRITE [sector u64 | cantidad u32]` → escribe lo que hay al principio
//!   del búfer; `DONE`.
//! * `FLUSH` → todo lo escrito antes queda en el disco; `DONE`.
//!
//! Estado 0 = bien, 1 = error del dispositivo, 2 = pedido inválido.
//!
//! Sin interrupciones: sondea el anillo "used" cediendo la CPU. Un pedido a
//! la vez. Termina con 0 cuando el cliente cierra el canal.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    bootstrap, channel_recv, channel_send, fence, ioport_read, ioport_write, mem_info, mem_map,
    println, yield_now, Mapped,
};

const INFO: u64 = 1;
const READ: u64 = 2;
const WRITE: u64 = 3;
const DONE: u64 = 4;
const FLUSH: u64 = 5;

const OK: u8 = 0;
const DEV_ERROR: u8 = 1;
const BAD_REQUEST: u8 = 2;

// Registros virtio legacy (desplazamientos desde BAR0).
const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_PFN: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
/// Configuración del dispositivo (sin MSI-X): capacidad en sectores.
const REG_CAPACITY: u16 = 0x14;

const STATUS_ACK: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FAILED: u32 = 128;

/// `VIRTIO_BLK_F_FLUSH`: el dispositivo tiene caché y entiende `FLUSH`.
const F_FLUSH: u32 = 1 << 9;

const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

const RING_VADDR: u64 = 0x1000_0000;
const BUF_VADDR: u64 = 0x2000_0000;
const PAGE: usize = 4096;
const SECTOR: usize = 512;
/// La cola usa como mucho las primeras 4 páginas del objeto de anillos.
const QUEUE_SPAN: usize = 4 * PAGE;
/// Cabecera del pedido (16 bytes) y estado (1 byte) en la quinta página.
const HDR_OFF: usize = QUEUE_SPAN;
const STATUS_OFF: usize = QUEUE_SPAN + 16;
const MAX_POLLS: u32 = 10_000_000;

struct Device {
    ports: CapHandle,
    ring: Mapped,
    ring_phys: u64,
    buf: Mapped,
    buf_phys: u64,
    size: usize,
    avail_off: usize,
    used_off: usize,
    next_avail: u16,
    sectors: u64,
    flush: bool,
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

    fn init(ports: CapHandle, ring_cap: CapHandle, buf_cap: CapHandle) -> Result<Device, Error> {
        let ring = mem_map(ring_cap, RING_VADDR, Rights::READ | Rights::WRITE)?;
        let buf = mem_map(buf_cap, BUF_VADDR, Rights::READ | Rights::WRITE)?;
        let ring_phys = mem_info(ring_cap)?.phys;
        let buf_phys = mem_info(buf_cap)?.phys;
        if ring_phys == 0 || buf_phys == 0 {
            return Err(Error::AccessDenied); // sin CONTROL no hay DMA
        }
        if ring.len() < QUEUE_SPAN + PAGE || buf.len() < SECTOR {
            return Err(Error::NoMemory);
        }
        let mut d = Device {
            ports,
            ring,
            ring_phys,
            buf,
            buf_phys,
            size: 0,
            avail_off: 0,
            used_off: 0,
            next_avail: 0,
            sectors: 0,
            flush: false,
        };
        // Secuencia legacy: reset, ACK, DRIVER, features, cola, DRIVER_OK.
        d.reg_w(REG_STATUS, 1, 0)?;
        d.reg_w(REG_STATUS, 1, STATUS_ACK)?;
        d.reg_w(REG_STATUS, 1, STATUS_ACK | STATUS_DRIVER)?;
        let features = d.reg_r(REG_DEVICE_FEATURES, 4)?;
        d.flush = features & F_FLUSH != 0;
        d.reg_w(REG_GUEST_FEATURES, 4, features & F_FLUSH)?;
        let lo = d.reg_r(REG_CAPACITY, 4)? as u64;
        let hi = d.reg_r(REG_CAPACITY + 4, 4)? as u64;
        d.sectors = lo | hi << 32;
        d.reg_w(REG_QUEUE_SELECT, 2, 0)?;
        let n = d.reg_r(REG_QUEUE_SIZE, 2)? as usize;
        let avail_off = 16 * n;
        let used_off = align_up(avail_off + 6 + 2 * n, PAGE);
        let total = used_off + align_up(6 + 8 * n, PAGE);
        if n < 3 || total > QUEUE_SPAN {
            println!("virtblk: la cola de {} entradas no entra", n);
            d.reg_w(REG_STATUS, 1, STATUS_FAILED)?;
            return Err(Error::NoMemory);
        }
        d.size = n;
        d.avail_off = avail_off;
        d.used_off = used_off;
        d.reg_w(REG_QUEUE_PFN, 4, (d.ring_phys / PAGE as u64) as u32)?;
        d.reg_w(REG_STATUS, 1, STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK)?;
        println!(
            "virtblk: listo ({} sectores = {} KiB, flush {}, cola de {})",
            d.sectors,
            d.sectors / 2,
            if d.flush { "sí" } else { "no" },
            n
        );
        Ok(d)
    }

    fn desc(&self, i: usize, addr: u64, len: u32, flags: u16, next: u16) {
        let off = 16 * i;
        self.ring.write_u64(off, addr);
        self.ring.write_u32(off + 8, len);
        self.ring.write_u16(off + 12, flags);
        self.ring.write_u16(off + 14, next);
    }

    /// Un pedido: cabecera, datos (si hay) y estado, encadenados desde el
    /// descriptor 0. Espera a que el dispositivo lo complete.
    fn request(&mut self, kind: u32, sector: u64, count: u32) -> Result<u8, Error> {
        let hdr = self.ring_phys + HDR_OFF as u64;
        let status = self.ring_phys + STATUS_OFF as u64;
        self.ring.write_u32(HDR_OFF, kind);
        self.ring.write_u32(HDR_OFF + 4, 0);
        self.ring.write_u64(HDR_OFF + 8, sector);
        self.ring.write_u8(STATUS_OFF, 0xFF);
        if count == 0 {
            self.desc(0, hdr, 16, DESC_NEXT, 1);
            self.desc(1, status, 1, DESC_WRITE, 0);
        } else {
            let data_flags = if kind == T_IN { DESC_WRITE } else { 0 };
            self.desc(0, hdr, 16, DESC_NEXT, 1);
            self.desc(
                1,
                self.buf_phys,
                count * SECTOR as u32,
                data_flags | DESC_NEXT,
                2,
            );
            self.desc(2, status, 1, DESC_WRITE, 0);
        }
        let slot = self.avail_off + 4 + 2 * (self.next_avail as usize % self.size);
        self.ring.write_u16(slot, 0);
        fence();
        self.next_avail = self.next_avail.wrapping_add(1);
        self.ring.write_u16(self.avail_off + 2, self.next_avail);
        fence();
        self.reg_w(REG_QUEUE_NOTIFY, 2, 0)?;
        for _ in 0..MAX_POLLS {
            fence();
            if self.ring.read_u16(self.used_off + 2) == self.next_avail {
                return Ok(self.ring.read_u8(STATUS_OFF));
            }
            yield_now();
        }
        Err(Error::TimedOut)
    }

    /// Atiende un pedido del cliente; devuelve el estado.
    fn serve(&mut self, kind: u32, p: &[u8]) -> Result<u8, Error> {
        if p.len() < 12 {
            return Ok(BAD_REQUEST);
        }
        let mut s = [0u8; 8];
        s.copy_from_slice(&p[..8]);
        let sector = u64::from_le_bytes(s);
        let mut c = [0u8; 4];
        c.copy_from_slice(&p[8..12]);
        let count = u32::from_le_bytes(c);
        let max = (self.buf.len() / SECTOR) as u32;
        if count == 0
            || count > max
            || sector
                .checked_add(count as u64)
                .is_none_or(|e| e > self.sectors)
        {
            return Ok(BAD_REQUEST);
        }
        let st = self.request(kind, sector, count)?;
        Ok(if st == 0 { OK } else { DEV_ERROR })
    }
}

fn run(dev: &mut Device, client: CapHandle) -> Result<(), Error> {
    let mut m = Message::default();
    loop {
        match channel_recv(client, &mut m, 0) {
            Ok(()) => {}
            Err(Error::PeerClosed) => return Ok(()),
            Err(e) => return Err(e),
        }
        let reply = match m.header.label {
            INFO => {
                let mut p = [0u8; 13];
                p[..8].copy_from_slice(&dev.sectors.to_le_bytes());
                p[8] = dev.flush as u8;
                p[9..13].copy_from_slice(&((dev.buf.len() / SECTOR) as u32).to_le_bytes());
                Message::new(INFO, &p)?
            }
            READ => Message::new(DONE, &[dev.serve(T_IN, m.payload())?])?,
            WRITE => Message::new(DONE, &[dev.serve(T_OUT, m.payload())?])?,
            FLUSH => {
                let st = if dev.flush {
                    match dev.request(T_FLUSH, 0, 0)? {
                        0 => OK,
                        _ => DEV_ERROR,
                    }
                } else {
                    OK // sin caché: lo escrito ya está en el disco
                };
                Message::new(DONE, &[st])?
            }
            _ => Message::new(DONE, &[BAD_REQUEST])?,
        };
        match channel_send(client, &reply, 0) {
            Ok(()) => {}
            Err(Error::PeerClosed) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

nsos_rt::entry!(main);

fn main() -> i32 {
    let client = bootstrap(4);
    let mut dev = match Device::init(bootstrap(1), bootstrap(2), bootstrap(3)) {
        Ok(d) => d,
        Err(e) => {
            println!("virtblk: inicialización falló: {:?}", e);
            return 2;
        }
    };
    match run(&mut dev, client) {
        Ok(()) => 0,
        Err(e) => {
            println!("virtblk: {:?}", e);
            3
        }
    }
}
