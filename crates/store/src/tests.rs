//! Tests en el host: uso normal, discos ajenos, handles y caídas simuladas
//! en cada combinación de escrituras hechas, no hechas o a medias (lo mismo
//! que cuantifica `Store.crash_safe` en el modelo de Lean).

use super::*;
use crate::handles::Handles;
use std::vec;
use std::vec::Vec;

const SECTOR: usize = 512;

#[derive(Clone)]
struct MemDev {
    blocks: Vec<[u8; BLOCK]>,
    /// Escrituras registradas (bloque, contenido), en orden.
    log: Vec<(u32, [u8; BLOCK])>,
    /// Si es `Some(n)`, falla a partir de la escritura `n`.
    fail_at: Option<usize>,
    writes: usize,
}

impl MemDev {
    fn new(n: usize) -> MemDev {
        MemDev {
            blocks: vec![[0; BLOCK]; n],
            log: Vec::new(),
            fail_at: None,
            writes: 0,
        }
    }
}

impl BlockDev for MemDev {
    fn blocks(&self) -> u32 {
        self.blocks.len() as u32
    }
    fn read(&mut self, blk: u32, buf: &mut [u8; BLOCK]) -> Result<(), DevError> {
        *buf = *self.blocks.get(blk as usize).ok_or(DevError)?;
        Ok(())
    }
    fn write(&mut self, blk: u32, buf: &[u8; BLOCK]) -> Result<(), DevError> {
        if self.fail_at.is_some_and(|n| self.writes >= n) {
            return Err(DevError);
        }
        self.writes += 1;
        *self.blocks.get_mut(blk as usize).ok_or(DevError)? = *buf;
        self.log.push((blk, *buf));
        Ok(())
    }
    fn flush(&mut self) -> Result<(), DevError> {
        Ok(())
    }
}

fn name(s: &str) -> Name {
    Name::new(s.as_bytes()).unwrap()
}

fn put_bytes(st: &mut Store<MemDev>, owner: u32, n: &str, data: &[u8]) -> Result<u64, Error> {
    st.put(owner, &name(n), data.len(), None, |i, buf| {
        let off = i as usize * BLOCK;
        let end = (off + BLOCK).min(data.len());
        buf[..end - off].copy_from_slice(&data[off..end]);
        Ok(())
    })
}

fn read_all(st: &mut Store<MemDev>, e: &Entry) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; BLOCK];
    for i in 0..e.nblocks {
        st.read_block(e, i, &mut buf).unwrap();
        out.extend_from_slice(&buf);
    }
    out.truncate(e.len as usize);
    out
}

/// Catálogo completo, para comparar: (dueño, nombre, versión, datos).
type Catalog = Vec<(u32, Vec<u8>, u64, Vec<u8>)>;

fn catalog(st: &mut Store<MemDev>) -> Catalog {
    let es: Vec<Entry> = st.entries().to_vec();
    let mut c: Catalog = es
        .iter()
        .map(|e| {
            (
                e.owner,
                e.name.as_bytes().to_vec(),
                e.version,
                read_all(st, e),
            )
        })
        .collect();
    c.sort();
    c
}

fn pattern(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| seed.wrapping_add((i / 7) as u8)).collect()
}

fn fresh() -> (Store<MemDev>, MountInfo) {
    Store::mount(MemDev::new(64), true).unwrap()
}

#[test]
fn format_put_get() {
    let (mut st, info) = fresh();
    assert!(info.formatted);
    assert_eq!(put_bytes(&mut st, 1, "a", b"hola").unwrap(), 1);
    assert_eq!(put_bytes(&mut st, 1, "a", b"chau").unwrap(), 2);
    let big = pattern(3, 3 * BLOCK + 100);
    assert_eq!(put_bytes(&mut st, 1, "grande", &big).unwrap(), 1);
    assert_eq!(put_bytes(&mut st, 2, "a", b"otro dueno").unwrap(), 1);
    let e = st.get(1, &name("a")).unwrap();
    assert_eq!(e.version, 2);
    assert_eq!(read_all(&mut st, &e), b"chau");
    let e = st.get(1, &name("grande")).unwrap();
    assert_eq!(read_all(&mut st, &e), big);
    // Mismo nombre, otro dueño: otro objeto.
    let e = st.get(2, &name("a")).unwrap();
    assert_eq!(read_all(&mut st, &e), b"otro dueno");
    // Volver a montar da lo mismo.
    let before = catalog(&mut st);
    let (mut st2, info) = Store::mount(st.into_dev(), false).unwrap();
    assert!(!info.formatted && !info.cleaned);
    assert_eq!(catalog(&mut st2), before);
}

