//! Cell `almacen`: almacén de objetos propio de nsos, sobre el driver
//! `virtblk`.
//!
//! Guarda objetos con dueño, nombre y versión, con escritura copy-on-write
//! (`nsos_store`): una caída en cualquier momento deja el estado viejo o el
//! nuevo, nunca una mezcla. El acceso a cada objeto es una capacidad: un
//! extremo de canal ("handle") que se obtiene por la raíz del dueño y se
//! puede pasar a otra cell, entero o en copia de solo lectura. Protocolo:
//! `nsos_store::proto`. Modelo: `verification/lean/Nsos/Store.lean`.
//!
//! Capacidades de arranque:
//! 1. Canal con el driver de bloques (`virtblk`).
//! 2. `Memory` (lectura y escritura, sin `CONTROL`): el búfer que el driver
//!    usa para DMA, compartido.
//! 3. `Memory` de solo lectura con la configuración (`[modo, n, formatear]`).
//! 4. y 5. Raíces de los dueños 1 y 2.
//!
//! Modo `c` (prueba de caídas): en la escritura número `n` después de
//! montar, la cell termina justo antes de escribir el superbloque, como si
//! se cortara la luz (los datos y la tabla nuevos ya están en el disco).
//!
//! Termina con 0 cuando todos sus clientes cerraron raíces y handles.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::syscall::flags::NONBLOCK;
use nsos_rt::abi::{CapHandle, Error, Message, Rights};
use nsos_rt::{
    bootstrap, cap_drop, channel_create, channel_recv, channel_send, exit, mem_map, println,
    yield_now, Mapped,
};
use nsos_store::handles::{Handles, MAX_HANDLES};
use nsos_store::layout::Name;
use nsos_store::proto::{self, u32_at, u64_at};
use nsos_store::{BlockDev, DevError, Store, BLOCK};

// Protocolo con el driver (ver `cells/virtblk`).
const DRV_INFO: u64 = 1;
const DRV_READ: u64 = 2;
const DRV_WRITE: u64 = 3;
const DRV_DONE: u64 = 4;
const DRV_FLUSH: u64 = 5;
const SECTORS_PER_BLOCK: u64 = (BLOCK / 512) as u64;

const BUF_VADDR: u64 = 0x2000_0000;
const CONFIG_VADDR: u64 = 0x2100_0000;
const CLIENT_VADDR: u64 = 0x3000_0000;
const ROOTS: usize = 2;

/// Código de salida de una caída simulada.
const CRASH_EXIT: i32 = 7;

/// El disco, visto a través del driver.
struct Disk {
    drv: CapHandle,
    buf: Mapped,
    blocks: u32,
    /// Prueba de caídas: terminar antes del superbloque de la escritura
    /// número `crash_at` (0 = nunca).
    crash_at: u32,
    armed: bool,
    supers: u32,
}

impl Disk {
    fn call(&self, label: u64, p: &[u8]) -> Result<Message, DevError> {
        let m = Message::new(label, p).map_err(|_| DevError)?;
        channel_send(self.drv, &m, 0).map_err(|_| DevError)?;
        let mut r = Message::default();
        channel_recv(self.drv, &mut r, 0).map_err(|_| DevError)?;
        Ok(r)
    }

    fn io(&self, label: u64, blk: u32) -> Result<(), DevError> {
        let mut p = [0u8; 12];
        p[..8].copy_from_slice(&(blk as u64 * SECTORS_PER_BLOCK).to_le_bytes());
        p[8..12].copy_from_slice(&(SECTORS_PER_BLOCK as u32).to_le_bytes());
        let r = self.call(label, &p)?;
        if r.header.label == DRV_DONE && r.payload().first() == Some(&0) {
            Ok(())
        } else {
            Err(DevError)
        }
    }
}

