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
| `walk_any_tables` | `crates/kcore/src/paging.rs` | Con tablas de páginas de contenido cualquiera (incluso corruptas), `map`, `unmap` y `translate` no entran en pánico ni desbordan, y nunca usan un índice fuera de la tabla. |
| `map_then_translate` | `crates/kcore/src/paging.rs` | Mapear cualquier página de la mitad baja: después se traduce a esa física con exactamente los permisos pedidos (USER, WRITABLE, NX), y no se escribe nada en la mitad alta del PML4 ni en las tablas del kernel. |
| `unmap_only_that_page` | `crates/kcore/src/paging.rs` | Con dos páginas mapeadas, `unmap` de cualquier dirección quita solo la página pedida. |
| `parse_data_any`, `parse_hello_any` | `crates/link/src/lib.rs` | Con cualquier trama de hasta 40 bytes, el parser del enlace no entra en pánico; si acepta, la cabecera, los datos y la etiqueta cubren la trama exactamente (sin bytes de más ni de menos). |
| `unmap_target_exact` | `crates/kcore/src/paging.rs` | `mem_unmap` elige un mapeo solo si coincide exactamente en dirección y tamaño (alineado y no vacío), el primero que coincide; si ninguno coincide, no elige nada. |
| `map_request_never_exec` | `crates/kcore/src/paging.rs` | Con cualquier valor de 64 bits, `mem_map` solo acepta `READ` o `READ \| WRITE`: nunca `EXEC` ni bits ajenos, y pide a la capacidad exactamente esos derechos (cells selladas; una mutación que acepta `EXEC` la rompe). |
| `user_range_stays_low` | `crates/kcore/src/paging.rs` | Un rango que acepta `user_range_ok` (lo que exige `mem_map`) no desborda y queda entero en la mitad baja. |
| `codec_roundtrip` | `crates/kcore/src/paging_arm64.rs` | Toda entrada neutra presente se codifica como descriptor ARM64 y se decodifica igual; una no presente queda inválida. |
| `hw_agrees_with_view` | `crates/kcore/src/paging_arm64.rs` | Con tablas cualesquiera escritas por el codec (una entrada arbitraria por nivel), un modelo de la MMU de ARM64 escrito desde el manual y la vista del kernel (`translate_with_flags`) coinciden: misma física, y EL0 lee, escribe y ejecuta exactamente cuando la vista lo permite. EL1 nunca ejecuta una página de cell (PXN, como SMEP), no escribe lo que la vista marca de solo lectura y con PAN no toca páginas de cells. |
| `map_then_hw` | `crates/kcore/src/paging_arm64.rs` | De punta a punta: mapear una página de la mitad baja con `paging::map` sobre el codec deja en la MMU exactamente la física y los permisos pedidos. |
| `token_any` | `crates/kcore/src/fdt.rs` | Leer un token del device tree en cualquier posición de bloques de contenido cualquiera: sin pánicos, y el próximo token empieza al menos 4 bytes más adelante (todo recorrido termina). |
| `new_any` | `crates/kcore/src/fdt.rs` | Abrir cualquier blob: sin pánicos; si se acepta, los bloques de estructura y cadenas caen dentro. |
| `reg_entry_any` | `crates/kcore/src/fdt.rs` | Leer cualquier entrada de un `reg` cualquiera con cualquier cantidad de celdas: sin pánicos ni desbordes. |
| `translate_any` | `crates/kcore/src/fdt.rs` | Traducir una región por un `ranges` cualquiera con cualquier cantidad de celdas: sin pánicos ni desbordes. |
| `read_cells_any` | `crates/kcore/src/fdt.rs` | Leer celdas de cualquier buffer: sin pánicos; solo acepta 0 a 2 celdas que estén enteras. |
| `mmio_never_touches_ram` | `crates/kcore/src/platform.rs` | Si `mmio_allowed` acepta una región de dispositivo, ningún byte de las páginas que mapea el kernel cae en la RAM (dos regiones cualesquiera), y esas páginas cubren el dispositivo. |
| `io_ranges_disjoint` | `crates/kcore/src/platform.rs` | Un paso del asignador de E/S desde cualquier estado alcanzable y con cualquier tamaño: el rango empieza en o después del primer puerto libre, el nuevo primer libre es su fin, está alineado y cabe en la ventana y en 16 bits. Por inducción, todos los rangos que entrega son disjuntos. |
| `io_two_ranges_disjoint` | `crates/kcore/src/platform.rs` | Dos rangos seguidos con tamaños cualesquiera no se solapan (el caso concreto de la inducción). |
| `pio_access_in_range` | `crates/kcore/src/platform.rs` | Un acceso de una cell a sus puertos que acepta `pio_offset` cae entero en su rango y en la ventana de E/S. |
| `alloc_any` | `crates/kcore/src/frame_bitmap.rs` | Desde cualquier bitmap de hasta 128 marcos que cumpla el invariante (`free` cuenta los marcos libres; bits y `hint` cualesquiera, también los que sobran al final de la última palabra): `alloc` entrega un marco que estaba libre y dentro del rango, lo marca ocupado y no toca ningún otro; si no entrega nada, no había libres. Dos cells nunca reciben el mismo marco. |
| `free_any` | `crates/kcore/src/frame_bitmap.rs` | `free_frame` con cualquier número de marco: solo libera uno ocupado y dentro del rango; uno libre da `DoubleFree` y uno fuera de rango `OutOfRange`, sin cambiar nada. |
| `alloc_contiguous_0`, `alloc_contiguous_2`, `alloc_contiguous_3` | `crates/kcore/src/frame_bitmap.rs` | Con hasta 72 marcos, `n = 0` o una alineación inválida no entregan nada; con `(n, align)` = (2, 1) y (3, 4) (concretos: simbólicos, CBMC se queda sin memoria) los marcos que entrega estaban todos libres, están alineados y dentro del rango, y no toca ningún otro. |
| `regions_any` | `crates/kcore/src/frame_bitmap.rs` | Con hasta 72 marcos y cualquier dirección y largo de 64 bits: sin pánicos ni desbordes; `reserve_region` ocupa todo marco que toca la región y `release_region` libera solo los marcos enteros dentro de ella. |
| `new_any` (bitmap) | `crates/kcore/src/frame_bitmap.rs` | `FrameBitmap::new` con cualquier tamaño: todo ocupado, sin libres y con el invariante. |
| `check_item_any` | `crates/kcore/src/wait.rs` | Un ítem de `wait` aceptado pide al menos una señal, todas emitibles por el tipo de su objeto, con `reserved` en 0; memoria, puertos e imágenes nunca se aceptan. |
| `channel_signals_any` | `crates/kcore/src/wait.rs` | Con cualquier estado de canal, las señales son las del tipo y dicen exactamente lo que harían `recv` y `send`: `READABLE` sii hay mensaje, `PEER_CLOSED` sii el peer cerró, `WRITABLE` sii está abierto y su cola tiene lugar. |
| `deadline_any` | `crates/kcore/src/wait.rs` | El plazo de `wait` nunca desborda: con `At(d)`, `d = now + timeout` exacto y no vence antes; si no entra en 64 bits es "sin plazo". |
| `claims_exclusive` | `crates/kcore/src/irq.rs` | Una línea de interrupción reclamada no se vuelve a entregar hasta soltarla, reclamar o soltar otra no la cambia, y las líneas fuera de rango nunca se entregan. |
| `intx_any` | `crates/kcore/src/irq.rs` | Con cualquier `interrupt-map` de hasta dos entradas y cualquier máscara: sin pánicos, y todo INTID que devuelve la búsqueda es una SPI válida (32 a 1019). |
| `ecam_addr_in_region` | `crates/kcore/src/platform.rs` | La página de configuración de cualquier función PCI cae entera dentro de la región ECAM. |
| `decode_entry_any` | `crates/store/src/layout.rs` | Con cualquier entrada de 128 bytes leída del disco y cualquier tamaño de disco: sin panics ni overflows; si se acepta, el nombre es válido, los datos caen dentro del disco y fuera de las ranuras de superbloque, y codificarla da los mismos bytes. |
| `find_run_is_free` | `crates/store/src/alloc.rs` | Con cualquier mapa de bloques: el rango que devuelve está entero dentro del límite y libre, y después de marcarlo queda usado. Es la condición `Fresh` del modelo de Lean (`Store.crash_safe`). |
| `decode_any` | `crates/contract/src/proofs.rs` | Con cualquier mensaje que deje pasar el kernel (`validate`) y cualquier respuesta de `cap_info`, el servidor de `echo` en `Ready`: sin pánicos; si acepta, la etiqueta es de un mensaje que el cliente puede mandar ahí y se usaron exactamente sus datos y capacidades; si no, el canal vuelve en el mismo estado y se soltaron **todas** las capacidades recibidas. Un `NewChannel` aceptado trae un extremo creado para `note` (`channel_contract`). Quitar el `discard` de la rama de violación la hace fallar; que `is_channel` no compare el contrato, también. |
| `restore_decode_any` | `crates/contract/src/proofs.rs` | Con cualquier mensaje y cualquier respuesta de `cap_info`, la versión nueva en `Start`: sin pánicos; si acepta `Restore`, el estado es un objeto `Memory` con `READ` y **sin** `WRITE`, el extremo es un canal y se usaron exactamente las dos capacidades; si no, se soltaron todas. Ninguna versión nueva tipada recibe el estado escribible. Que `Mem<RO>` no prohíba `WRITE` la hace fallar. |
| `store_reading_decode_any` | `crates/contract/src/proofs.rs` | El cliente del almacén esperando la respuesta a `Read`, con cualquier mensaje y cualquier respuesta de `cap_info`: sin pánicos; si acepta, es `Data` o `FailMem` con exactamente una capacidad, un objeto `Memory` con `READ` y `WRITE` (el búfer de vuelta); si no, se soltaron todas y sigue esperando. Que el decodificador de `Mem` no compruebe tipo y derechos la hace fallar. |
| `try_send_any` | `crates/contract/src/proofs.rs` | `try_send` con cualquier resultado de la syscall: si se mandó, no se soltó nada; con la cola llena, el canal vuelve en el mismo estado y se soltó la capacidad del mensaje; si falló, se soltó también y el canal se cerró. No soltar la capacidad con la cola llena la hace fallar. |
| `roundtrip_echo`, `roundtrip_endpoint` | `crates/contract/src/proofs.rs` | Un `Echo` con cualquier texto y un `NewChannel` con cualquier handle válido se codifican en un mensaje que el kernel acepta y se decodifican igual (nada se trunca). |
| `check_any` | `crates/contract/src/proofs.rs` | Con cualquier tabla de hasta tres transiciones, el control de contratos no entra en pánico, y si acepta, ningún estado mezcla direcciones, cada etiqueta lleva a un solo estado y la cota no supera la cantidad de transiciones. |

