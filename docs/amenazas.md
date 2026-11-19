# Modelo de amenazas

Qué protege nsos, contra quién, con qué mecanismo y qué **no** protege. Es
un prototipo de investigación en QEMU: la lista de lo que falta es parte del
resultado, no una nota al pie.

## Activos y adversario

**Activos**: la integridad y confidencialidad de la memoria del kernel y de
cada cell, las capacidades de cada cell (quién puede hablar con quién y qué
puede hacer), y la disponibilidad del kernel.

**Adversario principal**: una cell arbitraria, con código nativo cualquiera
(no necesariamente escrito en Rust seguro: la imagen ELF puede venir de
cualquier compilador o estar hecha a mano). Puede hacer cualquier syscall con
cualquier argumento, en cualquier orden, desde cualquier estado.

**Fuera del modelo** (por ahora): el bootloader y las imágenes que carga
(se confía en ellos), el hardware y el hipervisor, y un atacante con acceso
físico.

## Qué se garantiza y cómo

| Propiedad | Mecanismo | Evidencia |
|---|---|---|
| Una cell no lee ni escribe memoria del kernel ni de otra cell | Tablas de páginas propias por cell; la mitad alta sin bit USER; W^X en kernel y cells | `intruso` (lectura del kernel → #PF), `fragil`; ktest de W^X; Lean: `Mem.step_iso`, `Mem.never_maps_foreign`; Kani: `map_then_translate`, `user_range_stays_low` |
| El kernel no puede ser engañado para tocar memoria ajena en nombre de una cell | `usercopy` traduce cada página con las tablas de la cell y exige USER (y WRITABLE); nunca desreferencia punteros de usuario | `intruso`: punteros al kernel y a código propio; `caos`: 50 000 syscalls al azar |
| …y eso lo hace cumplir la CPU | **SMAP**: el kernel no puede acceder a páginas de usuario; **SMEP**: no puede ejecutarlas. `AC` se limpia en cada entrada (SFMASK y `clac`) | `test-smap-fault`: una lectura directa del kernel a una página de usuario termina en #PF |
| Nadie fabrica ni amplía capacidades | Handles con generación; derechos que solo se atenúan; `GRANT` para moverlas | Lean: `no_forging`, `run_no_amplification`, `send_requires_grant`, `stale_handle_never_revives`; Kani sobre `kcore`; `intruso` |
| Una cell solo toca el hardware que se le dio | `IoPorts` con desplazamientos relativos al rango; sin acceso a puertos fuera de él | `intruso`: puertos sin capacidad, con objetos de otro tipo |
| Un fallo de una cell no afecta al kernel ni a las demás | Excepción en anillo 3 → se termina la cell, se cierran sus capacidades y se libera su memoria | `fragil`, `intruso`; ktest verifica que no se pierden marcos |
| Una cell colgada se puede terminar | `cell_kill` con `CONTROL`; la cell muere desde su hilo, nunca a mitad de una syscall | `dormilona` (bloqueada y girando) |
| Un servicio se actualiza sin que el cliente note saltos | Protocolo de supervisor con rollback | Lean: `Upgrade.client_sees_consecutive`; test en QEMU con rechazo y aceptación |
| Un servicio se muda a otra máquina sin que el cliente note saltos, y un rechazo remoto no daña el estado local | El enlace manda una *copia* del estado; la decisión vuelve por el cable; rollback local | Lean: `Migration.client_sees_consecutive`, `one_server`, `rollback_resumes`; `tools/migrate-test.sh` con dos QEMU |
| No se filtran extremos de canal en ciclos | Recolector `ipc_gc` | Lean: `step_no_leak`; ktest de ciclos |
| El `unsafe` del kernel está acotado y justificado | Lints `undocumented_unsafe_blocks`, `forbid(unsafe_code)` en `abi`/`kcore`, `deny` en cells, registro | `tools/audit-unsafe.sh` en la CI |

## Qué **no** se garantiza

Ordenado por gravedad para el objetivo del proyecto (aislamiento).

1. **Canales laterales de microarquitectura.** El kernel está mapeado (sin
   bit USER) en el espacio de cada cell y no hay KPTI: en una CPU vulnerable
   a Meltdown, una cell podría leer memoria del kernel por ejecución
   especulativa. Tampoco hay mitigaciones de Spectre (retpolines, IBRS,
   barreras tras validar índices de syscalls). `clock_monotonic` da
   resolución de nanosegundos, útil para medir tiempos. *Mitigación futura*:
   KPTI con PCID, `lfence` tras las comprobaciones de rango, reducir la
   resolución del reloj para cells sin un derecho específico.
2. **DMA sin IOMMU.** Un driver con capacidades `Memory` con `CONTROL`
   conoce direcciones físicas y el dispositivo puede escribir en cualquier
   lugar de la RAM. Un driver es, por lo tanto, de confianza respecto a la
   memoria. *Mitigación futura*: programar el IOMMU (VT-d) para que cada
   dispositivo solo vea los objetos `Memory` de su driver.
3. **Mapeos que sobreviven a mover una capacidad `Memory`.** *Resuelto*:
   un mapeo vale mientras la cell tenga una capacidad al objeto con los
   derechos del mapeo. Al enviarla, pasarla a una cell hija o soltarla, el
   kernel quita el mapeo (`Cell::revoke_mappings`). Demostrado en
   `Mem.step_iso` y `Mem.send_revokes` (verification/lean). Falta
   `mem_unmap` para soltar un mapeo sin soltar la capacidad.
4. **Agotamiento de recursos (DoS).** No hay cuotas: una cell puede llenar
   su CSpace (1024 entradas) de objetos `Memory` de hasta 1 MiB, o crear
   cells sin límite si tiene una imagen, y así dejar sin memoria al resto.
   El scheduler es round-robin sin prioridades: no hay garantías de tiempo
   real. *Mitigación futura*: cuotas de memoria y de cells por supervisor
   (las cuenta quien crea la cell).
5. **La consola es compartida y falsificable.** `debug_log` antepone el
   nombre de la cell solo al inicio de cada escritura; una cell puede
   escribir un salto de línea y luego texto que parezca venir de otra.
   *Mitigación futura*: prefijar cada línea o pasar la consola a una cell
   con canal propio.
6. **Caída de la versión nueva durante una actualización.** El rollback
   cubre el rechazo explícito; si la nueva falla con las capacidades, los
   clientes ven `PEER_CLOSED`.
7. **El enlace entre máquinas no está autenticado ni cifrado.** Quien
   controle el cable (o la otra máquina) lee el estado migrado y puede
   inventar respuestas para el cliente; la suma de control solo detecta
   errores, no ataques. La máquina de destino confía en el estado que
   recibe: lo valida el servicio (número mágico), no el sistema. Además el
   enlace de origen conserva un mapeo de solo lectura del estado tras un
   rechazo (punto 3). *Mitigación futura*: canal autenticado (claves por
   máquina, MAC por trama) y un derecho específico para migrar.
8. **Arranque sin verificación.** Limine carga el kernel y las cells sin
   comprobar firmas; quien pueda cambiar la imagen ISO controla el sistema.
9. **Un solo núcleo.** Varias invariantes (syscalls con interrupciones
   apagadas, la pila de kernel en una variable global) asumen un CPU. Con
   SMP hay que revisarlas antes de prender el segundo núcleo.

## Supuestos de confianza

* El kernel (con su `unsafe` registrado) es correcto en lo que no está
  demostrado. Lo demostrado en Lean es el *modelo*; el puente con el código
  son las trazas derivadas del modelo y las pruebas de Kani sobre `kcore`.
* Los drivers con DMA son de confianza (ver punto 2).
* El supervisor `init` recibe las imágenes del kernel y decide qué crear:
  quien controle `init` controla qué corre.

## Cómo se mantiene al día

Cada PR que cambie un mecanismo de aislamiento (capacidades, IPC, memoria,
cells, actualización) debe:

1. decir en una sección "Verificación formal" si necesita modelo (Lean) o
   pruebas sobre el código (Kani), y por qué;
2. actualizar este documento si cambia lo que se garantiza o se descubre un
   hueco nuevo;
3. pasar `tools/audit-unsafe.sh` (en `make lint` y en la CI).