impl BlockDev for Disk {
    fn blocks(&self) -> u32 {
        self.blocks
    }
    fn read(&mut self, blk: u32, out: &mut [u8; BLOCK]) -> Result<(), DevError> {
        self.io(DRV_READ, blk)?;
        for (i, w) in out.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            *w = self.buf.read_u64(i * 8).to_le_bytes();
        }
        Ok(())
    }
    fn write(&mut self, blk: u32, data: &[u8; BLOCK]) -> Result<(), DevError> {
        if self.armed && blk < 2 {
            self.supers += 1;
            if self.supers == self.crash_at {
                println!(
                    "almacen: CAÍDA SIMULADA en la escritura {}, antes del superbloque",
                    self.supers
                );
                exit(CRASH_EXIT);
            }
        }
        for (i, w) in data.as_chunks::<8>().0.iter().enumerate() {
            self.buf.write_u64(i * 8, u64::from_le_bytes(*w));
        }
        self.io(DRV_WRITE, blk)
    }
    fn flush(&mut self) -> Result<(), DevError> {
        let r = self.call(DRV_FLUSH, &[])?;
        if r.header.label == DRV_DONE && r.payload().first() == Some(&0) {
            Ok(())
        } else {
            Err(DevError)
        }
    }
}

struct Server {
    store: Store<Disk>,
    roots: [Option<CapHandle>; ROOTS],
    handles: Handles,
    eps: [CapHandle; MAX_HANDLES],
}

/// Un pedido rechazado: código de error y versión actual.
struct Fail(u32, u64);

impl From<nsos_store::Error> for Fail {
    fn from(e: nsos_store::Error) -> Fail {
        let v = match e {
            nsos_store::Error::VersionMismatch(v) => v,
            _ => 0,
        };
        Fail(proto::code(e), v)
    }
}

const BAD: Fail = Fail(proto::E_BAD_REQUEST, 0);

fn err(code: u32, version: u64) -> Message {
    let mut p = [0u8; 12];
    p[..4].copy_from_slice(&code.to_le_bytes());
    p[4..].copy_from_slice(&version.to_le_bytes());
    Message::new(proto::ERR, &p).unwrap_or_default()
}

impl Server {
    /// Abre un handle nuevo y devuelve el mensaje `OPENED` con el extremo
    /// del cliente adjunto.
    fn new_handle(&mut self, h: Option<usize>) -> Message {
        let Some(h) = h else {
            return err(proto::E_NO_HANDLES, 0);
        };
        let Ok((mine, theirs)) = channel_create() else {
            self.handles.close(h);
            return err(proto::E_NO_HANDLES, 0);
        };
        self.eps[h] = mine;
        let x = self.handles.get(h).expect("handle recién abierto");
        let (v, len) = self.stat(&x.name, x.owner);
        let mut m = Message::new(proto::OPENED, &proto::info(v, len, x.write)).unwrap_or_default();
        if m.push_cap(theirs).is_err() {
            return err(proto::E_BAD_REQUEST, 0);
        }
        m
    }

    fn stat(&self, name: &Name, owner: u32) -> (u64, u32) {
        self.store
            .get(owner, name)
            .map(|e| (e.version, e.len))
            .unwrap_or((0, 0))
    }

    /// Pedido por la raíz del dueño `owner`.
    fn on_root(&mut self, owner: u32, m: &Message) -> Message {
        let p = m.payload();
        if m.header.label != proto::OPEN || p.is_empty() {
            return err(proto::E_BAD_REQUEST, 0);
        }
        let Some(name) = Name::new(&p[1..]) else {
            return err(proto::E_BAD_REQUEST, 0);
        };
        // El dueño sale de la raíz, nunca del mensaje.
        let h = self
            .handles
            .open(owner, name, p[0] & proto::OPEN_WRITE != 0);
        self.new_handle(h)
    }

