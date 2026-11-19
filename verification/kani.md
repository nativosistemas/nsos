# Pruebas de Kani sobre el código Rust

[Kani](https://github.com/model-checking/kani) verifica el código Rust real
(no un modelo) con model checking acotado: explora **todas** las entradas
posibles dentro de unos límites y demuestra que no hay panics, overflows,
índices fuera de rango ni fallas de las aserciones.

Complementa a Lean (`verification/lean`): Lean demuestra propiedades del
diseño sobre un modelo; Kani demuestra que la implementación no rompe en
casos borde.

## Qué se verifica

| Prueba | Archivo | Qué demuestra |
|---|---|---|
| `handle_roundtrip_any` | `crates/abi/src/cap.rs` | Cualquier `(slot, generación)` válido se codifica y decodifica igual. |
| `attenuate_any` | `crates/abi/src/cap.rs` | Atenuar nunca agrega derechos; sin `DUPLICATE` falla. |
| `from_bytes_any_input` | `crates/abi/src/msg.rs` | Cualquier buffer de 256 bytes: sin panics; si es válido, como mucho `MAX_CAPS` handles no nulos y `to_bytes` lo reproduce. |
| `insert_any`, `get_with_any`, `remove_any`, `dup_any`, `remove_all_any`, `insert_all_any`, `clear_any` | `crates/kcore/src/cspace.rs` | Desde **cualquier** CSpace que cumpla los invariantes (generaciones arbitrarias, slots retirados, lista libre en cualquier orden, límites arbitrarios), cada operación con argumentos cualesquiera, incluidos handles fabricados: sin panics ni overflows, mantiene los invariantes (`check_invariants`), y cumple su contrato (un handle quitado deja de valer, `dup` no agrega derechos, `remove_all`/`insert_all` son atómicos, etc.). |
| `cspace_get_any_handle` | `crates/kcore/src/cspace.rs` | `get` con cualquier valor de 32 bits nunca entra en pánico. |
| `cspace_generation_never_wraps` | `crates/kcore/src/cspace.rs` | Un slot en su última generación se retira; el handle viejo no revive. |
| `cspace_new_any_limit` | `crates/kcore/src/cspace.rs` | `CSpace::new` con cualquier límite. |
| `garbage_no_panic_3` | `crates/kcore/src/ipc_gc.rs` | Recolector con cualquier grafo de 3 nodos (ciclos, índices fuera de rango): sin panics. |
| `garbage_spec_2` | `crates/kcore/src/ipc_gc.rs` | Con cualquier grafo de 2 nodos, la respuesta cumple la especificación de `Sys.gc`: lo que alguien tiene no es basura, lo que viaja en la cola de un vivo está vivo, y la basura solo aparece en colas de basura. |

`check_invariants` (en `cspace.rs`) también corre en los tests unitarios.

## Límites

Kani explora estados chicos (CSpace de 3 slots, grafos de 2 o 3 nodos). Los
errores de este tipo de código suelen aparecer ya con tamaños chicos, pero no
es una demostración para cualquier tamaño: eso lo dan las demostraciones de
Lean sobre el modelo, y los tests derivados del modelo conectan ambos.

Para que el verificador termine, el código evita realocaciones de `Vec` en
los caminos verificados (capacidad reservada de antemano) y usa bucles
simples. Si una prueba se queda sin memoria, ese es el primer lugar a mirar.

## Uso

```sh
cargo install --locked kani-verifier --version 0.68.0
cargo kani setup
make kani                       # todas (unos 20 minutos)
cd crates/kcore && cargo kani --harness remove_any   # una sola
```

No correr pruebas en paralelo (`-j`): algunas usan varios GB de memoria.
