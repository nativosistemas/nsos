# Verificación formal en Lean 4

Modelo del sistema de capacidades y del IPC de nsos, con demostraciones de
sus propiedades de seguridad. No verifica el código Rust directamente: lo
conecta con el modelo mediante **tests derivados del modelo** (ver abajo).

## Qué hay

| Archivo | Contenido |
|---|---|
| `Nsos/Rights.lean` | Derechos y atenuación (`nsos_abi::cap::Rights`). |
| `Nsos/Handle.lean` | Handles `[gen:12 \| slot:20]` y errores del ABI. |
| `Nsos/CSpace.lean` | Modelo ejecutable de `nsos_kcore::cspace::CSpace`, paso a paso. |
| `Nsos/System.lean` | Cells, canales y syscalls `cap_dup`, `cap_drop`, `channel_create`, `channel_send`, `channel_recv`, con cierre de extremos en cascada. |
| `Nsos/Security.lean` | Teoremas de seguridad. |
| `Nsos/Examples.lean` | Escenarios evaluados al compilar (ciclos de canales, handles viejos). |
| `TraceGen.lean` | Genera `crates/kcore/tests/data/cspace_model.trace`. |

## Teoremas demostrados

Todos sin `sorry` ni axiomas extra.

* `Rights.attenuate_subset`: derivar una capacidad nunca agrega derechos.
  `Rights.attenuate_needs_duplicate`: sin `DUPLICATE` no se deriva nada.
* `CSpace.get_after_remove`: un handle quitado deja de valer de inmediato.
* `CSpace.removeAll_rights`: `remove_all` solo entrega capacidades que estaban
  en la CSpace y tenían el derecho pedido.
* `Sys.step_other_cells`: una syscall de una cell no toca la CSpace de otra.
* `Sys.step_no_amplification` y `Sys.run_no_amplification`: en cualquier
  ejecución, toda capacidad que existe al final (en una CSpace o en una cola)
  viene de una del estado inicial con derechos iguales o mayores, o apunta a un
  objeto creado durante la ejecución.
* `Sys.send_requires_grant`: un `send` exitoso mueve capacidades del emisor que
  tenían `GRANT`, y viajan con los mismos derechos.
* `Sys.no_forging`: una cell que nunca hace `recv` termina solo con capacidades
  a objetos que ya tenía (con derechos iguales o menores) o a objetos nuevos.
  Nadie puede fabricar una capacidad ni recibirla sin un `recv` propio.
* `CSpace.stale_handle_never_revives`: después de quitar un handle, ninguna
  secuencia de operaciones lo vuelve a hacer válido.
* `Sys.step_no_leak`: después de cada syscall, todo extremo que ya nadie puede
  alcanzar está cerrado y con la cola vacía.

## Tests derivados del modelo

`lake exe tracegen` ejecuta el modelo de la CSpace con unas 4800 operaciones
pseudoaleatorias (deterministas) y anota el resultado esperado de cada una.
`crates/kcore/tests/model_traces.rs` repite esas operaciones sobre la `CSpace`
real y exige resultados idénticos: handles, errores, objetos, derechos y
cantidad de capacidades vivas. La CI regenera la traza y falla si el archivo
del repo no coincide con lo que produce el modelo.

El IPC del kernel (`kernel/src/cell.rs`) todavía no tiene este puente porque
depende del scheduler. Para tenerlo hay que mover la lógica de colas de canal
a `kcore`.

## Límites que encontró el modelo (corregidos)

1. **Ciclos de referencias.** Un extremo que viaja solo en su propia cola, o
   un ciclo de extremos en colas ajenas, quedaba vivo para siempre con el
   conteo de referencias. Ahora `Sys.gc` cierra todo lo inalcanzable
   (`Sys.step_no_leak`) y el kernel lo implementa con `ipc_gc`
   (`crates/kcore/src/ipc_gc.rs` + `kernel/src/cell.rs`). Ver
   `Examples.selfLoop` y `Examples.twoCycle`.
2. **La generación daba la vuelta.** Con 12 bits, tras 4096 reusos un handle
   viejo volvía a valer. Ahora el slot se retira
   (`CSpace.stale_handle_never_revives`) y la última traza comprueba que Rust
   hace lo mismo. Ver `Examples.noWrap`.

## Uso

Requiere [elan](https://github.com/leanprover/elan); la versión de Lean está
fijada en `lean-toolchain`.

```sh
make verify                     # desde la raíz: demostraciones + trazas
# o, desde verification/lean:
lake build                      # compila y verifica todas las demostraciones
lake exe tracegen ../../crates/kcore/tests/data/cspace_model.trace
```