#[test]
fn versions_and_compare_and_swap() {
    let (mut st, _) = fresh();
    let n = name("x");
    let w = |st: &mut Store<MemDev>, exp| {
        st.put(1, &n, 1, exp, |_, b| {
            b[0] = 9;
            Ok(())
        })
    };
    assert_eq!(w(&mut st, Some(1)), Err(Error::VersionMismatch(0)));
    assert_eq!(w(&mut st, Some(0)), Ok(1));
    assert_eq!(w(&mut st, Some(0)), Err(Error::VersionMismatch(1)));
    assert_eq!(w(&mut st, Some(1)), Ok(2));
    assert_eq!(w(&mut st, None), Ok(3));
}

#[test]
fn limits() {
    let (mut st, _) = fresh();
    let too_big = MAX_OBJECT_BLOCKS as usize * BLOCK + 1;
    assert_eq!(
        st.put(1, &name("x"), too_big, None, |_, _| Ok(())),
        Err(Error::TooBig)
    );
    for i in 0..MAX_OBJECTS {
        put_bytes(&mut st, 1, &std::format!("o{i}"), b"").unwrap();
    }
    assert_eq!(
        put_bytes(&mut st, 1, "otro", b""),
        Err(Error::TooManyObjects)
    );
    // Disco chico: sin lugar para la copia nueva.
    let (mut st, _) = Store::mount(MemDev::new(8), true).unwrap();
    put_bytes(&mut st, 1, "x", &pattern(1, 2 * BLOCK)).unwrap();
    assert_eq!(
        put_bytes(&mut st, 1, "y", &pattern(1, 3 * BLOCK)),
        Err(Error::NoSpace)
    );
    // El estado sigue intacto.
    let e = st.get(1, &name("x")).unwrap();
    assert_eq!(read_all(&mut st, &e), pattern(1, 2 * BLOCK));
}

#[test]
fn foreign_disk_is_never_touched() {
    let mut dev = MemDev::new(64);
    // Sector de arranque de un FAT32.
    dev.blocks[0][..11].copy_from_slice(b"\xEB\x58\x90MSDOS5.0");
    dev.blocks[0][510] = 0x55;
    dev.blocks[0][511] = 0xAA;
    let before = dev.blocks.clone();
    let r = Store::mount(dev.clone(), true);
    assert!(matches!(r, Err(Error::Foreign)));
    assert_eq!(dev.blocks, before);
    // Vacío y sin permiso para formatear: no escribe nada.
    assert!(matches!(
        Store::mount(MemDev::new(64), false),
        Err(Error::Unformatted)
    ));
}

#[derive(Clone, Copy, Debug)]
enum Out {
    Skip,
    Full,
    /// El primer sector nuevo, el resto viejo.
    TornHead,
    /// El primer sector viejo, el resto nuevo.
    TornTail,
}
const OUTS: [Out; 4] = [Out::Skip, Out::Full, Out::TornHead, Out::TornTail];

fn apply(base: &MemDev, log: &[(u32, [u8; BLOCK])], outs: &[Out]) -> MemDev {
    let mut d = base.clone();
    d.log.clear();
    for ((blk, data), o) in log.iter().zip(outs) {
        let b = &mut d.blocks[*blk as usize];
        match o {
            Out::Skip => {}
            Out::Full => *b = *data,
            Out::TornHead => b[..SECTOR].copy_from_slice(&data[..SECTOR]),
            Out::TornTail => b[SECTOR..].copy_from_slice(&data[SECTOR..]),
        }
    }
    d
}

