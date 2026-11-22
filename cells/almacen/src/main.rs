//! Cell `almacen`: almacén de objetos propio de nsos, sobre el driver
//! `virtblk`.
//!
//! Guarda objetos con dueño, nombre y versión, con escritura copy-on-write
//! (`nsos_store`): una caída en cualquier momento deja el estado viejo o el
//! nuevo, nunca una mezcla. El acceso a cada objeto es una capacidad: un
//! extremo de canal ("handle") que se obtiene por la raíz del dueño y se
//! puede pasar a otra cell, entero o en copia de solo lectura. Protocolo:
//! `nsos_store::proto`, escrito como los contratos `store_root` (por las
//! raíces) y `store_obj` (por cada handle) de `nsos_contract::protocols`.
//! Modelos: `verification/lean/Nsos/Store.lean` y `StoreContract.lean`.
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

use nsos_rt::abi::{CapHandle, Message, Rights};
use nsos_rt::contract::protocols::{store_obj, store_root};
use nsos_rt::contract::{Chan, Endpoint, RecvError};
use nsos_rt::{
    bootstrap, channel_recv, channel_send, exit, mem_map, println, yield_now, Mapped, Sys,
};
use nsos_store::handles::{Handles, MAX_HANDLES};
use nsos_store::layout::Name;
use nsos_store::proto::{self, u64_at};
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

type Root = Chan<Sys, store_root::server::Ready>;
type Obj = Chan<Sys, store_obj::server::Idle>;

struct Server {
    store: Store<Disk>,
    roots: [Option<Root>; ROOTS],
    handles: Handles,
    /// El extremo del almacén de cada handle abierto. `None` también
    /// mientras se atiende un pedido por él.
    eps: [Option<Obj>; MAX_HANDLES],
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

/// Datos de un handle recién abierto, para `Opened`.
struct NewHandle {
    version: u64,
    len: u32,
    write: bool,
    handle: Endpoint<store_obj::client::Idle>,
}

/// Lo que pasó al mirar un canal.
enum Polled<C> {
    /// No había nada; el canal vuelve.
    Idle(C),
    /// Se atendió un pedido; el canal vuelve.
    Served(C),
    /// El cliente cerró.
    Closed,
}

impl Server {
    /// Abre un handle nuevo: canal propio, con el extremo del cliente para
    /// mandarlo en `Opened`.
    fn new_handle(&mut self, h: Option<usize>) -> Result<NewHandle, Fail> {
        let h = h.ok_or(Fail(proto::E_NO_HANDLES, 0))?;
        let Ok((theirs, mine)) = store_obj::pair(Sys) else {
            self.handles.close(h);
            return Err(Fail(proto::E_NO_HANDLES, 0));
        };
        self.eps[h] = Some(mine);
        let x = self.handles.get(h).expect("handle recién abierto");
        let (version, len) = self.stat(&x.name, x.owner);
        Ok(NewHandle {
            version,
            len,
            write: x.write,
            handle: theirs.into_endpoint(),
        })
    }

    fn stat(&self, name: &Name, owner: u32) -> (u64, u32) {
        self.store
            .get(owner, name)
            .map(|e| (e.version, e.len))
            .unwrap_or((0, 0))
    }

    /// Un pedido por la raíz del dueño `owner`.
    fn poll_root(&mut self, owner: u32, root: Root) -> Polled<Root> {
        let (opening, o) = match root.try_recv() {
            Ok(store_root::server::recv::Ready::Open(c, o)) => (c, o),
            Err(RecvError::WouldBlock(c)) => return Polled::Idle(c),
            Err(RecvError::Violation { chan, label }) => {
                println!(
                    "almacen: pedido fuera de protocolo por una raíz ({})",
                    label
                );
                return Polled::Served(chan);
            }
            Err(RecvError::Failed(_)) => return Polled::Closed,
        };
        // El dueño sale de la raíz: `Open` no tiene campo de dueño.
        let r = match Name::new(o.name.as_slice()) {
            Some(name) => {
                let h = self
                    .handles
                    .open(owner, name, o.flags & proto::OPEN_WRITE != 0);
                self.new_handle(h)
            }
            None => Err(BAD),
        };
        let sent = match r {
            Ok(n) => opening.send(store_root::Opened {
                version: n.version,
                len: n.len,
                write: n.write,
                handle: n.handle,
            }),
            Err(Fail(code, version)) => opening.send(store_root::Fail { code, version }),
        };
        match sent {
            Ok(c) => Polled::Served(c),
            Err(_) => Polled::Closed,
        }
    }

