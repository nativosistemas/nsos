# Registro de `unsafe`

Política:

* `crates/abi`, `crates/kcore`, `crates/link`, `crates/store` y `crates/contract`: `#![forbid(unsafe_code)]`.
* Cells de usuario: `#![deny(unsafe_code)]`; la única excepción es el `_start` que
  genera `nsos_rt::entry!`. El `unsafe` de usuario vive en `cells/rt` (instrucción
  `syscall`, o `svc #0` en ARM64). Las cells `intruso`, `fragil`, `desmapeo` y `sellada` lo usan a propósito para provocar fallos de página.
* Kernel: `#![deny(unsafe_op_in_unsafe_fn)]` y
  `#![deny(clippy::undocumented_unsafe_blocks)]`. Cada bloque lleva un
  comentario `// SAFETY:` con el invariante que lo justifica.
* Cada módulo con `unsafe` documenta sus invariantes en el doc del módulo y
  aparece en esta tabla. Un PR que agregue `unsafe` en un módulo nuevo debe
  actualizarla.

| Módulo | Motivo | Invariante clave |
|---|---|---|
| `kernel/src/boot/limine.rs` | leer estructuras que escribe el bootloader | Limine escribe antes de `kmain` y nunca después; la memoria sigue válida hasta reclamarla |
| `kernel/src/arch/x86_64/port.rs` | instrucciones `in`/`out` | quien crea un `Port` garantiza que el puerto es del dispositivo que maneja |
| `kernel/src/arch/x86_64/serial.rs` | construir los `Port` de COM1 | 0x3F8..0x3FD son de COM1 y solo los usa la consola (bajo lock) |
| `kernel/src/arch/x86_64/mod.rs` | `cli; hlt`, puerto `isa-debug-exit` | no acceden a memoria |
| `kernel/src/sync.rs` | `Sync` para `SpinLock`, acceso a `UnsafeCell` | acceso serializado por un `AtomicBool` con acquire/release |
| `kernel/src/arch/x86_64/cpu.rs` | CR0/CR2/CR3/CR4, MSR, `rdtsc`, `sti`/`cli`/`hlt`, `invlpg`, `clac` | escribir CR0/CR3/CR4/MSR y `clac` son `unsafe fn`: el llamador garantiza que el estado nuevo mantiene válidos código y pila (y que la CPU soporta el bit o la instrucción) |
| `kernel/src/arch/x86_64/gdt.rs` | `lgdt`, recarga de segmentos, `ltr`, escritura del TSS | se inicializa una vez en el BSP sin interrupciones; GDT/TSS son `static` |
| `kernel/src/arch/x86_64/idt.rs` | stubs en ensamblador, `lidt`, puntero al `TrapFrame`, `clac` al entrar | layout del frame fijo y verificado en compilación; el frame devuelto siempre fue guardado por un stub o fabricado por el scheduler; `clac` solo si SMAP está activo |
| `kernel/src/arch/x86_64/paging.rs` | acceso a tablas vía HHDM; activar NX/WP, SMEP/SMAP y cambiar CR3 | toda tabla proviene del allocador de marcos o de una entrada presente; el espacio nuevo mapea kernel, HHDM y la pila de arranque; SMEP/SMAP solo si CPUID los reporta |
| `kernel/src/arch/x86_64/apic.rs` | MMIO del APIC, puertos del PIC y del PIT | página MMIO mapeada sin caché; puertos exclusivos del módulo |
| `kernel/src/arch/x86_64/ioapic.rs` | MMIO del IOAPIC (selección de registro + ventana) | página MMIO en `0xFEC00000` mapeada sin caché por `mm::map_mmio`; selección y acceso siempre bajo el mismo lock |
| `kernel/src/mm/mod.rs` | direcciones de los símbolos del linker y activar el espacio del kernel (`arch::paging::install`) | el espacio nuevo mapea kernel, HHDM y la pila de arranque; el kernel nunca toca páginas de usuario (copia por el HHDM) |
| `kernel/src/mm/pmm.rs` | bitmap y puesta a cero vía HHDM | memoria `Usable` reservada en el propio bitmap; marcos recién reservados |
| `kernel/src/mm/heap.rs` | `GlobalAlloc`, región del heap | delega en `linked_list_allocator` con interrupciones deshabilitadas |
| `kernel/src/sched.rs` | fabricar el `TrapFrame` inicial, `Send` de `Thread` | el frame vive en la pila propia del hilo; solo el scheduler lo toca bajo lock |
| `kernel/src/ktest.rs` | escrituras de prueba, cambiar la mitad baja (ARM64, test de PAN) | solo en modo test; direcciones mapeadas por el propio test |
| `kernel/src/main.rs` | tests negativos `test-wx-fault` y `test-smap-fault` | solo con esas banderas; se espera el #PF |
| `kernel/src/arch/x86_64/syscall.rs` | MSR de `syscall`, stub de entrada | un solo núcleo: la pila de kernel del hilo actual está en `NSOS_KSTACK_TOP` (con SMP: por CPU vía `swapgs`) |
| `kernel/src/usercopy.rs` | copiar desde/hacia memoria de cells vía HHDM | cada página se traduce con las tablas de la cell y exige USER (y WRITABLE al escribir) |
| `kernel/src/cell.rs` | copiar segmentos ELF a marcos nuevos | marcos recién reservados; rangos validados por `kcore::elf` |
| `kernel/src/boot/limine.rs` (módulos) | leer módulos cargados por Limine | `size` bytes válidos en memoria que el kernel nunca reclama |
| `kernel/src/trap.rs` | reescribir el frame guardado de un hilo cuya cell se pidió matar | el frame es del hilo que se va a reanudar, en su propia pila de kernel; se lo convierte en un frame de modo kernel hacia `kill_trampoline` |
| `kernel/src/arch/aarch64/mod.rs` | punto de entrada (`SPSel`), `CPACR_EL1`/`CNTKCTL_EL1`, `svc`/`brk`, generic timer, salida por semihosting (`SYS_EXIT` y `SYS_WRITEC` del texto guardado si el kernel muere antes de mapear el UART) | registros de sistema de EL1 que solo restringen (FP y timers fuera del alcance de EL0); semihosting solo con la bandera `semihosting` |
| `kernel/src/arch/aarch64/platform.rs` | lectura del DTB que entrega Limine | se lee antes de `mm::init`, con el HHDM de Limine activo; primero la cabecera (40 bytes) y después `totalsize`, acotado a 2 MiB; el contenido lo valida `kcore::fdt` sin confiar en él |
| `kernel/src/arch/aarch64/cpu.rs` | `mrs` de registros de sistema, `DAIF`, `wfi`, contador | lecturas sin efectos; `DAIF` solo cambia el enmascarado de IRQ |
| `kernel/src/arch/aarch64/trap.rs` | vectores en ensamblador, `VBAR_EL1`, puntero al `TrapFrame` | layout del frame fijo y verificado en compilación; el kernel corre con `SP_EL1` y el frame de un hilo en EL0 está siempre en el tope de su pila de kernel; el frame devuelto fue guardado por los vectores o fabricado por el scheduler |
| `kernel/src/arch/aarch64/paging.rs` | tablas vía HHDM, `MAIR`/`TCR`/`TTBR0`/`TTBR1`/`SCTLR`, TLBI, PAN | como en x86_64; además `TCR` conserva tamaño y granulo y apaga HPD/PBHA (lo exige el codec verificado con Kani), `SCTLR` solo agrega WXN y quita UMA/SPAN, y PAN solo si `ID_AA64MMFR1_EL1` lo reporta |
| `kernel/src/arch/aarch64/gic.rs` | MMIO del GICv2 | bloques mapeados como dispositivo por `mm::map_mmio` |
| `kernel/src/arch/aarch64/serial.rs` | MMIO del PL011 | solo después de mapearlo como dispositivo; antes, búfer en memoria |
| `kernel/src/pci.rs` | x86_64: puertos de configuración PCI 0xCF8/0xCFC. ARM64: MMIO de ECAM y de la ventana de E/S | x86_64: solo este módulo los usa, serializado por un lock. ARM64: la página de configuración sale de `PciHost::config_addr` (dentro de la región ECAM, Kani `ecam_addr_in_region`) y el acceso de una cell a sus puertos, de `pio_offset` (dentro de su rango y de la ventana, Kani `pio_access_in_range`); las dos regiones se validaron fuera de la RAM (`mmio_allowed`) |
| `kernel/src/cell.rs` (`MemObj`) | poner a cero marcos de un objeto `Memory` y copiar en él los bytes iniciales (`from_bytes`) | marcos contiguos recién reservados, vía HHDM; la copia cabe en el objeto |
| `kernel/src/syscall.rs` (`ioport`) | `in`/`out` en nombre de un driver | el puerto es `base + off` con `off + width` dentro del rango de la capacidad `IoPorts` |
| `cells/rt/src/lib.rs` | instrucción `syscall` (`svc #0` en ARM64), `_start`, accesos volátiles de `Mapped`, `rdrand` | convención del ABI; el kernel valida todo argumento; `Mapped` solo existe tras un `mem_map` exitoso y cada acceso comprueba rango y alineación; `rdrand` solo si CPUID lo reporta |
| `cells/intruso/src/main.rs` | leer memoria del kernel a propósito | test de aislamiento: se espera #PF y que el kernel termine la cell |
| `cells/desmapeo/src/main.rs` | leer una página desmapeada a propósito | prueba de `mem_unmap`: se espera #PF y que el kernel termine la cell |
| `cells/sellada/src/main.rs` | saltar a una página de datos que la cell escribió | prueba negativa de cells selladas: la página es NX, se espera #PF (x86_64) o instruction abort (ARM64) y que el kernel termine la cell |
| `cells/fragil/src/main.rs` | escribir en la dirección 0 a propósito | test del supervisor: se espera #PF y un reinicio |

Revisión automática: `tools/audit-unsafe.sh` (en `make lint` y en la CI)
falla si un archivo usa `unsafe` sin estar en esta tabla, si la tabla nombra
un archivo que ya no lo usa, si `crates/abi` o `crates/kcore` pierden
`#![forbid(unsafe_code)]`, o si una cell no declara `#![deny(unsafe_code)]`.
También imprime cuántos usos hay por archivo, para ver en cada PR si crecen.
El modelo de amenazas está en [`amenazas.md`](amenazas.md).
