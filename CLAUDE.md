# Instrucciones para agentes

Leé y seguí [`CONTRIBUTING.md`](CONTRIBUTING.md). Lo esencial:

* **Verificación formal en cada cambio.** Antes de escribir código, decidí
  si el cambio necesita modelo en Lean (protocolos, capacidades, IPC,
  memoria), pruebas de Kani (código de `crates/kcore`, `crates/abi` o `crates/link`), una
  prueba negativa en QEMU (propiedades que hace cumplir la CPU) o nada.
  Prioridad: todo lo que pueda romper el aislamiento. Para eso, el modelo
  va **antes** que el código y se prueba al menos una mutación que haga
  fallar la demostración.
* Cada PR lleva una sección `## Verificación formal` (la CI la exige; ver
  `.github/pull_request_template.md`).
* Si cambia lo que se garantiza o aparece un hueco, actualizá
  `docs/amenazas.md`. Todo `unsafe` nuevo va en `docs/unsafe.md`.
* Antes de abrir el PR: `make lint`, `make test`, `make smoke`,
  `make smoke-arm` y, si tocaste el modelo, `make verify` (Lean:
  `cd verification/lean && lake build`).
* Español en documentación y mensajes; identificadores en inglés.
