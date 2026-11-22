import Nsos.Contract
import Nsos.Upgrade

/-!
# La actualización en vivo sigue sus contratos

`Nsos/Upgrade.lean` modela el protocolo de actualización por fases; las
cells lo implementan con los contratos `handover` (supervisor ↔ versión
vieja) y `restore` (supervisor ↔ versión nueva) de
`nsos_contract::protocols`, cuyas tablas están en `Nsos/Contract.lean`.

Aquí se demuestra que el modelo y los contratos dicen lo mismo: cada fase
corresponde a una configuración de cada canal de control (`hcfg`, `rcfg`) y
cada paso del modelo corresponde a cero, uno o dos pasos de cada contrato
(`handover_follows`, `restore_follows`). La única excepción es la caída de
la versión nueva: su canal de control muere y el intento siguiente usa uno
nuevo, que arranca en el estado inicial.

Consecuencia (`upgrade_contract_safe`): en todo estado alcanzable del
modelo, los dos canales de control están en configuraciones alcanzables de
sus contratos, así que valen para ellos los teoremas de `Contract.lean`:
ningún mensaje inesperado, sin bloqueo de protocolo, nada pendiente al
terminar y colas acotadas (2 y 1 mensajes).

Abstracciones (además de las de los dos modelos): el modelo junta en un
paso lo que en el código son un `recv` y un `send` de la misma cell (por
ejemplo `handOver`: la vieja recibe `Prepare` y manda `State`); por eso un
paso del modelo puede ser dos del contrato. El canal del servicio
(`counter`) no se sigue aquí: `serveOld` y `serveNew` son una ida y vuelta
completa.
-/

namespace Nsos.UpgradeContract

open Nsos.Contract (Cfg Steps Step)
open Nsos.Upgrade (Phase)

/-- Canal de control supervisor (cliente) ↔ versión vieja (servidor). -/
def hcfg : Phase → Cfg
  | .idle => ⟨0, 0, [], []⟩
  | .preparing => ⟨2, 0, [], [Contract.lPrepare]⟩
  | .handedOver => ⟨2, 1, [Contract.lState], []⟩
  | .restoring | .accepted | .rejected => ⟨1, 1, [], []⟩
  | .committing => ⟨3, 1, [], [Contract.lCommit]⟩
  | .done => ⟨3, 3, [], []⟩
  | .rollingBack => ⟨0, 1, [], [Contract.lRollback]⟩

/-- Canal de control supervisor (cliente) ↔ versión nueva (servidor), el
del intento en curso. -/
def rcfg : Phase → Cfg
  | .idle | .preparing | .handedOver | .rollingBack => ⟨0, 0, [], []⟩
  | .restoring => ⟨1, 0, [], [Contract.lRestore]⟩
  | .accepted => ⟨1, 2, [Contract.lAccept], []⟩
  | .rejected => ⟨1, 0, [Contract.lReject], []⟩
  | .committing | .done => ⟨2, 2, [], []⟩

/-- Pasos de una transición de la tabla. -/
private theorem one {C : Contract.Contract} {c d : Cfg} (h : Step C c d) : Steps C c d :=
  Steps.step h (Steps.refl _)

