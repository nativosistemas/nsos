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
    for node in nodes {
        if node.candidate {
            for &j in &node.contents {
                if j < n {
                    internal[j] += 1;
                }
            }
        }
    }
    // Vivos de entrada: los que alguien tiene, y los que aparecen en alguna
    // cola que no es de un candidato.
    // Pila de trabajo de tamaño fijo: cada nodo entra a lo sumo una vez (al
    // marcarlo vivo), así que nunca se pasa de `n`.
    let mut alive = vec![false; n];
    let mut work = vec![0usize; n];
    let mut top = 0;
    for (i, node) in nodes.iter().enumerate() {
        let a = !node.candidate || node.in_flight > internal[i];
        alive[i] = a;
        if a && node.candidate {
            work[top] = i;
            top += 1;
        }
    }
    // Lo que viaja en la cola de un vivo está vivo.
    while top > 0 {
        top -= 1;
        let i = work[top];
        for &j in &nodes[i].contents {
            if j < n && !alive[j] {
                alive[j] = true;
                work[top] = j;
                top += 1;
            }
        }
    }
    for a in alive.iter_mut() {
        *a = !*a;
    }
    alive
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

/// Pruebas de Kani: grafos cualesquiera (índices fuera de rango incluidos).
/// Sin panics ni overflows, y la respuesta es coherente.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// `n` nodos con hasta 2 apariciones en cola cada uno. Un índice `n` está
    /// fuera de rango (el kernel nunca lo genera, pero no debe romper nada).
    fn any_graph(n: usize) -> Vec<Node> {
        let mut nodes = Vec::with_capacity(n);
        for _ in 0..n {
            let in_flight: u8 = kani::any();
            kani::assume(in_flight <= 4);
            let mut contents = Vec::with_capacity(2);
            for _ in 0..2 {
                let j: u8 = kani::any();
                if (j as usize) <= n && kani::any() {
                    contents.push(j as usize);
                }
            }
            nodes.push(Node {
                candidate: kani::any(),
                in_flight: in_flight as usize,
                contents,
            });
        }
        nodes
    }

    /// Con 3 nodos (ciclos de hasta 3, colgados de otros): sin panics.
    #[kani::proof]
    #[kani::unwind(8)]
    #[kani::solver(cadical)]
    fn garbage_no_panic_3() {
        let nodes = any_graph(3);
        assert_eq!(garbage(&nodes).len(), 3);
    }

    /// Con 2 nodos (auto-ciclos, ciclos de 2, referencias externas): la
    /// respuesta cumple la especificación.
    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(cadical)]
    fn garbage_spec_2() {
        let n = 2;
        let nodes = any_graph(n);
        // Como en el kernel: `in_flight` cuenta todas las apariciones, las de
        // colas de nodos del grafo y quizás otras (colas de extremos que no
        // viajan).
        for i in 0..n {
            let mut seen = 0;
            for k in 0..n {
                for &j in &nodes[k].contents {
                    if j == i {
                        seen += 1;
                    }
                }
            }
            kani::assume(nodes[i].in_flight >= seen);
        }
        let g = garbage(&nodes);
        assert_eq!(g.len(), n);
        for i in 0..n {
            let node = &nodes[i];
            // Lo que alguien tiene (no candidato) nunca es basura.
            if !node.candidate {
                assert!(!g[i]);
            }
            // Lo que viaja en la cola de un extremo vivo está vivo.
            if !g[i] {
                for &j in &node.contents {
                    if j < n {
                        assert!(!g[j]);
                    }
                }
            }
            // Si es basura, todas sus apariciones están en colas de basura.
            if g[i] {
                let mut from_garbage = 0;
                for k in 0..n {
                    if g[k] {
                        for &j in &nodes[k].contents {
                            if j == i {
                                from_garbage += 1;
                            }
                        }
                    }
                }
                assert!(node.in_flight <= from_garbage);
            }
        }
    }
}
