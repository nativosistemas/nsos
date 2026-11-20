# nsos en ARM64

Estado: el kernel y las cells corren en **QEMU `virt` (aarch64)**, arrancados
por Limine sobre UEFI. El kernel lee la máquina del **device tree** y maneja
**PCI por ECAM**, así que pasan los mismos tests que en x86_64: ktests, cells,
consola virtio, almacenamiento con virtio-blk y migración por virtio-net
entre dos QEMU. La Raspberry Pi es el paso siguiente (ver abajo).

```sh
sudo apt install qemu-system-arm qemu-efi-aarch64 mtools dosfstools
make run-arm      # imagen FAT (ESP) + QEMU virt (salir: Ctrl-A X)
make smoke-arm    # tests en QEMU, como la CI (incluye almacén y migración)
ARCH=aarch64 tools/store-test.sh    # solo el almacén
ARCH=aarch64 tools/migrate-test.sh  # solo la migración entre dos QEMU
```

## Qué cambia respecto de x86_64

| Tema | x86_64 | ARM64 |
|---|---|---|
| Arranque | Limine, BIOS o UEFI, ISO | Limine, solo UEFI, imagen FAT (`BOOTAA64.EFI`); mismas *requests* (HHDM, mapa de memoria, módulos) |
| Target de Rust | `x86_64-unknown-none` | `aarch64-unknown-none-softfloat` (kernel y cells sin FP/SIMD: no hay que guardar esos registros) |
| Modo kernel / cell | anillo 0 / anillo 3 | EL1 / EL0 |
| Entrada al kernel | IDT + `syscall` | `VBAR_EL1` (16 vectores) y `svc #0` |
| Pila de kernel al entrar desde la cell | TSS y variable de `syscall` | `SP_EL1`: el frame de un hilo en EL0 está siempre en el tope de su pila |
| Tablas de páginas | una raíz (CR3); la mitad alta se copia en cada cell | dos raíces: `TTBR1_EL1` (kernel, fija) y `TTBR0_EL1` (cell); las cells no tienen nada del kernel en sus tablas |
| Formato de las entradas | el de x86_64 | se traduce con un codec verificado (abajo) |
| El kernel no ejecuta páginas de cells | SMEP (si la CPU lo tiene) | PXN en toda página de cell (siempre) |
| El kernel no lee páginas de cells | SMAP (si la CPU lo tiene) | PAN (ARMv8.1; Cortex-A76 de la Pi 5, que es la CPU por defecto en QEMU, sí; Cortex-A72 de la Pi 4, no) |
| W^X | en las tablas | en las tablas y además `SCTLR_EL1.WXN` (lo escribible nunca se ejecuta) |
| Timer | APIC (calibrado con el PIT) | generic timer virtual (PPI 27) con GICv2 |
| Consola | COM1 | PL011 del device tree (con búfer hasta que se mapea; si el kernel muere antes, el búfer sale por semihosting) |
| Descubrir dispositivos | direcciones fijas y PCI por CF8/CFC | device tree de Limine (request DTB) y PCI por ECAM |
| BAR de E/S de PCI | los asigna el firmware | los asigna el kernel (asignador verificado); la cell los usa con la misma capacidad `IoPorts` y el kernel los traduce a la ventana de E/S |
| Disco de arranque en QEMU | CD (ISO) | NVMe (así el único virtio-blk es el del almacén) |
| Salida de QEMU en tests | `isa-debug-exit` | semihosting (bandera `semihosting`) |
| Código de un fallo de página | vector 14 | `EC` 0x24 (data abort desde EL0) |

La lógica genérica (`cell`, `syscall`, `sched`, `usercopy`, `mm`, `ktest`,
`pci`) es la misma, y las cells driver (`virtcon`, `virtnet`, `virtblk`) no
cambian: hablan virtio legacy por puertos de E/S como en x86_64. Cada arquitectura ofrece la interfaz descrita en
`kernel/src/arch/mod.rs`.

## Tablas de páginas y verificación

El recorrido de tablas (`crates/kcore/src/paging.rs`) y sus pruebas de Kani
no cambian: trabajan con entradas *neutras* (los bits de x86_64). En ARM64,
`crates/kcore/src/paging_arm64.rs` traduce cada lectura y escritura al
formato de descriptor ARM64. Como un descriptor de tabla ignora los bits de
atributos de hoja y uno de hoja ignora `APTable`/`XNTable` (con PBHA
apagado), la misma codificación sirve para los dos tipos sin saber el
nivel.

`usercopy` decide si una cell puede tocar una dirección mirando las
entradas decodificadas. Para que eso valga, lo que ve el kernel tiene que
ser lo que hace cumplir la MMU. `hw_walk` es un modelo de la MMU escrito
desde el manual de ARM, independiente del codec, y Kani demuestra:

* `codec_roundtrip`: toda entrada neutra se codifica y decodifica igual;
* `hw_agrees_with_view`: con tablas cualesquiera escritas por el codec, la
  MMU y la vista del kernel coinciden en la física y en lo que EL0 puede
  leer, escribir y ejecutar; EL1 nunca ejecuta una página de cell ni
  escribe lo que la vista marca de solo lectura, y con PAN no toca una
  página de cell;
* `map_then_hw`: de punta a punta, mapear una página con `paging::map` deja
  en la MMU exactamente los permisos pedidos.