private theorem two {C : Contract.Contract} {c d e : Cfg} (h : Step C c d) (h' : Step C d e) :
    Steps C c e :=
  Steps.step h (one h')

/-- Cada paso del modelo es una secuencia de pasos de `handover`. -/
theorem handover_follows {s t : Upgrade.St} (h : Upgrade.Step s t) :
    Steps Contract.handover (hcfg s.phase) (hcfg t.phase) := by
  cases h with
  | serveOld => exact Steps.refl _
  | serveNew => exact Steps.refl _
  | scribble => exact Steps.refl _
  | accept hp _ _ => rw [hp]; exact Steps.refl _
  | reject hp _ => rw [hp]; exact Steps.refl _
  | prepare hp =>
    rw [hp]
    exact one (Step.sendA (C := Contract.handover) _ ⟨0, .ab, Contract.lPrepare, 2⟩ (by decide) rfl rfl)
  | handOver hp _ _ =>
    rw [hp]
    exact two (Step.recvB (C := Contract.handover) _ ⟨0, .ab, Contract.lPrepare, 2⟩ [] rfl (by decide) rfl rfl)
      (Step.sendB (C := Contract.handover) _ ⟨2, .ba, Contract.lState, 1⟩ (by decide) rfl rfl)
  | forward hp _ =>
    rw [hp]
    exact one (Step.recvA (C := Contract.handover) _ ⟨2, .ba, Contract.lState, 1⟩ [] rfl (by decide) rfl rfl)
  | crash hp =>
    rw [hp]
    exact one (Step.sendA (C := Contract.handover) _ ⟨1, .ab, Contract.lRollback, 0⟩ (by decide) rfl rfl)
  | commit hp =>
    rw [hp]
    exact one (Step.sendA (C := Contract.handover) _ ⟨1, .ab, Contract.lCommit, 3⟩ (by decide) rfl rfl)
  | oldExit hp =>
    rw [hp]
    exact one (Step.recvB (C := Contract.handover) _ ⟨1, .ab, Contract.lCommit, 3⟩ [] rfl (by decide) rfl rfl)
  | rollback hp _ =>
    rw [hp]
    exact one (Step.sendA (C := Contract.handover) _ ⟨1, .ab, Contract.lRollback, 0⟩ (by decide) rfl rfl)
  | resume hp _ =>
    rw [hp]
    exact one (Step.recvB (C := Contract.handover) _ ⟨1, .ab, Contract.lRollback, 0⟩ [] rfl (by decide) rfl rfl)

/-- Cada paso del modelo es una secuencia de pasos de `restore`, salvo la
caída de la versión nueva, después de la cual el canal es uno nuevo. -/
theorem restore_follows {s t : Upgrade.St} (h : Upgrade.Step s t) :
    Steps Contract.restore (rcfg s.phase) (rcfg t.phase) ∨
      (s.phase = .restoring ∧ t.phase = .rollingBack ∧
        rcfg t.phase = Contract.init Contract.restore) := by
  cases h with
  | serveOld => exact Or.inl (Steps.refl _)
  | serveNew => exact Or.inl (Steps.refl _)
  | scribble => exact Or.inl (Steps.refl _)
  | prepare hp => rw [hp]; exact Or.inl (Steps.refl _)
  | handOver hp _ _ => rw [hp]; exact Or.inl (Steps.refl _)
  | oldExit hp => rw [hp]; exact Or.inl (Steps.refl _)
  | resume hp _ => rw [hp]; exact Or.inl (Steps.refl _)
  | crash hp => exact Or.inr ⟨hp, rfl, rfl⟩
  | forward hp _ =>
    rw [hp]
    exact Or.inl (one (Step.sendA (C := Contract.restore) _ ⟨0, .ab, Contract.lRestore, 1⟩ (by decide) rfl rfl))
  | accept hp _ _ =>
    rw [hp]
    exact Or.inl (two (Step.recvB (C := Contract.restore) _ ⟨0, .ab, Contract.lRestore, 1⟩ [] rfl (by decide) rfl rfl)
      (Step.sendB (C := Contract.restore) _ ⟨1, .ba, Contract.lAccept, 2⟩ (by decide) rfl rfl))
  | reject hp _ =>
    rw [hp]
    exact Or.inl (two (Step.recvB (C := Contract.restore) _ ⟨0, .ab, Contract.lRestore, 1⟩ [] rfl (by decide) rfl rfl)
      (Step.sendB (C := Contract.restore) _ ⟨1, .ba, Contract.lReject, 0⟩ (by decide) rfl rfl))
  | commit hp =>
    rw [hp]
    exact Or.inl (one (Step.recvA (C := Contract.restore) _ ⟨1, .ba, Contract.lAccept, 2⟩ [] rfl (by decide) rfl rfl))
  | rollback hp _ =>
    rw [hp]
    exact Or.inl (one (Step.recvA (C := Contract.restore) _ ⟨1, .ba, Contract.lReject, 0⟩ [] rfl (by decide) rfl rfl))

theorem handover_reachable {s : Upgrade.St} (h : Upgrade.Reachable s) :
    Contract.Reachable Contract.handover (hcfg s.phase) := by
  induction h with
  | init => exact Contract.Reachable.init
  | step _ st ih => exact Contract.reachable_steps _ ih (handover_follows st)

theorem restore_reachable {s : Upgrade.St} (h : Upgrade.Reachable s) :
    Contract.Reachable Contract.restore (rcfg s.phase) := by
  induction h with
  | init => exact Contract.Reachable.init
  | step _ st ih =>
    rcases restore_follows st with hs | ⟨_, _, hi⟩
    · exact Contract.reachable_steps _ ih hs
    · rw [hi]; exact Contract.Reachable.init

/-- En todo estado alcanzable de la actualización, los dos canales de
control cumplen los teoremas de `Contract.lean`. -/
theorem upgrade_contract_safe {s : Upgrade.St} (h : Upgrade.Reachable s) :
    (let c := hcfg s.phase
     (∀ l r, c.qa = l :: r →
        ∃ t ∈ Contract.handover.trans, t.src = c.a ∧ t.dir = .ba ∧ t.label = l) ∧
     (∀ l r, c.qb = l :: r →
        ∃ t ∈ Contract.handover.trans, t.src = c.b ∧ t.dir = .ab ∧ t.label = l) ∧
     ((∃ d, Step Contract.handover c d) ∨
        (c.qa = [] ∧ c.qb = [] ∧ c.a = c.b ∧ Contract.Terminal Contract.handover c.a)) ∧
     c.qa.length + c.qb.length ≤ 2) ∧
    (let c := rcfg s.phase
     (∀ l r, c.qa = l :: r →
        ∃ t ∈ Contract.restore.trans, t.src = c.a ∧ t.dir = .ba ∧ t.label = l) ∧
     (∀ l r, c.qb = l :: r →
        ∃ t ∈ Contract.restore.trans, t.src = c.b ∧ t.dir = .ab ∧ t.label = l) ∧
     ((∃ d, Step Contract.restore c d) ∨
        (c.qa = [] ∧ c.qb = [] ∧ c.a = c.b ∧ Contract.Terminal Contract.restore c.a)) ∧
     c.qa.length + c.qb.length ≤ 1) := by
  obtain ⟨h1, h2, h3, _, _, h6⟩ :=
    Contract.contract_safe Contract.handover Contract.handover_check (handover_reachable h)
  obtain ⟨r1, r2, r3, _, _, r6⟩ :=
    Contract.contract_safe Contract.restore Contract.restore_check (restore_reachable h)
  exact ⟨⟨h1, h2, h3, h6⟩, ⟨r1, r2, r3, r6⟩⟩

/-- Cuando termina la actualización (la vieja salió), los dos canales de
control terminaron y no quedó nada en vuelo. -/
theorem done_clean : hcfg .done = ⟨3, 3, [], []⟩ ∧ rcfg .done = ⟨2, 2, [], []⟩ ∧
    Contract.Terminal Contract.handover 3 ∧ Contract.Terminal Contract.restore 2 := by
  refine ⟨rfl, rfl, ?_, ?_⟩ <;> (unfold Contract.Terminal; decide)

end Nsos.UpgradeContract
