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
| Una cell no lee ni escribe memoria del kernel ni de otra cell | Tablas de páginas propias por cell; la mitad alta sin bit USER; W^X en kernel y cells; `mem_unmap` solo quita un objeto entero mapeado por la cell | `intruso` (lectura del kernel → #PF), `fragil`, `desmapeo` (página desmapeada → #PF, y 9 rangos rechazados); ktest de W^X; Lean: `Mem.step_iso`, `Mem.never_maps_foreign`, `Mem.unmap_removes`, `Mem.unmap_only_shrinks`; Kani: `map_then_translate`, `user_range_stays_low`, `unmap_target_exact` |
| El kernel no puede ser engañado para tocar memoria ajena en nombre de una cell | `usercopy` traduce cada página con las tablas de la cell y exige USER (y WRITABLE); nunca desreferencia punteros de usuario | `intruso`: punteros al kernel y a código propio; `caos`: 50 000 syscalls al azar |
| …y eso lo hace cumplir la CPU | **SMAP**: el kernel no puede acceder a páginas de usuario; **SMEP**: no puede ejecutarlas. `AC` se limpia en cada entrada (SFMASK y `clac`). En ARM64: **PAN** (se repone en cada excepción) y **PXN** en toda página de cell | `test-smap-fault`: una lectura directa del kernel a una página de usuario termina en #PF (en ARM64, en un permission fault) |
| En ARM64, lo que ve el kernel es lo que hace cumplir la MMU | Codec entre entradas neutras y descriptores ARM64 (`kcore::paging_arm64`); `TCR_EL1` con permisos jerárquicos y sin PBHA; `SCTLR_EL1.WXN` | Kani: `codec_roundtrip`, `hw_agrees_with_view`, `map_then_hw` contra un modelo de la MMU escrito desde el manual (4 mutaciones las rompen); `intruso`, `fragil` y `test-wx-fault` en QEMU `virt` |
| Una cell no frena el scheduler ni toca hardware de la CPU | x86_64: anillo 3 (sin `cli`, sin puertos fuera de `IoPorts`). ARM64: `SCTLR_EL1.UMA` = 0 (no enmascara IRQ), `CNTKCTL_EL1` = 0 (no toca timers), FP/SIMD deshabilitado | `dormilona` girando es terminada; `caos` |
| Nadie fabrica ni amplía capacidades | Handles con generación; derechos que solo se atenúan; `GRANT` para moverlas | Lean: `no_forging`, `run_no_amplification`, `send_requires_grant`, `stale_handle_never_revives`; Kani sobre `kcore`; `intruso` |
| Una interrupción va solo al driver de su línea, no satura la CPU y no se pierde | Una línea, un dueño (`IrqClaims`); la capacidad `Irq` sin `GRANT` ni `DUPLICATE`; el kernel enmascara al entregar y solo `irq_ack` con `CONTROL` desenmascara; al morir el dueño la línea queda enmascarada | Lean: `IrqModel.no_storm`, `no_lost_irq`, `ack_needs_owner`, `unowned_silent`, `claim_exclusive` (4 mutaciones las rompen); Kani: `claims_exclusive`, `intx_any`; `virtcon` intenta copiar y mandar su `Irq`; `intruso`: `irq_ack` sin capacidad, de un canal y de una memoria; ktest y los tests de migración y almacén exigen interrupciones reales |
| Una cell solo toca el hardware que se le dio | `IoPorts` con desplazamientos relativos al rango; sin acceso a puertos fuera de él. En ARM64 el kernel asigna él mismo los BAR de E/S, y el acceso pasa por la ventana de E/S mapeada | `intruso`: puertos sin capacidad, con objetos de otro tipo. Kani: `io_ranges_disjoint` (dos rangos asignados nunca se solapan y caben en la ventana) y `pio_access_in_range` (el acceso cae entero en el rango y en la ventana) |
| Un device tree roto o malicioso no rompe el kernel ni lo hace mapear memoria propia o de una cell como dispositivo (ARM64) | Lector sin índices sin verificar (`kcore::fdt`); toda región de dispositivo (UART, GIC, ECAM, ventana de E/S) se descarta si, redondeada a páginas, toca alguna región no reservada del mapa de memoria (`platform::mmio_allowed`) | Kani: `token_any`, `new_any`, `read_cells_any`, `reg_entry_any`, `translate_any` (sin pánicos, cada token avanza: todo recorrido termina), `mmio_never_touches_ram`, `ecam_addr_in_region`; tests en el host con cada byte de un árbol alterado, cada truncamiento y miles de árboles al azar (el iterador de nodos no entra en Kani) |
| Un fallo de una cell no afecta al kernel ni a las demás | Excepción en anillo 3 → se termina la cell, se cierran sus capacidades y se libera su memoria | `fragil`, `intruso`; ktest verifica que no se pierden marcos |
| Una cell colgada se puede terminar | `cell_kill` con `CONTROL`; la cell muere desde su hilo, nunca a mitad de una syscall | `dormilona` (bloqueada, girando y dormida en `wait`) |
| `wait` no deja a una cell dormida con un evento que le interesa, y solo observa lo que puede | El hilo se marca bloqueado **antes** de registrarse y mirar cada ítem bajo el lock de su objeto; quien activa una señal despierta a todos los registrados; cada handle necesita `WAIT` y señales que su tipo emita | Lean: `WaitModel.no_lost_wakeup`, `run_no_lost_wakeup` (4 mutaciones la rompen); Kani: `check_item_any`, `channel_signals_any`, `deadline_any`; `espera`, `init` y `contador-v1` en QEMU; `intruso`: 11 abusos de `wait`; `caos` |
| Un servicio se actualiza sin que el cliente note saltos | Protocolo de supervisor con rollback, escrito con los contratos `handover` y `restore`; la versión nueva solo recibe el estado como `Mem<RO>` (sin `WRITE`, comprobado al recibir) | Lean: `Upgrade.client_sees_consecutive`, `UpgradeContract.upgrade_contract_safe` (el modelo sigue los contratos; 8 mutaciones lo rompen); Kani: `restore_decode_any`; test en QEMU con rechazo, caída y aceptación |
| Un servicio se muda a otra máquina sin que el cliente note saltos, y un rechazo remoto no daña el estado local | El enlace manda una *copia* del estado; la decisión vuelve por la red; rollback local | Lean: `Migration.client_sees_consecutive`, `one_server`, `rollback_resumes`; `tools/migrate-test.sh` con dos QEMU |
| Nadie sin la clave inyecta, altera, repite, reordena ni refleja mensajes entre máquinas | HMAC-SHA256 por trama con clave de sesión (clave compartida + nonce de cada punta), dirección y secuencia exacta (`crates/link`) | Lean: `Link.delivered_prefix`, `no_forgery`, `no_replay`, `in_order` (4 mutaciones las rompen); vectores RFC 4231 y tests de ataques; Kani sobre el parser; `migrate-test.sh --clave-distinta`: un impostor no recibe nada y el servicio no sale de A |
| Una caída (corte de luz, `kill -9` a QEMU) en cualquier momento de una escritura al almacén deja el objeto viejo o el nuevo, nunca una mezcla | Copy-on-write: datos y tabla nuevos en bloques libres, superbloque con secuencia + 1 en la otra ranura, SHA-256 de todo el árbol; al montar se borra una ranura de una escritura sin terminar (`crates/store`) | Lean: `Store.crash_safe`, `commit_good`, `mount_good`, `crash_then_mount` (4 contraejemplos con `decide` muestran por qué hace falta cada regla); tests en el host con todas las combinaciones de escrituras hechas, no hechas o rotas; `tools/store-test.sh`: caída simulada y `kill -9` en momentos al azar |
| Un dueño no toca los objetos de otro; una copia de solo lectura no escribe | El dueño sale de la raíz por la que llega el pedido (el contrato `store_root` no tiene campo de dueño en `Open`); cada objeto abierto es un extremo de canal propio (capacidad); `DUP_RO` siempre quita la escritura | Lean: `Store.rw_handle_from_owner`, `change_needs_owner`, `untouched_without_owner` (2 mutaciones las rompen); `curioso` en `tools/store-test.sh` |
| El almacén no pisa un disco que no es suyo | Solo formatea si las dos ranuras están a cero o son superbloques suyos a medias | Test en el host con un sector de arranque FAT; `tools/store-test.sh`: el disco ajeno queda idéntico byte a byte |
| Una cell con contratos tipados no manda fuera de protocolo, y lo que recibe fuera de protocolo no la saca de su estado ni le deja capacidades ocupando slots | `contract!`: un tipo por estado (`send` consume el canal), decodificación según el estado; lo que no corresponde se descarta, se sueltan sus capacidades y el canal vuelve en el mismo estado. Al compilar, `check` exige estados de una sola dirección, determinismo y una cota de mensajes en vuelo que entre en la cola del kernel | Lean: `Contract.contract_safe` (sin mensajes inesperados, sin bloqueo de protocolo, nada pendiente al terminar, colas acotadas) para dos puntas que cumplen; contraejemplos `mixed_breaks`, `nondet_breaks`, `loop_unbounded`; 6 mutaciones lo rompen; `contract_model`: la `const fn` de Rust da lo mismo que el modelo en 412 contratos y las tablas son las del modelo; Kani: `decode_any`, `restore_decode_any`, `roundtrip_echo`, `roundtrip_endpoint`, `check_any`; `ping` le manda 4 mensajes fuera de protocolo a `pong` en QEMU |
| No se filtran extremos de canal en ciclos | Recolector `ipc_gc` | Lean: `step_no_leak`; ktest de ciclos |
| El `unsafe` del kernel está acotado y justificado | Lints `undocumented_unsafe_blocks`, `forbid(unsafe_code)` en `abi`/`kcore`/`link`/`store`/`contract`, `deny` en cells, registro | `tools/audit-unsafe.sh` en la CI |

## Qué **no** se garantiza

Ordenado por gravedad para el objetivo del proyecto (aislamiento).

1. **Canales laterales de microarquitectura.** El kernel está mapeado (sin
   bit USER) en el espacio de cada cell (en ARM64, en `TTBR1_EL1`, siempre
   activo) y no hay KPTI: en una CPU vulnerable
   a Meltdown, una cell podría leer memoria del kernel por ejecución
   especulativa. Tampoco hay mitigaciones de Spectre (retpolines, IBRS,
   barreras tras validar índices de syscalls). `clock_monotonic` da
   resolución de nanosegundos, útil para medir tiempos. *Mitigación futura*:
   KPTI con PCID, `lfence` tras las comprobaciones de rango, reducir la
   resolución del reloj para cells sin un derecho específico.
2. **DMA sin IOMMU.** Un driver con capacidades `Memory` con `CONTROL`
   conoce direcciones físicas y el dispositivo puede escribir en cualquier
   lugar de la RAM. Un driver es, por lo tanto, de confianza respecto a la
   memoria. *Mitigación futura*: programar el IOMMU (VT-d; SMMU en ARM64) para que cada
   dispositivo solo vea los objetos `Memory` de su driver.
3. **Mapeos que sobreviven a mover una capacidad `Memory`.** *Resuelto*:
   un mapeo vale mientras la cell tenga una capacidad al objeto con los
   derechos del mapeo. Al enviarla, pasarla a una cell hija o soltarla, el
   kernel quita el mapeo (`Cell::revoke_mappings`). Demostrado en
   `Mem.step_iso` y `Mem.send_revokes` (verification/lean). Desde el hito
   13, `mem_unmap` suelta un mapeo sin soltar la capacidad (`Mem.unmap_removes`;
   la cell `desmapeo` comprueba que leer la página desmapeada es un #PF).
4. **Agotamiento de recursos (DoS).** No hay cuotas: una cell puede llenar
   su CSpace (1024 entradas) de objetos `Memory` de hasta 1 MiB, o crear
   cells sin límite si tiene una imagen, y así dejar sin memoria al resto.
   El scheduler es round-robin sin prioridades: no hay garantías de tiempo
   real. `wait` no agrega un vector nuevo: cada objeto anota a lo sumo un
   registro por hilo y por lista, y el scheduler un plazo por hilo. *Mitigación futura*: cuotas de memoria y de cells por supervisor
   (las cuenta quien crea la cell).
5. **La consola es compartida y falsificable.** `debug_log` antepone el
   nombre de la cell solo al inicio de cada escritura; una cell puede
   escribir un salto de línea y luego texto que parezca venir de otra.
   *Mitigación futura*: prefijar cada línea o pasar la consola a una cell
   con canal propio.
6. **Caída de la versión nueva durante una actualización.** *Resuelto*
   antes de que decida: el supervisor le da el estado de solo lectura y
   guarda una copia de los extremos, así que si se cae hace rollback y los
   clientes no notan nada (`Upgrade.crash_rolls_back`). Si se cae después
   de aceptar, los clientes ven `PEER_CLOSED` y corresponde reiniciarla.
7. **El enlace entre máquinas autentica pero no cifra ni insiste.** Los
   datos (el estado migrado, los pedidos) viajan en claro. Los `HELLO` no
   están autenticados y no hay retransmisión: quien controle la red puede
   impedir la sesión o cortar el enlace (negación de servicio), pero no
   hacer aceptar nada. La clave compartida va en la imagen ISO (`enlace.key`):
   quien lea la imagen la tiene (ver punto 8). La máquina de destino confía
   en el estado que recibe de una punta autenticada: lo valida el servicio
   (número mágico), no el sistema. El enlace de origen conserva un mapeo de
   solo lectura del estado tras un rechazo. *Mitigación futura*: cifrado
   autenticado (AEAD), claves por máquina con intercambio Diffie-Hellman y
   un derecho específico para migrar.
8. **Arranque sin verificación.** Limine carga el kernel y las cells sin
   comprobar firmas; quien pueda cambiar la imagen ISO controla el sistema.
9. **Un solo núcleo.** Varias invariantes (syscalls con interrupciones
   apagadas, la pila de kernel en una variable global) asumen un CPU. Con
   SMP hay que revisarlas antes de prender el segundo núcleo.
10. **ARM64 sin PAN (Raspberry Pi 4).** El Cortex-A72 es ARMv8.0: no tiene
   PAN, así que, como en un x86 sin SMAP, la CPU no impide que un bug del
   kernel lea o escriba una página de cell directamente. El diseño no lo
   hace (`usercopy` copia por el HHDM) y PXN sigue impidiendo ejecutarlas.
   El arranque lo informa y `test-smap-fault` falla, como debe, en esa CPU.
11. **ARM64: el device tree es de confianza en lo que no se valida.** El
   kernel comprueba que las regiones de dispositivo no toquen la RAM y que
   el blob no lo haga entrar en pánico, pero cree en *qué* hay en cada
   dirección (que el PL011 sea un PL011, que la región ECAM sea PCI). Un
   firmware malo puede igual describir mal la máquina (negación de
   servicio) o esconder memoria marcándola como reservada. La semilla del
   nonce del enlace (`/chosen/rng-seed`) también viene del firmware: sin
   ella la cell `enlace` se niega a arrancar, y si el firmware la repite
   entre arranques el nonce de esta punta se puede repetir (la otra punta
   igual aporta el suyo). *Mitigación futura*: `RNDR` donde exista y un
   generador del kernel con más fuentes.

12. **Almacenamiento: el disco y el almacén son de confianza.** Los hashes
   detectan daños y escrituras a medias, pero no a alguien que reescribe el
   disco a propósito (puede armar un árbol válido con otros datos), y los
   datos van en claro. La cell `almacen` ve todos los objetos de todos los
   dueños, y el driver con DMA puede tocar cualquier memoria (punto 2). No
   hay cuotas: un dueño puede llenar el disco o los 30 objetos (negación de
   servicio para los demás). Una copia de solo lectura que se entregó no se
   puede revocar (dura hasta que quien la tiene la cierra). Las raíces las
   reparte el kernel al arrancar, por orden. *Mitigación futura*: MAC con
   una clave del almacén (o cifrado autenticado por objeto), cuotas por
   dueño, revocación de handles y un supervisor que cree las raíces.

13. **Interrupciones: líneas compartidas y firmware de confianza.** Con
   INTx, dos dispositivos pueden compartir una línea. El kernel se la da solo
   al primer driver; el otro sondea y le pide a su dispositivo que no
   interrumpa, pero un driver comprometido puede reactivar las interrupciones
   de su dispositivo y despertar al dueño de la línea de más (o hacer que se
   vuelva a disparar sin parar si nunca apaga su dispositivo): negación de
   servicio para ese driver, nunca acceso. Qué línea usa cada dispositivo lo
   dice el firmware (`Interrupt Line` en x86_64, `interrupt-map` del device
   tree en ARM64) y el IOAPIC se supone en `0xFEC00000`; un firmware malo
   puede cruzar líneas (otra vez negación de servicio). *Mitigación futura*:
   MSI/MSI-X (una interrupción por dispositivo, sin compartir) y leer el
   enrutamiento de ACPI.

14. **Los contratos de canal no los hace cumplir el kernel.** El kernel solo
   conoce mensajes con etiqueta: una cell sin contrato (cualquier ELF) puede
   mandar cualquier cosa por un canal. La cell tipada lo descarta y sigue,
   pero los teoremas de `Contract.lean` suponen que las dos puntas cumplen:
   una punta que no lee puede llenar la cola de la otra y bloquear su
   `send` (negación de servicio para ese canal, nunca acceso). Un extremo
   recibido en un mensaje se comprueba que sea un canal con `SEND` y
   `RECV`, no que su otra punta siga el contrato esperado: un emisor
   malicioso puede pasar un canal de otro protocolo, y del otro lado todo
   llega como violación. Lo mismo para la capacidad de arranque
   (`Chan::open`): el contrato lo garantiza quien reparte el canal.
   *Mitigación futura*: un `contract_id` fijo por canal que el kernel
   informe en `cap_info` (etapa 5 de [`contratos.md`](contratos.md)), y
   `send` con plazo.

## Supuestos de confianza

* El kernel (con su `unsafe` registrado) es correcto en lo que no está
  demostrado. Lo demostrado en Lean es el *modelo*; el puente con el código
  son las trazas derivadas del modelo y las pruebas de Kani sobre `kcore`.
* Los drivers con DMA son de confianza (ver punto 2).
* El firmware dice qué línea de interrupción usa cada dispositivo (ver
  punto 13).
* El firmware (UEFI y su device tree) es de confianza en lo que no se
  valida (ver punto 11).
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
