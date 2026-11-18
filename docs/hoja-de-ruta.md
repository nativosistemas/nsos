# Hoja de ruta

Cada hito se entrega como un PR con tests y documentación.

| # | Hito | Estado |
|---|---|---|
| 1 | Especificar ABI de IPC y modelo de capacidades | ✅ `docs/abi-ipc-capacidades.md`, `crates/abi` |
| 2 | Imagen booteable mínima: Limine + kernel que imprime por serial (BIOS y UEFI) | ✅ |
| 3 | Allocador físico/virtual, paginación, GDT/IDT, scheduler básico | ⏳ siguiente |
| 4 | IPC seguro (canales + transferencia de capacidades) y creación de cells | ○ |
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
- [ ] Benchmarks básicos de IPC y arranque (hoy: tiempo total de QEMU en el test de humo).

## Hito 3 en detalle (próximo)

1. GDT + TSS, IDT con handlers de excepción que imprimen y detienen.
2. Allocador de marcos físicos (bitmap) a partir del mapa de memoria de
   Limine, reclamando `BootloaderReclaimable` al final.
3. Tablas de páginas propias (4 niveles), mapeo del kernel y HHDM.
4. Heap del kernel (`alloc`) acotado.
5. APIC timer y scheduler round-robin de hilos del kernel.
6. Tests en QEMU: un mini framework `#[kernel_test]` que ejecuta pruebas al
   arrancar con `test-exit`.

## Riesgos

* **Spectre**: no confiar solo en el lenguaje; MMU por cell siempre.
* **`unsafe`**: lint `undocumented_unsafe_blocks` en deny, registro en
  `docs/unsafe.md`, auditoría por PR.
* **Hardware**: empezar solo con virtio en QEMU.
* **Live update**: primero por cell (protocolo de supervisor), no kernel.
