import Nsos.Contract
import Nsos.Store

/-!
# El almacén sigue sus contratos

`Nsos/Store.lean` (parte B) modela lo que hace el almacén con cada pedido
(`Exec`): abrir un objeto por la raíz de un dueño, copiar un handle de solo
lectura, escribir y cerrar. Las cells hablan los contratos `store_root` (por
la raíz) y `store_obj` (por cada handle) de `nsos_contract::protocols`, cuyas
tablas están en `Nsos/Contract.lean`.

Aquí se demuestra que cada pedido del modelo es un intercambio completo del
contrato de su canal, de reposo a reposo: el pedido y una respuesta que el
contrato permite. Cuando el modelo lo atiende (`Exec`), la respuesta es la
de éxito (`exec_follows`); cuando no (por ejemplo, escribir con un handle
de solo lectura), el contrato prevé la de error, que en `Write` y `Read`
devuelve el búfer (`rejected_follows`). `Read` y `Stat` no cambian el estado
del modelo, pero también son intercambios del contrato (`read_answered`,
`stat_answered`).

Lo que el contrato no puede expresar y garantiza el tipo: `Open` no tiene
campo de dueño (el dueño sale de la raíz por la que llega, la premisa de
`Store.rw_handle_from_owner`), y las respuestas a `Read` y `Write` llevan el
`Mem<RW>` del pedido, así que el cliente tipado siempre lo recupera.
-/

namespace Nsos.StoreContract

open Nsos.Contract (Contract Cfg Steps Step)

/-- Canal en reposo: el cliente puede pedir y no hay nada en vuelo. -/
def idle : Cfg := ⟨0, 0, [], []⟩

/-- El contrato `C` permite, desde el reposo, el pedido `q` del cliente y la
respuesta `r` del servidor, que vuelve al reposo. -/
def Answered (C : Contract) (q r : Nat) : Prop :=
  ∃ tq ∈ C.trans, ∃ tr ∈ C.trans,
    tq.src = 0 ∧ tq.dir = .ab ∧ tq.label = q ∧
    tr.src = tq.dst ∧ tr.dir = .ba ∧ tr.label = r ∧ tr.dst = 0

instance (C : Contract) (q r : Nat) : Decidable (Answered C q r) := by
  unfold Answered; exact inferInstance

/-- Un intercambio son cuatro pasos del contrato: el cliente manda, el
servidor recibe, contesta y el cliente recibe. -/
theorem answered_steps {C : Contract} {q r : Nat} (h : Answered C q r) :
    Steps C idle idle := by
  obtain ⟨tq, hq, tr, hr, hs, hd, rfl, hs', hd', rfl, hdst⟩ := h
  have s1 : Step C idle ⟨tq.dst, 0, [], [tq.label]⟩ :=
    Step.sendA idle tq hq hs hd
  have s2 : Step C ⟨tq.dst, 0, [], [tq.label]⟩ ⟨tq.dst, tq.dst, [], []⟩ :=
    Step.recvB _ tq [] rfl hq hs hd
  have s3 : Step C ⟨tq.dst, tq.dst, [], []⟩ ⟨tq.dst, 0, [tr.label], []⟩ := by
    have := Step.sendB (C := C) ⟨tq.dst, tq.dst, [], []⟩ tr hr hs' hd'
    rwa [hdst] at this
  have s4 : Step C ⟨tq.dst, 0, [tr.label], []⟩ idle := by
    have := Step.recvA (C := C) ⟨tq.dst, 0, [tr.label], []⟩ tr [] rfl hr hs' hd'
    rwa [hdst] at this
  exact Steps.step s1 (Steps.step s2 (Steps.step s3 (Steps.step s4 (Steps.refl _))))

/-- El canal y el intercambio de cada pedido del modelo, con la respuesta
de éxito; `none` para cerrar (no hay mensajes: el kernel lo informa). -/
def exchangeOf : Store.Op → Option (Contract × Nat × Nat)
  | .openObj .. => some (Contract.storeRoot, 10, 11)
  | .dupRo .. => some (Contract.storeObj, 18, 11)
  | .write .. => some (Contract.storeObj, 16, 17)
  | .close _ => none

/-- Todo lo que el almacén del modelo atiende es un intercambio de su
contrato, del reposo al reposo. -/
theorem exec_follows {s t : Store.Srv} {op : Store.Op} (_h : Store.Exec s op t) :
    match exchangeOf op with
    | some (C, q, r) => Answered C q r ∧ Steps C idle idle
    | none => True := by
  cases op with
  | openObj =>
    simp only [exchangeOf]; exact ⟨by decide, answered_steps (q := 10) (r := 11) (by decide)⟩
  | dupRo =>
    simp only [exchangeOf]; exact ⟨by decide, answered_steps (q := 18) (r := 11) (by decide)⟩
  | write =>
    simp only [exchangeOf]; exact ⟨by decide, answered_steps (q := 16) (r := 17) (by decide)⟩
  | close => trivial

/-- Lo que el almacén rechaza también tiene su respuesta en el contrato: el
error (99), que en `Write` es `FailMem` y devuelve el búfer. -/
theorem rejected_follows (op : Store.Op) :
    match exchangeOf op with
    | some (C, q, _) => Answered C q 99
    | none => True := by
  cases op <;> simp only [exchangeOf] <;> decide

theorem read_answered : Answered Contract.storeObj 14 15 ∧ Answered Contract.storeObj 14 99 := by
  decide

theorem stat_answered : Answered Contract.storeObj 12 13 ∧ Answered Contract.storeObj 12 99 := by
  decide

/-- Lo que el cliente pide con un búfer (`Read`, `Write`) solo se contesta
con mensajes de esos estados (`Data`, `Written` o el error), que en la
crate llevan el mismo `Mem<RW>`: el contrato no le deja al almacén otra
salida que devolverlo. -/
theorem buffer_states_reply_only :
    ∀ t ∈ Contract.storeObj.trans, (t.src = 2 ∨ t.src = 3) →
      t.dir = .ba ∧ t.dst = 0 ∧ (t.label = 15 ∨ t.label = 17 ∨ t.label = 99) := by
  decide

end Nsos.StoreContract
