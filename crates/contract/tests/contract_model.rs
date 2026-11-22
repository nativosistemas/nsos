//! Tests derivados del modelo formal en Lean (`verification/lean/Nsos/Contract.lean`).
//!
//! `lake exe tracegen` evalúa `Contract.check` del modelo sobre cientos de
//! contratos (los contraejemplos del modelo y otros al azar) y exporta las
//! tablas de los contratos de `nsos_contract::protocols`. Aquí se exige:
//! * que la `const fn` [`check`] que corre la macro al compilar dé el mismo
//!   veredicto y la misma cota en todos: los teoremas del modelo
//!   (`Contract.contract_safe`) valen para todo contrato que acepte;
//! * que las tablas que genera `contract!` sean las del modelo, que ya
//!   demostró `echo_check` y `note_check`.

use nsos_contract::protocols::{
    counter, echo, handover, note, restore, store_obj, store_root, store_share,
};
use nsos_contract::Msg;
use nsos_contract::{check, Dir, Transition};

const TRACE: &str = include_str!("data/contract_model.trace");

fn parse_table(words: &[&str]) -> Vec<Transition> {
    words
        .iter()
        .map(|w| {
            let f: Vec<&str> = w.split(',').collect();
            assert_eq!(f.len(), 4, "transición mal escrita: {w}");
            Transition {
                src: f[0].parse().unwrap(),
                dir: match f[1] {
                    "ab" => Dir::ClientToServer,
                    "ba" => Dir::ServerToClient,
                    d => panic!("dirección inválida: {d}"),
                },
                label: f[2].parse().unwrap(),
                dst: f[3].parse().unwrap(),
            }
        })
        .collect()
}

#[test]
fn check_agrees_with_model() {
    let mut n = 0;
    let (mut ok, mut rejected) = (0, 0);
    for line in TRACE.lines().filter(|l| l.starts_with("C ")) {
        let (lhs, want) = line.split_once(" => ").unwrap();
        let words: Vec<&str> = lhs.split_whitespace().collect();
        let table = parse_table(&words[2..]);
        let got = match check(&table) {
            Ok(k) => {
                ok += 1;
                format!("some {k}")
            }
            Err(_) => {
                rejected += 1;
                "none".to_string()
            }
        };
        assert_eq!(got, want, "línea: {line}");
        n += 1;
    }
    assert!(n >= 400, "la traza tiene {n} contratos");
    assert!(
        ok >= 50 && rejected >= 50,
        "{ok} aceptados, {rejected} rechazados"
    );
}

fn model_table(name: &str) -> (u16, Vec<Transition>) {
    let line = TRACE
        .lines()
        .find(|l| l.starts_with(&format!("P {name} ")))
        .unwrap_or_else(|| panic!("la traza no tiene el contrato {name}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    (words[2].parse().unwrap(), parse_table(&words[3..]))
}

fn sorted(mut t: Vec<Transition>) -> Vec<(u16, bool, u64, u16)> {
    let mut v: Vec<_> = t
        .drain(..)
        .map(|t| (t.src, t.dir == Dir::ClientToServer, t.label, t.dst))
        .collect();
    v.sort();
    v
}

#[test]
fn protocol_tables_are_the_model_ones() {
    for (name, start, table) in [
        ("echo", echo::START, echo::TRANSITIONS),
        ("note", note::START, note::TRANSITIONS),
        ("counter", counter::START, counter::TRANSITIONS),
        ("handover", handover::START, handover::TRANSITIONS),
        ("restore", restore::START, restore::TRANSITIONS),
        ("store_root", store_root::START, store_root::TRANSITIONS),
        ("store_obj", store_obj::START, store_obj::TRANSITIONS),
        ("store_share", store_share::START, store_share::TRANSITIONS),
    ] {
        let (mstart, mtable) = model_table(name);
        assert_eq!(start, mstart, "{name}: estado inicial");
        assert_eq!(sorted(table.to_vec()), sorted(mtable), "{name}: tabla");
    }
    assert_eq!(echo::BOUND, 1);
    assert_eq!(note::BOUND, 1);
    assert_eq!(counter::BOUND, 1);
    assert_eq!(handover::BOUND, 2);
    assert_eq!(restore::BOUND, 1);
    assert_eq!(store_root::BOUND, 1);
    assert_eq!(store_obj::BOUND, 1);
}

/// Los contratos del almacén usan las etiquetas de `nsos_store::proto`, que
/// es lo que habla el resto del sistema.
#[test]
fn store_labels_are_the_protocol_ones() {
    use nsos_store::proto as p;
    assert_eq!(<store_root::Open as Msg>::LABEL, p::OPEN);
    assert_eq!(<store_root::Opened as Msg>::LABEL, p::OPENED);
    assert_eq!(<store_root::Fail as Msg>::LABEL, p::ERR);
    assert_eq!(<store_obj::Stat as Msg>::LABEL, p::STAT);
    assert_eq!(<store_obj::Info as Msg>::LABEL, p::INFO);
    assert_eq!(<store_obj::Read as Msg>::LABEL, p::READ);
    assert_eq!(<store_obj::Data as Msg>::LABEL, p::DATA);
    assert_eq!(<store_obj::Write as Msg>::LABEL, p::WRITE);
    assert_eq!(<store_obj::Written as Msg>::LABEL, p::WRITTEN);
    assert_eq!(<store_obj::DupRo as Msg>::LABEL, p::DUP_RO);
    assert_eq!(<store_obj::Opened as Msg>::LABEL, p::OPENED);
    assert_eq!(<store_obj::Fail as Msg>::LABEL, p::ERR);
    assert_eq!(<store_obj::FailMem as Msg>::LABEL, p::ERR);
}
