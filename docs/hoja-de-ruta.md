# Hoja de ruta

Cada hito se entrega como un PR con tests y documentación.

| # | Hito | Estado |
|---|---|---|
| 1 | Especificar ABI de IPC y modelo de capacidades | ✅ `docs/abi-ipc-capacidades.md`, `crates/abi` |
| 2 | Imagen booteable mínima: Limine + kernel que imprime por serial (BIOS y UEFI) | ✅ |
| 3 | Allocador físico/virtual, paginación, GDT/IDT, scheduler básico | ✅ 9 tests en QEMU + 7 en host |
| 4 | IPC seguro (canales + transferencia de capacidades) y creación de cells | ⏳ siguiente |
| 5 | "hello cell" y driver virtio de consola como cell | ○ |
| 6 | Fault containment: matar/reiniciar una cell sin tumbar el kernel | ○ |
| 7 | Actualización en vivo de una cell | ○ |
| 8 | Tests de aislamiento y benchmark de IPC | ○ |
| 9 | Documentar límites de `unsafe` y modelo de amenazas | ⏳ registro inicial en `docs/unsafe.md` |
| 10 | Fase 2: migración de cells entre dos instancias QEMU | ○ |

## Criterios de aceptación del MVP

- [x] Arranca en QEMU x86_64.
- [x] Kernel mínimo sin GC.
- [ ] Al menos 3 cells aisladas comunicándose por IPC.
- [ ] Un driver en user-space.
- [ ] Un fallo de cell no afecta al kernel.
- [ ] Actualización en vivo de una cell sin reinicio.
- [ ] `unsafe` confinado y documentado (en curso: lint obligatorio + registro).
- [ ] Benchmarks básicos de IPC y arranque (arranque: el kernel mide su tiempo desde `kmain` con el TSC; IPC pendiente).

## Hito 3 (hecho)

* GDT + TSS con pila IST para el doble fallo; IDT con 256 stubs y un único
  punto de entrada que devuelve el frame a reanudar (el cambio de contexto
  es "devolver otro frame").
* Allocador de marcos con bitmap (`crates/kcore`, sin `unsafe`, probado
  contra un modelo de referencia en el host).
* Tablas de páginas propias: HHDM sin ejecución, kernel con W^X por sección
  (text R-X, rodata R--, data RW-), NX y CR0.WP activos.
* Heap del kernel de 8 MiB con allocador seguro frente a interrupciones.
* APIC local con timer periódico a 100 Hz calibrado con el PIT; PIC 8259
  deshabilitado.
* Scheduler round-robin apropiativo de hilos de kernel; `yield` por
  interrupción de software; liberación diferida de pilas de hilos muertos.
* `ktest`: tests dentro del kernel en QEMU, más un test negativo que
  comprueba que escribir en `.rodata` produce #PF.

## Hito 4 en detalle (próximo)

1. Espacios de direcciones por cell (mitad alta compartida con el kernel).
2. Entrada a anillo 3 y syscalls con `syscall`/`sysret` (STAR/LSTAR/SFMASK).
3. CSpace por cell (`crates/kcore`, testeable en host) y objetos del kernel.
4. Canales: `channel_create/send/recv` con transferencia de capacidades.
5. Carga de cells desde módulos ELF de Limine; `cell_spawn`.
6. Dos cells de prueba intercambiando mensajes.

## Riesgos

* **Spectre**: no confiar solo en el lenguaje; MMU por cell siempre.
* **`unsafe`**: lint `undocumented_unsafe_blocks` en deny, registro en
  `docs/unsafe.md`, auditoría por PR.
* **Hardware**: empezar solo con virtio en QEMU.
* **Live update**: primero por cell (protocolo de supervisor), no kernel.