Mutaciones probadas (cada una rompe al menos una prueba): quitar PXN de
las páginas de cells, no poner `APTable[0]` en las tablas del kernel, no
poner `APTable[1]` en las de solo lectura, y decodificar NX siempre desde
PXN.

El modelo asume `TCR_EL1.HPD{0,1}` = 0 y `HWU*` = 0 (permisos jerárquicos
activos, sin PBHA). `arch::aarch64::paging::install` los fuerza.

## Device tree y PCI

QEMU necesita `-M virt,acpi=off`: con ACPI prendido, el UEFI de QEMU no
publica el device tree y Limine no tiene qué entregar (el kernel lo dice y
se detiene).

`arch::aarch64::platform::discover` corre antes de `mm::init` (el blob está
en el HHDM de Limine) y usa dos piezas de `crates/kcore`:

* `fdt`: lector del formato `.dtb` que no confía en los bytes. Solo usa
  índices verificados y cada token avanza al menos 4 bytes, así que con
  cualquier contenido no entra en pánico y termina. Traduce las
  direcciones por los `ranges` de los buses (en la Pi, `/soc` pasa de
  `0x7e...` a `0xfe...`), sigue `stdout-path` y los alias.
* `platform`: saca del árbol el PL011 de la consola, el GICv2
  (`arm,gic-400` o `arm,cortex-a15-gic`), el controlador PCI
  (`pci-host-ecam-generic`: región ECAM, buses y ventana de E/S) y
  `/chosen/rng-seed`. Toda región se descarta si, redondeada a páginas,
  toca la RAM del mapa de memoria (`mmio_allowed`).

Después `pci::init` mapea la ventana de E/S, recorre el bus (y los buses
detrás de puentes) y le asigna a cada BAR de E/S un rango del
`IoAllocator`: rangos alineados, disjuntos y dentro de la ventana. Los
accesos de una cell (`ioport_read`/`ioport_write`) pasan por
`pio_offset`, que exige que caigan enteros en su rango y en la ventana.

Una cell en EL0 no puede saber si la CPU tiene `RNDR` (y la Cortex-A76 no lo
tiene), así que el nonce del enlace entre máquinas sale de una semilla que
le pasa el kernel: `HMAC-SHA256(rng-seed, "nsos-nonce" ‖ contador)`. Sin
semilla del firmware, la cell `enlace` no arranca.

Pruebas de Kani (ver `verification/kani.md`): `token_any`, `new_any`,
`read_cells_any`, `reg_entry_any` y `translate_any` para el lector;
`mmio_never_touches_ram`, `io_ranges_disjoint`, `io_two_ranges_disjoint`,
`pio_access_in_range` y `ecam_addr_in_region` para las reglas de la
plataforma. Siete mutaciones las hacen fallar. El iterador de nodos no
entra en Kani; lo cubren tests en el host con árboles alterados.

## Endurecimiento propio de ARM64

* `SCTLR_EL1.UMA` = 0: una cell no puede enmascarar interrupciones.
* `CNTKCTL_EL1` = 0: una cell no puede leer ni programar los timers (no
  frena el del scheduler; tampoco tiene un reloj de alta resolución propio:
  usa `clock_monotonic`).
* `CPACR_EL1.FPEN` = 0: cualquier instrucción FP/SIMD es una excepción (en
  una cell, la termina).
* `SCTLR_EL1.SPAN` = 0: PAN se vuelve a poner en cada excepción.
* Sin ASID: el TLB local se invalida entero en cada cambio de cell (más
  lento, pero sin entradas viejas de otra cell).

## Raspberry Pi (siguiente paso)

* **Arranque**: Limine en ARM64 necesita UEFI. Pi 4: firmware
  [pftf/RPi4](https://github.com/pftf/RPi4); Pi 5: el port UEFI de
  worproject. La imagen FAT que arma `ARCH=aarch64 tools/build-image.sh`
  se copia a la tarjeta SD junto al firmware.
* **Dispositivos**: el kernel ya los lee del device tree. El firmware
  UEFI de la Pi tiene que entregar el DT (en pftf/RPi4: *System Table
  Selection* en *Device Tree* o *ACPI+DT*). La Pi 4 y la 5 tienen GIC-400
  (GICv2, como ya usa nsos). Consola: el PL011 de la Pi 4 está bajo `/soc`
  (la traducción de `ranges` ya está probada con un árbol como el de la
  Pi); en la Pi 5 la consola de depuración es otro PL011 del BCM2712.
* **PCI**: el PCIe de la Pi 4 (`brcm,bcm2711-pcie`) y el de la Pi 5 (RP1)
  no son ECAM genérico: hacen falta sus propios accesos a configuración.
* **CPU**: Cortex-A72 (Pi 4) no tiene PAN. El arranque lo detecta y lo
  informa (`mm: PXN sí, PAN no`), y el test negativo `test-smap-fault`
  falla como debe (`ARM_CPU=cortex-a72 make run-arm` lo reproduce). El aislamiento de
  las cells no depende de PAN; PAN protege contra bugs del kernel.
* **Pendiente también**: el controlador PCIe de la Pi, lector de FAT32
  para la SD, `random_u64` en las cells (RNDR o una syscall), SMP.
