# Cómo contribuir a nsos

Este documento es la "constitución" del proyecto: las reglas que valen para
todo cambio, lo haga una persona o un agente. `CLAUDE.md` las resume para
los agentes y la plantilla de PR las pide en cada cambio.

## 1. Verificación formal: se piensa en cada cambio

Todo trabajo nuevo (un hito, una función, un arreglo) se diseña pensando
desde el principio si necesita verificación formal, y cada PR lo dice en
una sección **"Verificación formal"**, aunque la respuesta sea "no hace
falta, porque…". La CI rechaza un PR sin esa sección.

### Cuándo y con qué

| Si el cambio… | Herramienta | Dónde |
|---|---|---|
| define o cambia un **protocolo** entre cells o máquinas (actualización, migración, enlace, supervisión) | **Lean**: modelo de estados y teoremas | `verification/lean/Nsos/` |
| cambia el **modelo de capacidades, IPC o memoria** (quién puede tener o tocar qué) | **Lean**, extendiendo el modelo existente | `System.lean`, `Ipc.lean`, `Memory.lean` |
| cambia **código de `crates/kcore`, `crates/abi` o `crates/link`** (lógica sin `unsafe`) | **Kani** sobre el código real | `verification/kani.md` |
| agrega **`unsafe`** o toca hardware | Registro en `docs/unsafe.md` (lo exige `tools/audit-unsafe.sh`) y, si hay una propiedad que la CPU hace cumplir, una **prueba negativa** en QEMU | `tools/smoke-test.sh` |
| es solo documentación, herramientas o una cell de ejemplo sin privilegios | Nada formal; se dice por qué | — |

### Prioridad

Primero lo que puede **romper el aislamiento**: capacidades, IPC, memoria y
paginación, terminación de cells, actualización en vivo, migración y
cualquier canal entre máquinas. Para esos cambios:

1. **El modelo va antes que el código.** Se escribe (o se extiende) el
   modelo en Lean, se demuestran las propiedades y recién después se
   implementa siguiendo el modelo. El PR explica cómo se corresponden el
   modelo y el código (qué paso del modelo es qué función o mensaje).
2. **Las demostraciones tienen que poder fallar.** Se prueba al menos una
   mutación del modelo (un error plausible del código) y se comprueba que
   la demostración deja de pasar. El PR dice cuáles.
3. **Sin `sorry` ni axiomas nuevos.**
4. Si el hueco no se puede cerrar ahora, se documenta en
   `docs/amenazas.md` con su mitigación futura.

### Qué escribir en el PR

La sección "Verificación formal" responde:

* ¿Necesita modelo (Lean), pruebas sobre el código (Kani), una prueba
  negativa, o nada? ¿Por qué?
* Si hay modelo: qué teoremas, qué mutaciones se probaron y qué queda fuera
  del modelo (sus abstracciones).
* Si cambia lo que se garantiza: qué se actualizó en `docs/amenazas.md`.

## 2. Otras reglas

* Un PR por hito o por cambio coherente, con tests y documentación.
* `make lint` (formato, clippy, auditoría de `unsafe`), `make test`,
  `make smoke`, `make smoke-arm` y `make verify` tienen que pasar. Los que tocan `kcore` o
  `abi` corren también `make kani`.
* `unsafe` solo en el kernel y en `cells/rt`, siempre con un comentario
  `SAFETY:` y registrado en `docs/unsafe.md`. `abi` y `kcore` tienen
  `forbid(unsafe_code)`; las cells, `deny(unsafe_code)`.
* Documentación y mensajes en español; identificadores en inglés.
* Después de mergear, `main` tiene que compilar: si dos PRs se cruzan, se
  arregla enseguida.
