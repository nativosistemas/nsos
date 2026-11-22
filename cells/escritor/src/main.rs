//! Cell `escritor`: cliente de prueba del almacén (dueño 1).
//!
//! Abre su objeto `contador` (16 KiB) y, si ya existe, comprueba que esté
//! entero: todas sus palabras tienen que ser las de su versión
//! (`nsos_store::proto::demo_word`). Una mezcla de dos escrituras se notaría.
//! Después, según el modo:
//! * `e n`: escribe `n` versiones nuevas y comparte una copia de solo
//!   lectura con `curioso`, que intenta escribirla.
//! * `v`: solo verifica (y comparte con `curioso`).
//! * `c n`: escribe hasta que el almacén se cae (prueba de caídas).
//! * `s`: escribe sin parar, hasta que maten a QEMU.
//!
//! Capacidades de arranque: 1. raíz del dueño 1 en el almacén (cliente de
//! `store_root`); 2. configuración (`[modo, n, ...]`, solo lectura);
//! 3. canal con `curioso` (cliente de `store_share`, en los modos `e` y
//! `v`). El handle del objeto es un `store_obj`.
//!
//! Sale con 0 si todo salió como se esperaba, 1 si encontró datos
//! inconsistentes y 4 si el almacén se cayó.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::Rights;
use nsos_rt::contract::protocols::{store_obj, store_root, store_share};
use nsos_rt::contract::{Bytes, Chan, Mem, RW};
use nsos_rt::{bootstrap, mem_create, mem_map, println, Sys};
use nsos_store::proto::{self, demo_word};

const OBJECT: &[u8] = b"contador";
const SIZE: usize = 16 * 1024;
const WORDS: usize = SIZE / 8;
/// Dónde se mapea el búfer para leer y para escribir. Son distintos porque
/// el mapeo de lectura sigue hasta que el búfer se vuelve a enviar (enviarlo
/// lo revoca).
const READ_VADDR: u64 = 0x4000_0000;
const WRITE_VADDR: u64 = 0x4200_0000;
const CONFIG_VADDR: u64 = 0x4100_0000;

/// El almacén se cayó (cerró el canal) o contestó fuera de protocolo.
struct Gone;

type Handle = Chan<Sys, store_obj::client::Idle>;

struct Client {
    /// `None` solo mientras hay un pedido en curso.
    handle: Option<Handle>,
    /// El búfer: siempre vuelve con la respuesta (`Mem<RW>` en todas).
    buf: Option<Mem<RW>>,
    version: u64,
}

impl Client {
    fn take(&mut self) -> Result<(Handle, Mem<RW>), Gone> {
        Ok((
            self.handle.take().ok_or(Gone)?,
            self.buf.take().ok_or(Gone)?,
        ))
    }

    /// Lee el objeto y comprueba que todas sus palabras sean de su versión.
    fn verify(&mut self) -> Result<bool, Gone> {
        let (h, buf) = self.take()?;
        let (v, len) = match h.call(store_obj::Read { buf }) {
            Ok(store_obj::client::recv::Reading::Data(h, d)) => {
                (self.handle, self.buf) = (Some(h), Some(d.buf));
                (d.version, d.len as usize)
            }
            Ok(store_obj::client::recv::Reading::FailMem(h, f)) => {
                (self.handle, self.buf) = (Some(h), Some(f.buf));
                println!("escritor: no pude leer (código {})", f.code);
                return Ok(false);
            }
            Err(_) => return Err(Gone),
        };
        let Some(buf) = &self.buf else {
            return Err(Gone);
        };
        let Ok(map) = mem_map(buf.handle(), READ_VADDR, Rights::READ) else {
            return Ok(false);
        };
        let bad = (0..WORDS)
            .filter(|&k| map.read_u64(k * 8) != demo_word(v, k))
            .count();
        if len != SIZE || bad != 0 {
            println!(
                "escritor: \"contador\" versión {}: INCONSISTENTE ({} de {} palabras mal, {} bytes)",
                v, bad, WORDS, len
            );
            return Ok(false);
        }
        self.version = v;
        Ok(true)
    }