/// Todas las combinaciones de resultados para `n` escrituras.
fn all_outcomes(n: usize) -> Vec<Vec<Out>> {
    let mut v = vec![Vec::new()];
    for _ in 0..n {
        v = v
            .into_iter()
            .flat_map(|p| OUTS.iter().map(move |o| [p.clone(), vec![*o]].concat()))
            .collect();
    }
    v
}

/// Graba las escrituras de `op` sobre el almacén montado en `base`.
fn record(
    base: &MemDev,
    op: impl FnOnce(&mut Store<MemDev>),
) -> (Catalog, Catalog, Vec<(u32, [u8; BLOCK])>) {
    let (mut st, _) = Store::mount(base.clone(), false).unwrap();
    let old = catalog(&mut st);
    st.dev.log.clear();
    op(&mut st);
    let new = catalog(&mut st);
    let log = st.into_dev().log;
    (old, new, log)
}

/// Para cada combinación: montar da el viejo o el nuevo, y después se puede
/// seguir escribiendo y volver a montar.
fn check_every_crash(base: &MemDev, op: impl FnOnce(&mut Store<MemDev>)) -> (usize, usize) {
    let (old, new, log) = record(base, op);
    assert_ne!(old, new);
    let (mut saw_old, mut saw_new) = (0, 0);
    for outs in all_outcomes(log.len()) {
        let d = apply(base, &log, &outs);
        let (mut st, _) =
            Store::mount(d, false).unwrap_or_else(|e| panic!("no montó con {outs:?}: {e:?}"));
        let got = catalog(&mut st);
        if got == old {
            saw_old += 1;
        } else if got == new {
            saw_new += 1;
        } else {
            panic!("mezcla con {outs:?}: {got:?}");
        }
        // Seguir escribiendo después de la caída.
        put_bytes(&mut st, 7, "despues", b"ok").unwrap();
        let after = catalog(&mut st);
        let (mut st2, _) = Store::mount(st.into_dev(), false).unwrap();
        assert_eq!(catalog(&mut st2), after);
    }
    (saw_old, saw_new)
}

fn populated() -> MemDev {
    let (mut st, _) = fresh();
    put_bytes(&mut st, 1, "a", &pattern(1, BLOCK + 10)).unwrap();
    put_bytes(&mut st, 1, "b", b"bbbb").unwrap();
    put_bytes(&mut st, 2, "a", &pattern(9, 300)).unwrap();
    st.into_dev()
}

#[test]
fn crash_anywhere_updating_an_object() {
    let base = populated();
    let (o, n) = check_every_crash(&base, |st| {
        put_bytes(st, 1, "a", &pattern(5, 2 * BLOCK + 1)).unwrap();
    });
    // 3 bloques de datos + tabla + superbloque: 4^5 combinaciones.
    assert_eq!(o + n, 1024);
    assert!(o > 0 && n > 0);
}

#[test]
fn crash_anywhere_creating_an_object() {
    let base = populated();
    let (o, n) = check_every_crash(&base, |st| {
        put_bytes(st, 2, "nuevo", &pattern(7, 10)).unwrap();
    });
    assert!(o > 0 && n > 0);
}

#[test]
fn crash_anywhere_formatting() {
    // Formatear también: con cualquier caída, el disco vuelve a montar
    // (formateando) o queda formateado.
    let (_, log) = {
        let (st, _) = fresh();
        ((), st.into_dev().log)
    };
    for outs in all_outcomes(log.len()) {
        let d = apply(&MemDev::new(64), &log, &outs);
        let (mut st, _) =
            Store::mount(d, true).unwrap_or_else(|e| panic!("no montó con {outs:?}: {e:?}"));
        assert!(catalog(&mut st).is_empty());
    }
}

