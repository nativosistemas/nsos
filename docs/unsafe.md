# Registro de `unsafe`

Política:

* `crates/abi`, `crates/kcore` y todas las cells de usuario: `#![forbid(unsafe_code)]`.
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
| `kernel/src/arch/x86_64/cpu.rs` | CR0/CR2/CR3, MSR, `rdtsc`, `sti`/`cli`/`hlt`, `invlpg` | escribir CR3/CR0/MSR es `unsafe fn`: el llamador garantiza que el estado nuevo mantiene válidos código y pila |
| `kernel/src/arch/x86_64/gdt.rs` | `lgdt`, recarga de segmentos, `ltr`, escritura del TSS | se inicializa una vez en el BSP sin interrupciones; GDT/TSS son `static` |
| `kernel/src/arch/x86_64/idt.rs` | stubs en ensamblador, `lidt`, puntero al `TrapFrame` | layout del frame fijo y verificado en compilación; el frame devuelto siempre fue guardado por un stub o fabricado por el scheduler |
| `kernel/src/arch/x86_64/paging.rs` | acceso a tablas vía HHDM | toda tabla proviene del allocador de marcos o de una entrada presente |
| `kernel/src/arch/x86_64/apic.rs` | MMIO del APIC, puertos del PIC y del PIT | página MMIO mapeada sin caché; puertos exclusivos del módulo |
| `kernel/src/mm/mod.rs` | activar NX/WP y cambiar CR3 | el espacio nuevo mapea kernel, HHDM y la pila de arranque |
| `kernel/src/mm/pmm.rs` | bitmap y puesta a cero vía HHDM | memoria `Usable` reservada en el propio bitmap; marcos recién reservados |
| `kernel/src/mm/heap.rs` | `GlobalAlloc`, región del heap | delega en `linked_list_allocator` con interrupciones deshabilitadas |
| `kernel/src/sched.rs` | fabricar el `TrapFrame` inicial, `Send` de `Thread` | el frame vive en la pila propia del hilo; solo el scheduler lo toca bajo lock |
| `kernel/src/ktest.rs` | `int3`, escrituras de prueba, lectura de tablas | solo en modo test; direcciones mapeadas por el propio test |
| `kernel/src/main.rs` | test negativo `test-wx-fault` | solo con esa bandera; se espera el #PF |

Revisión: `grep -rn "unsafe" kernel/src` debe coincidir con esta tabla.
