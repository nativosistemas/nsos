# nsos en ARM64

Estado: el kernel y las cells corren en **QEMU `virt` (aarch64)**, arrancados
por Limine sobre UEFI. Pasan los mismos ktests y tests de cells que en
x86_64, salvo los que necesitan PCI (consola virtio, red y migración). La
Raspberry Pi es el paso siguiente (ver abajo).

```sh
sudo apt install qemu-system-arm qemu-efi-aarch64 mtools dosfstools
make run-arm      # imagen FAT (ESP) + QEMU virt (salir: Ctrl-A X)
make smoke-arm    # tests en QEMU, como la CI
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
| El kernel no lee páginas de cells | SMAP (si la CPU lo tiene) | PAN (ARMv8.1; QEMU `-cpu max` y Pi 5 sí, Pi 4 no) |
| W^X | en las tablas | en las tablas y además `SCTLR_EL1.WXN` (lo escribible nunca se ejecuta) |
| Timer | APIC (calibrado con el PIT) | generic timer virtual (PPI 27) con GICv2 |
| Consola | COM1 | PL011 (con búfer hasta que se mapea) |
| Salida de QEMU en tests | `isa-debug-exit` | semihosting (bandera `semihosting`) |
| Código de un fallo de página | vector 14 | `EC` 0x24 (data abort desde EL0) |

La lógica genérica (`cell`, `syscall`, `sched`, `usercopy`, `mm`, `ktest`)
es la misma. Cada arquitectura ofrece la interfaz descrita en
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
* **Dispositivos**: hoy las direcciones del PL011 y del GIC son las de
  QEMU `virt`. Hay que leerlas del device tree (Limine lo entrega con la
  request DTB). La Pi 4 y la 5 tienen GIC-400 (GICv2, como ya usa nsos).
  Consola: el PL011 de la Pi 4 está en otra dirección; en la Pi 5 la
  consola de depuración es otro PL011 del RP1/BCM2712.
* **CPU**: Cortex-A72 (Pi 4) no tiene PAN. El arranque lo detecta y lo
  informa (`mm: PXN sí, PAN no`), y el test negativo `test-smap-fault`
  falla como debe (`qemu -cpu cortex-a72` lo reproduce). El aislamiento de
  las cells no depende de PAN; PAN protege contra bugs del kernel.
* **Pendiente también**: PCI (ECAM) para virtio en QEMU y para el RP1 de la
  Pi 5, `random_u64` en las cells (RNDR o una syscall), SMP.
