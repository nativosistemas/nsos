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
//! Capacidades de arranque: 1. raíz del dueño 1 en el almacén;
//! 2. configuración (`[modo, n, ...]`, solo lectura); 3. canal con
//! `curioso` (en los modos `e` y `v`).
//!
//! Sale con 0 si todo salió como se esperaba, 1 si encontró datos
//! inconsistentes y 4 si el almacén se cayó.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::{CapHandle, Message, Rights};
use nsos_rt::{bootstrap, channel_recv, channel_send, mem_create, mem_map, println};
use nsos_store::proto::{self, demo_word, u32_at, u64_at};

const OBJECT: &[u8] = b"contador";
const SIZE: usize = 16 * 1024;
const WORDS: usize = SIZE / 8;
/// Dónde se mapea el búfer para leer y para escribir. Son distintos porque
/// el mapeo de lectura sigue hasta que el búfer se vuelve a enviar (enviarlo
/// lo revoca; todavía no hay `mem_unmap`).
const READ_VADDR: u64 = 0x4000_0000;
const WRITE_VADDR: u64 = 0x4200_0000;
const CONFIG_VADDR: u64 = 0x4100_0000;
const SHARE: u64 = 1;
const RESULT: u64 = 2;

/// El almacén se cayó (cerró el canal).
struct Gone;

fn call(ch: CapHandle, m: &Message) -> Result<Message, Gone> {
    channel_send(ch, m, 0).map_err(|_| Gone)?;
    let mut r = Message::default();
    channel_recv(ch, &mut r, 0).map_err(|_| Gone)?;
    Ok(r)
}

struct Client {
    handle: CapHandle,
    mem: CapHandle,
    version: u64,
}

impl Client {
    /// Lee el objeto y comprueba que todas sus palabras sean de su versión.
    fn verify(&mut self) -> Result<bool, Gone> {
        let mut m = Message::new(proto::READ, &[]).map_err(|_| Gone)?;
        m.push_cap(self.mem).map_err(|_| Gone)?;
        let r = call(self.handle, &m)?;
        let [mem] = *r.cap_slice() else {
            return Err(Gone);
        };
        self.mem = mem;
        if r.header.label != proto::DATA {
            println!("escritor: no pude leer (código {})", u32_at(r.payload(), 0));
            return Ok(false);
        }
        let (v, len) = (u64_at(r.payload(), 0), u32_at(r.payload(), 8) as usize);
        let Ok(map) = mem_map(self.mem, READ_VADDR, Rights::READ) else {
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
        let map = mem_map(self.mem, WRITE_VADDR, Rights::READ | Rights::WRITE).map_err(|_| Gone)?;
        for k in 0..WORDS {
            map.write_u64(k * 8, demo_word(v, k));
        }
        let mut p = [0u8; 12];
        p[..8].copy_from_slice(&self.version.to_le_bytes());
        p[8..].copy_from_slice(&(SIZE as u32).to_le_bytes());
        let mut m = Message::new(proto::WRITE, &p).map_err(|_| Gone)?;
        m.push_cap(self.mem).map_err(|_| Gone)?;
        let r = call(self.handle, &m)?;
        let [mem] = *r.cap_slice() else {
            return Err(Gone);
        };
        self.mem = mem;
        if r.header.label != proto::WRITTEN || u64_at(r.payload(), 0) != v {
            println!(
                "escritor: la escritura de la versión {} falló (código {})",
                v,
                u32_at(r.payload(), 0)
            );
            return Ok(false);
        }
        self.version = v;
        Ok(true)
    }
}

/// Le da a `curioso` una copia de solo lectura y espera su resultado.
fn share(c: &mut Client, peer: CapHandle) -> Result<bool, Gone> {
    let r = call(
        c.handle,
        &Message::new(proto::DUP_RO, &[]).map_err(|_| Gone)?,
    )?;
    let [ro] = *r.cap_slice() else {
        return Ok(false);
    };
    let mut m = Message::new(SHARE, &[]).map_err(|_| Gone)?;
    m.push_cap(ro).map_err(|_| Gone)?;
    if channel_send(peer, &m, 0).is_err() {
        println!("escritor: no está el curioso");
        return Ok(false);
    }
    let mut r = Message::default();
    let ok = channel_recv(peer, &mut r, 0).is_ok()
        && r.header.label == RESULT
        && r.payload().first() == Some(&1);
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
    let root = bootstrap(1);
    let mut p = [0u8; 1 + OBJECT.len()];
    p[0] = proto::OPEN_WRITE;
    p[1..].copy_from_slice(OBJECT);
    let r = call(root, &Message::new(proto::OPEN, &p).map_err(|_| Gone)?)?;
    let [handle] = *r.cap_slice() else {
        println!("escritor: no pude abrir \"contador\"");
        return Ok(1);
    };
    let mem = mem_create(SIZE).map_err(|_| Gone)?;
    let mut c = Client {
        handle,
        mem,
        version: u64_at(r.payload(), 0),
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
            Ok(if share(&mut c, bootstrap(3))? { 0 } else { 1 })
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