    /// Un pedido por el handle `h`.
    fn poll_handle(&mut self, h: usize, ch: Obj) -> Polled<Obj> {
        use store_obj::server::recv::Idle as Req;
        let Some(x) = self.handles.get(h) else {
            return Polled::Closed;
        };
        let sent = match ch.try_recv() {
            Err(RecvError::WouldBlock(c)) => return Polled::Idle(c),
            Err(RecvError::Violation { chan, label }) => {
                println!(
                    "almacen: pedido fuera de protocolo por un handle ({})",
                    label
                );
                return Polled::Served(chan);
            }
            Err(RecvError::Failed(_)) => return Polled::Closed,
            Ok(Req::Stat(c, _)) => {
                let (version, len) = self.stat(&x.name, x.owner);
                c.send(store_obj::Info {
                    version,
                    len,
                    write: x.write,
                })
            }
            Ok(Req::DupRo(c, _)) => {
                let n = self.handles.dup_ro(h);
                match self.new_handle(n) {
                    Ok(n) => c.send(store_obj::Opened {
                        version: n.version,
                        len: n.len,
                        write: n.write,
                        handle: n.handle,
                    }),
                    Err(Fail(code, version)) => c.send(store_obj::Fail { code, version }),
                }
            }
            Ok(Req::Read(c, r)) => {
                // El búfer vuelve en toda respuesta: el contrato no deja otra.
                let mapped = mem_map(r.buf.handle(), CLIENT_VADDR, Rights::READ | Rights::WRITE);
                match mapped
                    .map_err(|_| BAD)
                    .and_then(|m| self.read(x.owner, &x.name, m))
                {
                    Ok((version, len)) => c.send(store_obj::Data {
                        version,
                        len,
                        buf: r.buf,
                    }),
                    Err(Fail(code, version)) => c.send(store_obj::FailMem {
                        code,
                        version,
                        buf: r.buf,
                    }),
                }
            }
            Ok(Req::Write(c, w)) => {
                let result = if self.handles.writable(h).is_none() {
                    Err(Fail(proto::E_READ_ONLY, 0))
                } else {
                    let expected = match w.expected {
                        proto::ANY_VERSION => None,
                        v => Some(v),
                    };
                    mem_map(w.buf.handle(), CLIENT_VADDR, Rights::READ)
                        .map_err(|_| BAD)
                        .and_then(|m| self.write(x.owner, &x.name, expected, w.len as usize, m))
                };
                match result {
                    Ok(version) => c.send(store_obj::Written {
                        version,
                        buf: w.buf,
                    }),
                    Err(Fail(code, version)) => c.send(store_obj::FailMem {
                        code,
                        version,
                        buf: w.buf,
                    }),
                }
            }
        };
        match sent {
            Ok(c) => Polled::Served(c),
            Err(_) => Polled::Closed,
        }
    }

    fn read(&mut self, owner: u32, name: &Name, mem: Mapped) -> Result<(u64, u32), Fail> {
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
        Ok((e.version, e.len))
    }

    fn write(
        &mut self,
        owner: u32,
        name: &Name,
        expected: Option<u64>,
        len: usize,
        mem: Mapped,
    ) -> Result<u64, Fail> {
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
        Ok(v)
    }

    fn run(&mut self) {
        loop {
            let mut idle = true;
            for i in 0..ROOTS {
                let Some(ch) = self.roots[i].take() else {
                    continue;
                };
                self.roots[i] = match self.poll_root(i as u32 + 1, ch) {
                    Polled::Idle(c) => Some(c),
                    Polled::Served(c) => {
                        idle = false;
                        Some(c)
                    }
                    Polled::Closed => None,
                };
            }
            for h in 0..MAX_HANDLES {
                let Some(ch) = self.eps[h].take() else {
                    continue;
                };
                self.eps[h] = match self.poll_handle(h, ch) {
                    Polled::Idle(c) => Some(c),
                    Polled::Served(c) => {
                        idle = false;
                        Some(c)
                    }
                    Polled::Closed => {
                        self.handles.close(h);
                        None
                    }
                };
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
    let (Ok(r1), Ok(r2)) = (Root::open(Sys, bootstrap(4)), Root::open(Sys, bootstrap(5))) else {
        println!("almacen: las raíces no son canales");
        return 7;
    };
    let mut s = Server {
        store,
        roots: [Some(r1), Some(r2)],
        handles: Handles::new(),
        eps: [const { None }; MAX_HANDLES],
    };
    s.run();
    println!("almacen: sin clientes, termino");
    0
}

/// Desde aquí cuentan las escrituras para la prueba de caídas.
fn store_arm(store: &mut Store<Disk>) {
    store.dev_mut().armed = true;
}
