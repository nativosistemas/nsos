import Nsos.Security

/-!
# El contrato de un canal no cambia

Etapa 5 de `docs/contratos.md`. `channel_create(out, contrato)` guarda en
el canal el contrato que declara quien lo crea (un número; 0 = sin
contrato) y `channel_contract(h)` lo devuelve. En el modelo es
`Sys.contract`, indexado por el par: los dos extremos lo comparten.

Lo que se demuestra:
* `step_contract_fixed`, `run_contract_fixed`: ninguna syscall cambia el
  contrato de un canal que ya existe. Ni mandarlo, ni duplicarlo, ni el
  recolector: nadie puede **reetiquetar** un extremo.
* `step_create_contract`: el canal nuevo tiene exactamente el contrato que
  pidió quien lo creó.
* `peer_contract`: los dos extremos de un canal tienen el mismo contrato.
* `held_contract`: corolario con `Sys.run_no_amplification`. Si una cell
  termina con una capacidad a un extremo que ya existía al principio, su
  contrato es el de entonces; si el extremo es nuevo, es el que declaró la
  syscall que lo creó.

Lo que **no** garantiza: quien crea un canal puede declarar el contrato que
quiera, y después hablar mal. El número no prueba que la otra punta cumpla
(para eso están los tipos de la cell que la usa, y una punta maliciosa
sigue siendo posible: `docs/amenazas.md`). Lo que cierra es que nadie haga
pasar un canal creado para un protocolo (por ejemplo un handle del
almacén) por uno de otro.
-/

namespace Nsos
namespace Sys

theorem gc_contract (s : Sys) : s.gc.contract = s.contract := rfl

theorem setCell_contract (s : Sys) (a : Nat) (cs : CSpace Obj) :
    (s.setCell a cs).contract = s.contract := rfl

theorem peer_contract (s : Sys) (e : Obj) : s.contract e.peer.pair = s.contract e.pair := by
  cases e; rfl

/-- Una syscall, sin la recolección: solo `create` toca `contract`, y solo
en el par nuevo. -/
theorem exec'_contract {s s' : Sys} {a : Nat} {op : Op} {r : List Nat}
    (he : s.exec' a op = .ok (s', r)) :
    ∀ p, p < s.nextPair → s'.contract p = s.contract p := by
  intro p hp
  cases op with
  | create cid =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    cases he
    simp [Nat.ne_of_lt hp]
  | dup =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; rfl
  | drop =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    cases he; rfl
  | send =>
    simp only [exec'] at he
    repeat' (first | (cases he; done) | (cases he; rfl) | split at he)
  | recv =>
    simp only [exec'] at he
    repeat' (first | (cases he; done) | (cases he; rfl) | split at he)
  | waitRecv =>
    simp only [exec'] at he
    repeat' (first | (cases he; done) | (cases he; rfl) | split at he)
  | waitSend =>
    simp only [exec'] at he
    repeat' (first | (cases he; done) | (cases he; rfl) | split at he)

/-- **Ninguna syscall reetiqueta un canal.** -/
theorem step_contract_fixed {s s' : Sys} {a : Nat} {op : Op} {r : List Nat}
    (hs : s.step a op = .ok (s', r)) :
    ∀ p, p < s.nextPair → s'.contract p = s.contract p := by
  unfold step at hs
  cases he : s.exec a op with
  | error e => rw [he] at hs; cases hs
  | ok v =>
    rw [he] at hs
    obtain ⟨s₁, r₁⟩ := v
    cases hs
    unfold exec at he
    split at he
    · cases he
    · intro p hp
      rw [gc_contract]
      exact exec'_contract he p hp

/-- El canal nuevo tiene el contrato que pidió quien lo creó. -/
theorem step_create_contract {s s' : Sys} {a cid : Nat} {r : List Nat}
    (hs : s.step a (.create cid) = .ok (s', r)) :
    s'.contract s.nextPair = cid ∧ s'.nextPair = s.nextPair + 1 := by
  unfold step at hs
  cases he : s.exec a (.create cid) with
  | error e => rw [he] at hs; cases hs
  | ok v =>
    rw [he] at hs
    obtain ⟨s₁, r₁⟩ := v
    cases hs
    unfold exec at he
    split at he
    · cases he
    · simp only [exec'] at he
      split at he; · cases he
      split at he; · cases he
      cases he
      exact ⟨by simp [gc_contract], rfl⟩

/-- En toda ejecución, los canales que ya existían conservan su contrato. -/
theorem run_contract_fixed {s s' : Sys} {tr : List (Nat × Op)} (h : Run s tr s') :
    ∀ p, p < s.nextPair → s'.contract p = s.contract p := by
  induction h with
  | nil => intro _ _; rfl
  | ok hs _ ih =>
    intro p hp
    rw [ih p (Nat.lt_of_lt_of_le hp (step_nextPair_le hs)), step_contract_fixed hs p hp]
  | err _ _ ih => exact ih

/-- Lo que tiene una cell al final de una ejecución: si es un extremo que ya
existía, con el contrato de entonces. (Que la capacidad venga de una del
principio, o sea un objeto nuevo, es `Sys.run_no_amplification`.) -/
theorem held_contract {s s' : Sys} {tr : List (Nat × Op)} (h : Run s tr s')
    {c : Cap Obj} (hold : c.obj.pair < s.nextPair) :
    s'.contract c.obj.pair = s.contract c.obj.pair :=
  run_contract_fixed h _ hold

end Sys
end Nsos