    /// Pedido por el handle `h`.
    fn on_handle(&mut self, h: usize, m: &Message) -> Message {
        let Some(x) = self.handles.get(h) else {
            return err(proto::E_BAD_REQUEST, 0);
        };
        match m.header.label {
            proto::STAT => {
                let (v, len) = self.stat(&x.name, x.owner);
                Message::new(proto::INFO, &proto::info(v, len, x.write)).unwrap_or_default()
            }
            proto::DUP_RO => {
                let n = self.handles.dup_ro(h);
                self.new_handle(n)
            }
            proto::READ => self.with_mem(m, |s, mem| s.read(x.owner, &x.name, mem)),
            proto::WRITE => {
                // Solo con un handle escribible (`Handles::writable`).
                if self.handles.writable(h).is_none() {
                    return self.with_mem(m, |_, _| Err(Fail(proto::E_READ_ONLY, 0)));
                }
                let expected = match u64_at(m.payload(), 0) {
                    proto::ANY_VERSION => None,
                    v => Some(v),
                };
                let len = u32_at(m.payload(), 8) as usize;
                self.with_mem(m, |s, mem| s.write(x.owner, &x.name, expected, len, mem))
            }
            _ => err(proto::E_BAD_REQUEST, 0),
        }
    }

    /// Corre `f` con el `Memory` adjunto mapeado y lo devuelve en la
    /// respuesta (pase lo que pase).
    fn with_mem(
        &mut self,
        m: &Message,
        f: impl FnOnce(&mut Self, Option<Mapped>) -> Result<Message, Fail>,
    ) -> Message {
        let [mem] = *m.cap_slice() else {
            for c in m.cap_slice() {
                let _ = cap_drop(*c);
            }
            return err(proto::E_BAD_REQUEST, 0);
        };
        let write_ok = m.header.label == proto::READ;
        let rights = if write_ok {
            Rights::READ | Rights::WRITE
        } else {
            Rights::READ
        };
        let mapped = mem_map(mem, CLIENT_VADDR, rights).ok();
        let mut r = match f(self, mapped) {
            Ok(r) => r,
            Err(Fail(code, v)) => err(code, v),
        };
        // Devolverlo también revoca el mapeo (lo hace el kernel).
        if r.push_cap(mem).is_err() {
            let _ = cap_drop(mem);
        }
        r
    }

    fn read(&mut self, owner: u32, name: &Name, mem: Option<Mapped>) -> Result<Message, Fail> {
        let mem = mem.ok_or(BAD)?;
        let e = self
            .store
            .get(owner, name)
            .ok_or(Fail(proto::E_NOT_FOUND, 0))?;
        if mem.len() < e.len as usize {
            return Err(Fail(proto::E_TOO_BIG, e.version));
        }
        let mut buf = [0u8; BLOCK];
        let mut left = e.len as usize;
        for i in 0..e.nblocks {
            self.store.read_block(&e, i, &mut buf)?;
            let n = left.min(BLOCK);
            mem.write_bytes(i as usize * BLOCK, &buf[..n]);
            left -= n;
        }
        let mut p = [0u8; 12];
        p[..8].copy_from_slice(&e.version.to_le_bytes());
        p[8..].copy_from_slice(&e.len.to_le_bytes());
        Message::new(proto::DATA, &p).map_err(|_| BAD)
    }

    fn write(
        &mut self,
        owner: u32,
        name: &Name,
        expected: Option<u64>,
        len: usize,
        mem: Option<Mapped>,
    ) -> Result<Message, Fail> {
        let mem = mem.ok_or(BAD)?;
        if mem.len() < len {
            return Err(BAD);
        }
        let v = self.store.put(owner, name, len, expected, |i, buf| {
            let off = i as usize * BLOCK;
            let n = (len - off).min(BLOCK);
            for (j, b) in buf[..n].iter_mut().enumerate() {
                *b = mem.read_u8(off + j);
            }
            Ok(())
        })?;
        Message::new(proto::WRITTEN, &v.to_le_bytes()).map_err(|_| BAD)
    }