/// El ejemplo `Store.stale_resurrects` del modelo, en el código: una
/// escritura que se cae con la tabla y el superbloque hechos pero sin los
/// datos deja una ranura que no valida. Al montar se borra; si no, la
/// escritura siguiente podría completarla.
#[test]
fn stale_slot_is_cleaned_and_never_resurrects() {
    let (st, _) = fresh();
    let base = st.into_dev();
    let data = pattern(4, 100);
    let (_, _, log) = record(&base, |st| {
        put_bytes(st, 1, "fantasma", &data).unwrap();
    });
    assert_eq!(log.len(), 3); // datos, tabla, superbloque
    let crashed = apply(&base, &log, &[Out::Skip, Out::Full, Out::Full]);
    let (mut st, info) = Store::mount(crashed, false).unwrap();
    assert!(info.cleaned);
    assert!(catalog(&mut st).is_empty());
    // Otra escritura con los mismos datos, que cae en el mismo bloque.
    let base2 = st.into_dev();
    let (_, _, log2) = record(&base2, |st| {
        put_bytes(st, 1, "otro", &data).unwrap();
    });
    assert_eq!(log2[0].0, log[0].0);
    for outs in all_outcomes(log2.len()) {
        let (st, _) = Store::mount(apply(&base2, &log2, &outs), false).unwrap();
        assert!(st.get(1, &name("fantasma")).is_none());
    }
}

#[test]
fn device_errors_leave_the_committed_state() {
    let base = populated();
    // 2 bloques de datos + tabla + superbloque: que falle cualquiera.
    for fail_at in 0..4 {
        let (mut st, _) = Store::mount(base.clone(), false).unwrap();
        let old = catalog(&mut st);
        st.dev.fail_at = Some(st.dev.writes + fail_at);
        let r = put_bytes(&mut st, 1, "a", &pattern(5, 2 * BLOCK));
        assert_eq!(r, Err(Error::Dev));
        assert_eq!(catalog(&mut st), old);
        // Al volver el dispositivo, se sigue sin mezclar nada.
        st.dev.fail_at = None;
        put_bytes(&mut st, 1, "b", b"nuevo b").unwrap();
        let after = catalog(&mut st);
        let (mut st2, _) = Store::mount(st.into_dev(), false).unwrap();
        assert_eq!(catalog(&mut st2), after);
    }
}

/// Muchas escrituras al azar con caídas al azar en el medio (semilla fija).
#[test]
fn random_crashes() {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    let (st, _) = Store::mount(MemDev::new(40), true).unwrap();
    let mut dev = st.into_dev();
    for round in 0..300 {
        let (mut st, _) = Store::mount(dev, false).unwrap();
        let old = catalog(&mut st);
        st.dev.log.clear();
        let base = st.dev.clone();
        let owner = rnd(2) as u32 + 1;
        let obj = std::format!("o{}", rnd(5));
        let data = pattern(round as u8, rnd(3 * BLOCK as u64) as usize);
        let r = put_bytes(&mut st, owner, &obj, &data);
        let new = catalog(&mut st);
        let log = st.into_dev().log;
        if r.is_err() {
            assert_eq!(r, Err(Error::NoSpace));
            dev = base;
            continue;
        }
        let outs: Vec<Out> = log.iter().map(|_| OUTS[rnd(4) as usize]).collect();
        dev = apply(&base, &log, &outs);
        let (mut st, _) = Store::mount(dev, false).unwrap();
        let got = catalog(&mut st);
        assert!(got == old || got == new, "ronda {round}: mezcla");
        dev = st.into_dev();
    }
}

#[test]
fn handles_isolate_owners() {
    let mut h = Handles::new();
    // La raíz del dueño 2 abre "x": es el "x" del dueño 2, no el del 1.
    let a = h.open(1, name("x"), true).unwrap();
    let b = h.open(2, name("x"), true).unwrap();
    assert_eq!(h.get(a).unwrap().owner, 1);
    assert_eq!(h.get(b).unwrap().owner, 2);
    // Una copia de solo lectura no escribe; copiarla otra vez tampoco.
    let ro = h.dup_ro(a).unwrap();
    assert!(h.writable(ro).is_none());
    assert_eq!(h.get(ro).unwrap().owner, 1);
    let ro2 = h.dup_ro(ro).unwrap();
    assert!(h.writable(ro2).is_none());
    assert!(h.writable(a).is_some());
    // Abierto sin escritura tampoco escribe.
    let r = h.open(1, name("x"), false).unwrap();
    assert!(h.writable(r).is_none());
    // Cerrado, deja de valer; un índice cualquiera no vale.
    h.close(a);
    assert!(h.get(a).is_none());
    assert!(h.writable(a).is_none());
    assert!(h.get(9999).is_none());
    assert!(h.dup_ro(9999).is_none());
}
