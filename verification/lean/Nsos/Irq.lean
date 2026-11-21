/-!
# Líneas de interrupción para drivers (`Irq`, `irq_ack`)

Modelo de una línea de interrupción entregada a un driver en user-space
(hito 12, segunda parte). Se escribió antes que el código del kernel
(`kernel/src/irq.rs`).

La línea es **por nivel**: el dispositivo la mantiene activa mientras tenga
algo sin atender (en virtio legacy, hasta que el driver lee el registro
ISR). El kernel nunca atiende el dispositivo: cuando llega la interrupción
enmascara la línea, marca `IRQ_PENDING` en el objeto `Irq` (lo que despierta
a quien espera en `wait`) y termina. El driver atiende el dispositivo y
llama a `irq_ack`, que borra `IRQ_PENDING` y desenmascara: si el dispositivo
todavía tiene algo, la interrupción vuelve a llegar.

Una línea tiene a lo sumo un dueño (un objeto `Irq`): el kernel la reclama
al crear el objeto y la suelta, enmascarada, cuando muere la última
capacidad.

## Pasos del modelo y su código

| Paso | Código |
|---|---|
| `devRaise` / `devLower` | el dispositivo activa la línea / el driver lee el ISR |
| `deliver` | la interrupción llega: `irq::on_irq` (enmascarar, `IRQ_PENDING`, despertar) |
| `ack o` | `irq_ack` con una capacidad `CONTROL` al objeto `o` |
| `claim o` | `irq::claim` al entregar la línea a un driver |
| `release o` | `Drop for IrqLine` (última capacidad soltada o cell muerta) |

## Qué se demuestra

* `step_inv`: el invariante se mantiene en todo paso. Con dueño, la línea
  está enmascarada exactamente cuando hay `IRQ_PENDING`; sin dueño, está
  enmascarada y no hay nada pendiente.
* `no_storm`: mientras haya `IRQ_PENDING` sin `irq_ack`, la interrupción no
  vuelve a llegar (un dispositivo que no se atiende no satura la CPU).
* `no_lost_irq`: si el dispositivo tiene la línea activa, o ya hay
  `IRQ_PENDING` (y quien espera en `wait` está despierto, por
  `WaitModel.no_lost_wakeup`) o la interrupción puede llegar.
* `ack_needs_owner`: solo el dueño de la línea la desenmascara.
* `unowned_silent`: una línea sin dueño nunca entrega nada.
* `claim_exclusive`: una línea con dueño no se puede volver a reclamar.

## Abstracciones

* Una sola línea; varias líneas son independientes (cada una con su
  estado en el controlador).
* El dispositivo es arbitrario (activa y apaga la línea cuando quiere).
* Que `irq_ack` exija el derecho `CONTROL` lo da la CSpace
  (`CSpace.lean`); aquí "tener el objeto `o`" ya es tener ese derecho.

## Mutaciones que rompen la demostración

Ver `docs/hoja-de-ruta.md` (hito 12): entregar sin enmascarar, que
`irq_ack` desenmascare sin borrar `IRQ_PENDING`, que borre `IRQ_PENDING`
sin desenmascarar, y soltar la línea sin enmascararla.
-/

namespace Nsos.IrqModel

structure St where
  /-- El dispositivo mantiene la línea activa. -/
  asserted : Bool
  /-- La línea está enmascarada en el controlador (IOAPIC o GIC). -/
  masked : Bool
  /-- `IRQ_PENDING` del objeto `Irq`. -/
  pending : Bool
  /-- Objeto `Irq` dueño de la línea. -/
  owner : Option Nat

inductive Ev where
  | devRaise
  | devLower
  | deliver
  | ack (o : Nat)
  | claim (o : Nat)
  | release (o : Nat)

def St.step (s : St) : Ev → Option St
  | .devRaise => some { s with asserted := true }
  | .devLower => some { s with asserted := false }
  | .deliver =>
    if s.asserted ∧ s.masked = false then
      some { s with masked := true, pending := true }
    else none
  | .ack o =>
    if s.owner = some o then some { s with pending := false, masked := false } else none
  | .claim o =>
    if s.owner = none then some { s with owner := some o, masked := false, pending := false }
    else none
  | .release o =>
    if s.owner = some o then some { s with owner := none, masked := true, pending := false }
    else none

