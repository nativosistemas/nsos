//! Recolector de extremos de canal en vuelo.
//!
//! Un extremo puede viajar dentro de un mensaje, en la cola de otro extremo.
//! Con conteo de referencias puro, un ciclo (un extremo en su propia cola, o
//! A en la cola de B y B en la de A) nunca se libera aunque ninguna cell
//! pueda volver a alcanzarlo. Es el mismo problema que tiene Linux con los
//! descriptores pasados por sockets Unix, y la solución es la misma: un
//! recolector que mira solo los extremos en vuelo.
//!
//! Este módulo tiene el algoritmo, sin `unsafe` y probado en el host. El
//! kernel arma el grafo y libera las colas de lo que este módulo marca como
//! basura. La especificación es `Sys.gc` en `verification/lean`: basura es
//! todo lo que no se alcanza desde una CSpace siguiendo colas.

use alloc::vec;
use alloc::vec::Vec;

/// Un extremo que viaja en al menos un mensaje.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// `true` si *todas* sus referencias están en colas (ninguna CSpace ni
    /// hilo lo tiene). Solo estos pueden ser basura.
    pub candidate: bool,
    /// Cuántas veces aparece en mensajes encolados.
    pub in_flight: usize,
    /// Índices de los nodos que viajan en *su* cola (con repeticiones). Quien
    /// tenga este extremo los puede recibir.
    pub contents: Vec<usize>,
}

/// Devuelve, para cada nodo, si es basura (inalcanzable).
///
/// Un candidato está vivo si alguna de sus apariciones está en la cola de un
/// extremo que no es candidato (alguien lo tiene, y puede recibirlo), o en la
/// cola de un candidato vivo.
pub fn garbage(nodes: &[Node]) -> Vec<bool> {
    let n = nodes.len();
    // Apariciones de cada nodo dentro de colas de candidatos.
    let mut internal = vec![0usize; n];
    for node in nodes.iter().filter(|x| x.candidate) {
        for &j in &node.contents {
            if j < n {
                internal[j] += 1;
            }
        }
    }
    let mut alive: Vec<bool> = nodes
        .iter()
        .zip(&internal)
        .map(|(x, &int)| !x.candidate || x.in_flight > int)
        .collect();
    let mut work: Vec<usize> = (0..n).filter(|&i| alive[i] && nodes[i].candidate).collect();
    while let Some(i) = work.pop() {
        for &j in &nodes[i].contents {
            if j < n && !alive[j] {
                alive[j] = true;
                work.push(j);
            }
        }
    }
    alive.into_iter().map(|a| !a).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(candidate: bool, in_flight: usize, contents: &[usize]) -> Node {
        Node {
            candidate,
            in_flight,
            contents: contents.to_vec(),
        }
    }

    #[test]
    fn self_loop_is_garbage() {
        // El extremo 0 viaja solo en su propia cola.
        assert_eq!(garbage(&[node(true, 1, &[0])]), [true]);
    }

    #[test]
    fn two_cycle_is_garbage() {
        let g = garbage(&[node(true, 1, &[1]), node(true, 1, &[0])]);
        assert_eq!(g, [true, true]);
    }

    #[test]
    fn held_endpoint_keeps_its_queue_alive() {
        // 0 lo tiene una cell (no candidato) y en su cola viaja 1, que lleva a 2.
        let g = garbage(&[
            node(false, 1, &[1]),
            node(true, 1, &[2]),
            node(true, 1, &[]),
        ]);
        assert_eq!(g, [false, false, false]);
    }

    #[test]
    fn reference_from_outside_keeps_alive() {
        // 0 y 1 forman un ciclo, pero 0 también viaja en la cola de un extremo
        // que no está en vuelo (lo tiene una cell): in_flight 2 > internas 1.
        let g = garbage(&[node(true, 2, &[1]), node(true, 1, &[0])]);
        assert_eq!(g, [false, false]);
    }

    #[test]
    fn cycle_hanging_from_garbage_is_garbage() {
        // 0 se apunta a sí mismo y lleva al ciclo 1 <-> 2.
        let g = garbage(&[
            node(true, 1, &[0, 1]),
            node(true, 2, &[2]),
            node(true, 1, &[1]),
        ]);
        assert_eq!(g, [true, true, true]);
    }

    #[test]
    fn not_candidate_is_never_garbage() {
        assert_eq!(garbage(&[node(false, 1, &[0])]), [false]);
    }
}