`check_invariants` (en `cspace.rs`) también corre en los tests unitarios.

### Mutaciones de ARM64

Para comprobar que las pruebas de `paging_arm64` no pasan por casualidad,
cada una de estas mutaciones del codec las hace fallar: quitar PXN de las
páginas de cells (`hw_agrees_with_view` y `map_then_hw`), no poner
`APTable[0]` en las tablas del kernel, no poner `APTable[1]` en las de
solo lectura, y decodificar NX siempre desde PXN (`hw_agrees_with_view`).

### Mutaciones del allocador de marcos

Cada una hace fallar la prueba indicada: no saltear en `alloc` los bits que
sobran después del último marco (`alloc_any`), quitar el control de
`DoubleFree` (`free_any`), saltar un hueco sin redondear a la alineación
(`alloc_contiguous_2`) y redondear `release_region` hacia afuera
(`regions_any`).

Los invariantes se cuentan por palabra (`count_ones` sobre máscaras), no
marco a marco: con un recuento marco a marco CBMC pasó los 6 GB en
`free_any`.

### Mutación de `wait`

Activar `WRITABLE` aunque el peer esté cerrado (cambiar el `else if` de
`channel_signals` por un `if`) hace fallar `channel_signals_any`.

### Mutación de `mem_unmap`

