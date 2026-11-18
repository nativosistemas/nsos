# Registro de `unsafe`

Política:

* `crates/abi` y todas las cells de usuario: `#![forbid(unsafe_code)]`.
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

Revisión: `grep -rn "unsafe" kernel/src` debe coincidir con esta tabla.