def St.Inv (s : St) : Prop :=
  (s.owner.isSome → s.masked = s.pending) ∧ (s.owner = none → s.masked = true ∧ s.pending = false)

theorem step_inv {s s' : St} {e : Ev} (h : s.Inv) (hs : s.step e = some s') : s'.Inv := by
  obtain ⟨h1, h2⟩ := h
  cases e with
  | devRaise => cases hs; exact ⟨h1, h2⟩
  | devLower => cases hs; exact ⟨h1, h2⟩
  | deliver =>
    simp only [St.step] at hs
    split at hs
    · rename_i hc
      cases hs
      refine ⟨fun _ => rfl, fun ho => ?_⟩
      have := (h2 ho).1
      simp [this] at hc
    · cases hs
  | ack o =>
    simp only [St.step] at hs
    split at hs
    · rename_i ho; cases hs; refine ⟨fun _ => rfl, fun h' => ?_⟩; simp_all
    · cases hs
  | claim o =>
    simp only [St.step] at hs
    split at hs
    · cases hs; exact ⟨fun _ => rfl, fun h' => by cases h'⟩
    · cases hs
  | release o =>
    simp only [St.step] at hs
    split at hs
    · cases hs; exact ⟨fun h' => (by cases h'), fun _ => ⟨rfl, rfl⟩⟩
    · cases hs

/-- **Sin tormentas**: con `IRQ_PENDING` sin atender, la interrupción no vuelve
a llegar. -/
theorem no_storm {s : St} (h : s.Inv) (hp : s.pending = true) : s.step .deliver = none := by
  have hm : s.masked = true := by
    cases ho : s.owner
    · rw [(h.2 ho).2] at hp; cases hp
    · rw [h.1 (by simp [ho]), hp]
  simp [St.step, hm]

/-- **Ninguna interrupción se pierde**: con dueño y la línea activa, o ya hay
`IRQ_PENDING` o la interrupción puede llegar. -/
theorem no_lost_irq {s : St} (h : s.Inv) {o : Nat} (ho : s.owner = some o)
    (ha : s.asserted = true) : s.pending = true ∨ (s.step .deliver).isSome := by
  have he := h.1 (by simp [ho])
  cases hp : s.pending
  · right
    rw [hp] at he
    simp [St.step, ha, he]
  · left; rfl

/-- Solo el dueño desenmascara. -/
theorem ack_needs_owner {s s' : St} {o : Nat} (hs : s.step (.ack o) = some s') :
    s.owner = some o := by
  simp only [St.step] at hs
  split at hs
  · assumption
  · cases hs

/-- Una línea sin dueño no entrega nada. -/
theorem unowned_silent {s : St} (h : s.Inv) (ho : s.owner = none) : s.step .deliver = none := by
  simp [St.step, (h.2 ho).1]

/-- Una línea con dueño no se vuelve a reclamar. -/
theorem claim_exclusive {s : St} {o o' : Nat} (ho : s.owner = some o) :
    s.step (.claim o') = none := by
  simp [St.step, ho]

/-- Ejecuciones. -/
inductive Run : St → List Ev → St → Prop
  | nil (s : St) : Run s [] s
  | cons {s s' s'' : St} {e : Ev} {es : List Ev} :
      s.step e = some s' → Run s' es s'' → Run s (e :: es) s''

/-- Al arrancar, el kernel enmascara todas las líneas y no tienen dueño. -/
theorem inv_init {a : Bool} : St.Inv ⟨a, true, false, none⟩ :=
  ⟨fun h => (by cases h), fun _ => ⟨rfl, rfl⟩⟩

theorem run_inv {s s' : St} {es : List Ev} (hr : Run s es s') (h : s.Inv) : s'.Inv := by
  induction hr with
  | nil => exact h
  | cons hs _ ih => exact ih (step_inv h hs)

/-- En toda ejecución desde el arranque, una interrupción pendiente sin
`irq_ack` no se repite. -/
theorem run_no_storm {a : Bool} {s : St} {es : List Ev} (hr : Run ⟨a, true, false, none⟩ es s)
    (hp : s.pending = true) : s.step .deliver = none :=
  no_storm (run_inv hr inv_init) hp

end Nsos.IrqModel