    /// Escribe la versión siguiente. `Ok(false)` si el almacén la rechazó.
    fn write_next(&mut self) -> Result<bool, Gone> {
        let v = self.version + 1;
        let (h, buf) = self.take()?;
        let map =
            mem_map(buf.handle(), WRITE_VADDR, Rights::READ | Rights::WRITE).map_err(|_| Gone)?;
        for k in 0..WORDS {
            map.write_u64(k * 8, demo_word(v, k));
        }
        let req = store_obj::Write {
            expected: self.version,
            len: SIZE as u32,
            buf,
        };
        match h.call(req) {
            Ok(store_obj::client::recv::Writing::Written(h, w)) => {
                (self.handle, self.buf) = (Some(h), Some(w.buf));
                if w.version != v {
                    println!(
                        "escritor: el almacén dice versión {} (esperaba {})",
                        w.version, v
                    );
                    return Ok(false);
                }
            }
            Ok(store_obj::client::recv::Writing::FailMem(h, f)) => {
                (self.handle, self.buf) = (Some(h), Some(f.buf));
                println!(
                    "escritor: la escritura de la versión {} falló (código {})",
                    v, f.code
                );
                return Ok(false);
            }
            Err(_) => return Err(Gone),
        }
        self.version = v;
        Ok(true)
    }
}

/// Le da a `curioso` una copia de solo lectura y espera su resultado.
fn share(c: &mut Client) -> Result<bool, Gone> {
    let h = c.handle.take().ok_or(Gone)?;
    let ro = match h.call(store_obj::DupRo {}) {
        Ok(store_obj::client::recv::Duping::Opened(h, o)) => {
            c.handle = Some(h);
            o.handle
        }
        Ok(store_obj::client::recv::Duping::Fail(h, f)) => {
            c.handle = Some(h);
            println!("escritor: no pude copiar el handle (código {})", f.code);
            return Ok(false);
        }
        Err(_) => return Err(Gone),
    };
    let Ok(peer) = Chan::<Sys, store_share::client::Start>::open(Sys, bootstrap(3)) else {
        println!("escritor: no está el curioso");
        return Ok(false);
    };
    let ok = match peer.call(store_share::Share { handle: ro }) {
        Ok(store_share::client::recv::Waiting::Verdict(end, v)) => {
            end.close();
            v.ok
        }
        Err(_) => false,
    };
    // El objeto tiene que seguir igual.
    let before = c.version;
    let same = c.verify()? && c.version == before;
    if ok && same {
        println!(
            "escritor: el curioso leyó mi objeto pero no pudo cambiarlo (sigue en la versión {})",
            before
        );
    }
    Ok(ok && same)
}

fn run(mode: u8, n: u8) -> Result<i32, Gone> {
    let root = Chan::<Sys, store_root::client::Ready>::open(Sys, bootstrap(1)).map_err(|_| Gone)?;
    let open = store_root::Open {
        flags: proto::OPEN_WRITE,
        name: Bytes::new(OBJECT).ok_or(Gone)?,
    };
    let (handle, version) = match root.call(open) {
        Ok(store_root::client::recv::Opening::Opened(_root, o)) => {
            (Chan::attach(Sys, o.handle), o.version)
        }
        Ok(store_root::client::recv::Opening::Fail(..)) => {
            println!("escritor: no pude abrir \"contador\"");
            return Ok(1);
        }
        Err(_) => return Err(Gone),
    };
    let buf = mem_create(SIZE)
        .and_then(|h| Mem::<RW>::open(Sys, h))
        .map_err(|_| Gone)?;
    let mut c = Client {
        handle: Some(handle),
        buf: Some(buf),
        version,
    };
    if c.version == 0 {
        println!("escritor: \"contador\" todavía no existe");
    } else if c.verify()? {
        println!(
            "escritor: recuperé \"contador\" en la versión {}: consistente ({} bytes)",
            c.version, SIZE
        );
    } else {
        return Ok(1);
    }
    match mode {
        b'e' | b'v' => {
            let count = if mode == b'e' { n } else { 0 };
            for _ in 0..count {
                if !c.write_next()? {
                    return Ok(1);
                }
                println!("escritor: escribí la versión {}", c.version);
            }
            Ok(if share(&mut c)? { 0 } else { 1 })
        }
        b'c' | b's' => loop {
            if !c.write_next()? {
                return Ok(1);
            }
            println!("escritor: escribí la versión {}", c.version);
        },
        _ => {
            println!("escritor: modo desconocido");
            Ok(1)
        }
    }
}

nsos_rt::entry!(main);

fn main() -> i32 {
    let (mode, n) = match mem_map(bootstrap(2), CONFIG_VADDR, Rights::READ) {
        Ok(m) => (m.read_u8(0), m.read_u8(1)),
        Err(e) => {
            println!("escritor: sin configuración: {:?}", e);
            return 2;
        }
    };
    match run(mode, n) {
        Ok(code) => code,
        Err(Gone) => {
            println!("escritor: el almacén se cayó");
            4
        }
    }
}
