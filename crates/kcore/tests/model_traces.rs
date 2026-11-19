//! Tests derivados del modelo formal en Lean (`verification/lean`).
//!
//! `lake exe tracegen` ejecuta el modelo de la CSpace con miles de
//! operaciones y anota el resultado que predice cada una. Aquí se repiten las
//! mismas operaciones sobre la `CSpace` real y se exige el mismo resultado:
//! handles, errores, objetos, derechos y cantidad de capacidades vivas. Así
//! las propiedades demostradas sobre el modelo valen también para este código
//! (en las secuencias probadas).

use nsos_abi::cap::CapHandle;
use nsos_abi::{Error, Rights};
use nsos_kcore::cspace::{CSpace, Cap};

const TRACES: &str = include_str!("data/cspace_model.trace");

fn num(s: &str) -> u32 {
    s.parse().unwrap_or_else(|_| panic!("número inválido: {s}"))
}

fn handles(s: &str) -> Vec<CapHandle> {
    if s == "-" {
        return Vec::new();
    }
    s.split(',').map(|h| CapHandle(num(h))).collect()
}

fn cap_of(s: &str) -> Cap<u32> {
    let (o, r) = s.split_once(':').unwrap();
    Cap {
        object: num(o),
        rights: Rights(num(r)),
    }
}

fn caps(s: &str) -> Vec<Cap<u32>> {
    if s == "-" {
        return Vec::new();
    }
    s.split(',').map(cap_of).collect()
}

fn show_cap(c: &Cap<u32>) -> String {
    format!("{}:{}", c.object, c.rights.bits())
}

fn show_caps(cs: &[Cap<u32>]) -> String {
    if cs.is_empty() {
        "-".into()
    } else {
        cs.iter().map(show_cap).collect::<Vec<_>>().join(",")
    }
}

fn show_handles(hs: &[CapHandle]) -> String {
    if hs.is_empty() {
        "-".into()
    } else {
        hs.iter()
            .map(|h| h.0.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn show<T>(r: Result<T, Error>, f: impl FnOnce(T) -> String) -> String {
    match r {
        Ok(v) => format!("ok {}", f(v)),
        Err(e) => format!("err {e:?}"),
    }
}

/// Ejecuta una operación y devuelve el resultado en el formato de la traza.
fn run(cs: &mut CSpace<u32>, op: &str, args: &[&str]) -> String {
    match (op, args) {
        ("insert", [o, r]) => show(
            cs.insert(Cap {
                object: num(o),
                rights: Rights(num(r)),
            }),
            |h| h.0.to_string(),
        ),
        ("get", [h]) => show(cs.get(CapHandle(num(h))).cloned(), |c| show_cap(&c)),
        ("getw", [h, need]) => show(
            cs.get_with(CapHandle(num(h)), Rights(num(need))).cloned(),
            |c| show_cap(&c),
        ),
        ("remove", [h]) => show(cs.remove(CapHandle(num(h))), |c| show_cap(&c)),
        ("dup", [h, mask]) => show(cs.dup(CapHandle(num(h)), Rights(num(mask))), |h| {
            h.0.to_string()
        }),
        ("removeall", [need, hs]) => show(cs.remove_all(&handles(hs), Rights(num(need))), |v| {
            show_caps(&v)
        }),
        ("insertall", [cs_]) => show(cs.insert_all(caps(cs_)).map_err(|(e, _)| e), |v| {
            show_handles(&v)
        }),
        ("clear", []) => format!("ok {}", show_caps(&cs.clear())),
        ("cycle", [n, o, r]) => {
            let mut last = 0;
            for _ in 0..num(n) {
                match cs.insert(Cap {
                    object: num(o),
                    rights: Rights(num(r)),
                }) {
                    Ok(h) => {
                        cs.remove(h).unwrap();
                        last = h.0;
                    }
                    Err(e) => return format!("err {e:?}"),
                }
            }
            format!("ok {last}")
        }
        _ => panic!("operación desconocida: {op} {args:?}"),
    }
}

#[test]
fn cspace_matches_lean_model() {
    let mut cs: Option<CSpace<u32>> = None;
    let mut ops = 0;
    for (n, line) in TRACES.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(limit) = line.strip_prefix("T ") {
            cs = Some(CSpace::new(num(limit)));
            continue;
        }
        let (call, expected) = line.split_once(" => ").expect("línea sin =>");
        let mut words = call.split(' ');
        let op = words.next().unwrap();
        let args: Vec<&str> = words.collect();
        let cs = cs.as_mut().expect("operación antes de T");
        let got = format!("{} len={}", run(cs, op, &args), cs.len());
        assert_eq!(
            got,
            expected,
            "línea {}: `{call}` no coincide con el modelo",
            n + 1
        );
        ops += 1;
    }
    assert!(
        ops > 1000,
        "la traza es demasiado corta ({ops} operaciones)"
    );
}