Aceptar un mapeo más grande que el rango pedido (`l >= len` en vez de
`l == len`, o sea desmapear un objeto entero cuando se pidió una parte) hace
fallar `unmap_target_exact`.

### Mutaciones de las interrupciones

Reclamar una línea sin mirar si ya tiene dueño hace fallar
`claims_exclusive`; aceptar del `interrupt-map` una SPI mayor que 987 (un
INTID fuera de rango) hace fallar `intx_any`.

### Mutaciones del device tree y de la plataforma

Cada una hace fallar la prueba indicada: comparar con la RAM la región sin
redondear a páginas (`mmio_never_touches_ram`), no avanzar el primer
puerto libre (`io_ranges_disjoint`, `io_two_ranges_disjoint`), no alinear
el rango (`io_ranges_disjoint`), validar solo el inicio del acceso y no su
fin (`pio_access_in_range`), no exigir que entre la página entera de
configuración (`ecam_addr_in_region`), que `END_NODE` no avance
(`token_any`) y sumar las celdas en 32 bits (`reg_entry_any`).

El iterador de nodos (`Fdt::nodes`) es demasiado grande para Kani (con 12
bytes de bloque CBMC no termina en 10 minutos). Lo que lee pasa entero por
`token`, `reg_entry` y `translate_ranges`, que sí están probados, y los
tests en el host lo recorren con cada byte de un árbol alterado, cada
truncamiento y miles de árboles con bytes al azar (también el DTB real de
QEMU).

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
cd crates/kcore && cargo kani --harness hw_agrees_with_view   # ARM64, segundos
cd crates/kcore && cargo kani --harness remove_any   # una sola
```

No correr pruebas en paralelo (`-j`): algunas usan varios GB de memoria.