    /// Atiende lo que haya en un canal. `Ok(false)` si no había nada,
    /// `Err(())` si el cliente cerró.
    fn poll(&mut self, ch: CapHandle, root: Option<u32>, h: usize) -> Result<bool, ()> {
        let mut m = Message::default();
        match channel_recv(ch, &mut m, NONBLOCK) {
            Ok(()) => {}
            Err(Error::WouldBlock) => return Ok(false),
            Err(_) => return Err(()),
        }
        let r = match root {
            Some(owner) => self.on_root(owner, &m),
            None => self.on_handle(h, &m),
        };
        match channel_send(ch, &r, 0) {
            Ok(()) => Ok(true),
            Err(_) => Err(()),
        }
    }

    fn run(&mut self) {
        loop {
            let mut idle = true;
            for i in 0..ROOTS {
                let Some(ch) = self.roots[i] else { continue };
                match self.poll(ch, Some(i as u32 + 1), 0) {
                    Ok(b) => idle &= !b,
                    Err(()) => {
                        let _ = cap_drop(ch);
                        self.roots[i] = None;
                    }
                }
            }
            for h in 0..MAX_HANDLES {
                if self.handles.get(h).is_none() {
                    continue;
                }
                match self.poll(self.eps[h], None, h) {
                    Ok(b) => idle &= !b,
                    Err(()) => {
                        let _ = cap_drop(self.eps[h]);
                        self.handles.close(h);
                    }
                }
            }
            if self.roots.iter().all(|r| r.is_none())
                && self.handles.open_indices().next().is_none()
            {
                return;
            }
            if idle {
                yield_now();
            }
        }
    }
}

nsos_rt::entry!(main);

fn main() -> i32 {
    let config = match mem_map(bootstrap(3), CONFIG_VADDR, Rights::READ) {
        Ok(m) => m,
        Err(e) => {
            println!("almacen: sin configuración: {:?}", e);
            return 2;
        }
    };
    let (mode, n, allow_format) = (config.read_u8(0), config.read_u8(1), config.read_u8(2) != 0);
    let buf = match mem_map(bootstrap(2), BUF_VADDR, Rights::READ | Rights::WRITE) {
        Ok(m) if m.len() >= BLOCK => m,
        _ => {
            println!("almacen: falta el búfer compartido con el driver");
            return 2;
        }
    };
    let mut disk = Disk {
        drv: bootstrap(1),
        buf,
        blocks: 0,
        crash_at: if mode == b'c' { n as u32 } else { 0 },
        armed: false,
        supers: 0,
    };
    let sectors = match disk.call(DRV_INFO, &[]) {
        Ok(r) if r.header.label == DRV_INFO => u64_at(r.payload(), 0),
        _ => {
            println!("almacen: el driver no responde");
            return 3;
        }
    };
    disk.blocks = (sectors / SECTORS_PER_BLOCK).min(u32::MAX as u64) as u32;
    let (mut store, info) = match Store::mount(disk, allow_format) {
        Ok(s) => s,
        Err(nsos_store::Error::Foreign) => {
            println!("almacen: el disco tiene otra cosa (¿FAT, particiones?); no lo toco");
            return 5;
        }
        Err(e) => {
            println!("almacen: no pude montar: {:?}", e);
            return 6;
        }
    };
    if info.formatted {
        println!("almacen: disco vacío, formateado");
    }
    if info.cleaned {
        println!("almacen: había una escritura sin terminar; quedó el estado anterior");
    }
    println!(
        "almacen: montado (secuencia {}, ranura {}, {} objetos)",
        info.seq, info.slot, info.objects
    );
    for e in store.entries() {
        println!(
            "almacen:   dueño {} \"{}\": versión {}, {} bytes",
            e.owner,
            core::str::from_utf8(e.name.as_bytes()).unwrap_or("?"),
            e.version,
            e.len
        );
    }
    store_arm(&mut store);
    let mut s = Server {
        store,
        roots: [Some(bootstrap(4)), Some(bootstrap(5))],
        handles: Handles::new(),
        eps: [CapHandle::INVALID; MAX_HANDLES],
    };
    s.run();
    println!("almacen: sin clientes, termino");
    0
}

/// Desde aquí cuentan las escrituras para la prueba de caídas.
fn store_arm(store: &mut Store<Disk>) {
    store.dev_mut().armed = true;
}
