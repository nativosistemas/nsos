//! Cell `curioso`: otro cliente del almacén (dueño 2), para probar el
//! aislamiento entre dueños.
//!
//! 1. Abre "contador" por **su** raíz: es su propio objeto, no el del
//!    `escritor` (dueño 1), aunque se llame igual. Lo escribe.
//! 2. Recibe del `escritor` una copia de solo lectura del objeto de él.
//!    Puede leerla (y comprobar que está entera), pero escribir con ella se
//!    rechaza, y copiarla de nuevo da otra de solo lectura.
//!
//! Capacidades de arranque: 1. raíz del dueño 2 (cliente de `store_root`);
//! 2. canal con `escritor` (servidor de `store_share`). Le contesta
//! `Verdict { ok: true }` al escritor si todo fue como se esperaba y sale
//! con 0; si no, `ok: false` y sale con 1.

#![no_std]
#![no_main]
#![deny(unsafe_code)]

use nsos_rt::abi::Rights;
use nsos_rt::contract::protocols::{store_obj, store_root, store_share};
use nsos_rt::contract::{Bytes, Chan, Mem, RW};
use nsos_rt::{bootstrap, mem_create, mem_map, println, Sys};
use nsos_store::proto::{self, demo_word};

const MEM_VADDR: u64 = 0x4000_0000;
const SIZE: usize = 16 * 1024;

type Handle = Chan<Sys, store_obj::client::Idle>;

fn check(what: &str, ok: bool) -> bool {
    println!("curioso: {} ... {}", what, if ok { "ok" } else { "FALLO" });
    ok
}

/// `Stat`: devuelve el handle y si dice que escribe.
fn stat(h: Handle) -> Option<(Handle, u64, bool)> {
    match h.call(store_obj::Stat {}).ok()? {
        store_obj::client::recv::Asking::Info(h, i) => Some((h, i.version, i.write)),
        store_obj::client::recv::Asking::Fail(..) => None,
    }
}

/// Las pruebas. Devuelve el canal con el escritor (en `Waiting`, para
/// contestarle) y si todo salió bien.
fn run(
    peer: Chan<Sys, store_share::server::Start>,
) -> Option<(Chan<Sys, store_share::server::Waiting>, bool)> {
    let root = Chan::<Sys, store_root::client::Ready>::open(Sys, bootstrap(1)).ok()?;
    let mut ok = true;

    // 1. Mi "contador" (dueño 2) es otro objeto.
    let open = store_root::Open {
        flags: proto::OPEN_WRITE,
        name: Bytes::new(b"contador")?,
    };
    let store_root::client::recv::Opening::Opened(_root, o) = root.call(open).ok()? else {
        return None;
    };
    let (mine, v0): (Handle, u64) = (Chan::attach(Sys, o.handle), o.version);
    let buf = Mem::<RW>::open(Sys, mem_create(SIZE).ok()?).ok()?;
    let map = mem_map(buf.handle(), MEM_VADDR, Rights::READ | Rights::WRITE).ok()?;
    map.write_bytes(0, b"soy el curioso");
    let w = store_obj::Write {
        expected: proto::ANY_VERSION,
        len: 14,
        buf,
    };
    let (written, buf) = match mine.call(w).ok()? {
        store_obj::client::recv::Writing::Written(_mine, w) => (w.version == v0 + 1, w.buf),
        store_obj::client::recv::Writing::FailMem(_mine, f) => (false, f.buf),
    };
    ok &= check(
        "mi \"contador\" es mío (otro dueño, otro objeto) y lo puedo escribir",
        written,
    );

    // 2. La copia de solo lectura del escritor.
    let store_share::server::recv::Start::Share(peer, s) = peer.recv().ok()?;
    ok &= check("me llegó una copia de solo lectura", true);
    let ro: Handle = Chan::attach(Sys, s.handle);
    let (ro, v, write) = stat(ro)?;
    ok &= check("la copia dice que no escribe", !write);
    let (ro, buf) = match ro.call(store_obj::Read { buf }).ok()? {
        store_obj::client::recv::Reading::Data(ro, d) => {
            let len = d.len as usize;
            let map = mem_map(d.buf.handle(), MEM_VADDR, Rights::READ).ok()?;
            let entero = (0..len / 8).all(|k| map.read_u64(k * 8) == demo_word(v, k));
            ok &= check(
                "puedo leer el objeto del escritor, entero",
                len == SIZE && entero && d.version == v,
            );
            (ro, d.buf)
        }
        store_obj::client::recv::Reading::FailMem(ro, f) => {
            ok &= check("puedo leer el objeto del escritor, entero", false);
            (ro, f.buf)
        }
    };
    let w = store_obj::Write {
        expected: proto::ANY_VERSION,
        len: 8,
        buf,
    };
    let ro = match ro.call(w).ok()? {
        store_obj::client::recv::Writing::FailMem(ro, f) => {
            ok &= check(
                "escribir con la copia de solo lectura: rechazado",
                f.code == proto::E_READ_ONLY,
            );
            f.buf.release(Sys);
            ro
        }
        store_obj::client::recv::Writing::Written(ro, w) => {
            ok &= check("escribir con la copia de solo lectura: rechazado", false);
            w.buf.release(Sys);
            ro
        }
    };
    let store_obj::client::recv::Duping::Opened(_ro, o) = ro.call(store_obj::DupRo {}).ok()? else {
        return None;
    };
    let (_ro2, _, write2) = stat(Chan::attach(Sys, o.handle))?;
    ok &= check(
        "copiar la copia da otra de solo lectura",
        !o.write && !write2,
    );
    Some((peer, ok))
}

nsos_rt::entry!(main);

fn main() -> i32 {
    let Ok(peer) = Chan::<Sys, store_share::server::Start>::open(Sys, bootstrap(2)) else {
        return 1;
    };
    let ok = match run(peer) {
        Some((peer, ok)) => {
            peer.send(store_share::Verdict { ok })
                .map(|end| end.close())
                .is_ok()
                && ok
        }
        None => false,
    };
    if ok {
        0
    } else {
        println!("curioso: algo no salió como se esperaba");
        1
    }
}
