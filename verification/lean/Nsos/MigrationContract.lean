import Nsos.Contract
import Nsos.Migration

/-!
# La migración sigue sus contratos

`Nsos/Migration.lean` modela la migración de un servicio a otra máquina
desde la máquina A: la versión vieja, el supervisor `origen` y el `enlace`,
que para el supervisor hace de versión nueva. Las cells hablan los
contratos `handover` (supervisor ↔ vieja) y `restore` (supervisor ↔
enlace), cuyas tablas están en `Nsos/Contract.lean`. A diferencia de la
actualización local, el canal con el enlace es **el mismo** en todos los
intentos: por eso `restore` vuelve a `Start` tras un rechazo, y aquí no
hace falta ninguna excepción.

Como en `UpgradeContract.lean`: a cada fase del modelo le corresponde una
configuración de cada canal (`hcfg`, `rcfg`) y cada paso del modelo es
cero, uno o dos pasos de cada contrato (`handover_follows`,
`restore_follows`); en todo estado alcanzable valen los teoremas de
`Contract.lean` (`migration_contract_safe`).

Abstracciones: el lado de B (enlace, `destino` y la versión nueva) es el
"cable" del modelo; los pasos `remoteAccept` y `remoteReject` no tocan los
canales de A. El reenvío de pedidos (`forwardReq`, `serveRemote`,
`deliver`) tampoco: va por el canal del servicio (`counter`).
-/

namespace Nsos.MigrationContract

open Nsos.Contract (Cfg Steps Step)
open Nsos.Migration (Phase)

/-- Canal supervisor (cliente) ↔ versión vieja (servidor). -/
def hcfg : Phase → Cfg
  | .idle => ⟨0, 0, [], []⟩
  | .preparing => ⟨2, 0, [], [Contract.lPrepare]⟩
  | .handedOver => ⟨2, 1, [Contract.lState], []⟩
  | .toProxy | .onWire | .remoteAccepted | .remoteRejected | .accepted | .rejected =>
    ⟨1, 1, [], []⟩
  | .committing => ⟨3, 1, [], [Contract.lCommit]⟩
  | .done => ⟨3, 3, [], []⟩
  | .rollingBack => ⟨0, 1, [], [Contract.lRollback]⟩

/-- Canal supervisor (cliente) ↔ enlace (servidor), el mismo en todos los
intentos. -/
def rcfg : Phase → Cfg
  | .idle | .preparing | .handedOver | .rollingBack => ⟨0, 0, [], []⟩
  | .toProxy => ⟨1, 0, [], [Contract.lRestore]⟩
  | .onWire | .remoteAccepted | .remoteRejected => ⟨1, 1, [], []⟩
  | .accepted => ⟨1, 2, [Contract.lAccept], []⟩
  | .committing | .done => ⟨2, 2, [], []⟩
  | .rejected => ⟨1, 0, [Contract.lReject], []⟩

private theorem one {C : Contract.Contract} {c d : Cfg} (h : Step C c d) : Steps C c d :=
  Steps.step h (Steps.refl _)

theorem handover_follows {s t : Migration.St} (h : Migration.Step s t) :
    Steps Contract.handover (hcfg s.phase) (hcfg t.phase) := by
  cases h with
  | serveOld => exact Steps.refl _
  | forwardReq => exact Steps.refl _
  | serveRemote => exact Steps.refl _
  | deliver => exact Steps.refl _
  | send hp _ => rw [hp]; exact Steps.refl _
  | remoteAccept hp => rw [hp]; exact Steps.refl _
  | remoteReject hp => rw [hp]; exact Steps.refl _
  | proxyAccept hp => rw [hp]; exact Steps.refl _
  | proxyReject hp _ => rw [hp]; exact Steps.refl _
  | prepare hp =>
    rw [hp]
    exact one (Step.sendA (C := Contract.handover) _ ⟨0, .ab, Contract.lPrepare, 2⟩
      (by decide) rfl rfl)
  | handOver hp _ _ =>
    rw [hp]
    exact Steps.step
      (Step.recvB (C := Contract.handover) _ ⟨0, .ab, Contract.lPrepare, 2⟩ [] rfl
        (by decide) rfl rfl)
      (one (Step.sendB (C := Contract.handover) _ ⟨2, .ba, Contract.lState, 1⟩
        (by decide) rfl rfl))
  | toProxy hp _ =>
    rw [hp]
    exact one (Step.recvA (C := Contract.handover) _ ⟨2, .ba, Contract.lState, 1⟩ [] rfl
      (by decide) rfl rfl)
  | commit hp =>
    rw [hp]
    exact one (Step.sendA (C := Contract.handover) _ ⟨1, .ab, Contract.lCommit, 3⟩
      (by decide) rfl rfl)
  | oldExit hp =>
    rw [hp]
    exact one (Step.recvB (C := Contract.handover) _ ⟨1, .ab, Contract.lCommit, 3⟩ [] rfl
      (by decide) rfl rfl)
  | rollback hp _ =>
    rw [hp]
    exact one (Step.sendA (C := Contract.handover) _ ⟨1, .ab, Contract.lRollback, 0⟩
      (by decide) rfl rfl)
  | resume hp _ =>
    rw [hp]
    exact one (Step.recvB (C := Contract.handover) _ ⟨1, .ab, Contract.lRollback, 0⟩ [] rfl
      (by decide) rfl rfl)

theorem restore_follows {s t : Migration.St} (h : Migration.Step s t) :
    Steps Contract.restore (rcfg s.phase) (rcfg t.phase) := by
  cases h with
  | serveOld => exact Steps.refl _
  | forwardReq => exact Steps.refl _
  | serveRemote => exact Steps.refl _
  | deliver => exact Steps.refl _
  | prepare hp => rw [hp]; exact Steps.refl _
  | handOver hp _ _ => rw [hp]; exact Steps.refl _
  | remoteAccept hp => rw [hp]; exact Steps.refl _
  | remoteReject hp => rw [hp]; exact Steps.refl _
  | oldExit hp => rw [hp]; exact Steps.refl _
  | resume hp _ => rw [hp]; exact Steps.refl _
  | toProxy hp _ =>
    rw [hp]
    exact one (Step.sendA (C := Contract.restore) _ ⟨0, .ab, Contract.lRestore, 1⟩
      (by decide) rfl rfl)
  | send hp _ =>
    rw [hp]
    exact one (Step.recvB (C := Contract.restore) _ ⟨0, .ab, Contract.lRestore, 1⟩ [] rfl
      (by decide) rfl rfl)
  | proxyAccept hp =>
    rw [hp]
    exact one (Step.sendB (C := Contract.restore) _ ⟨1, .ba, Contract.lAccept, 2⟩
      (by decide) rfl rfl)
  | proxyReject hp _ =>
    rw [hp]
    exact one (Step.sendB (C := Contract.restore) _ ⟨1, .ba, Contract.lReject, 0⟩
      (by decide) rfl rfl)
  | commit hp =>
    rw [hp]
    exact one (Step.recvA (C := Contract.restore) _ ⟨1, .ba, Contract.lAccept, 2⟩ [] rfl
      (by decide) rfl rfl)
  | rollback hp _ =>
    rw [hp]
    exact one (Step.recvA (C := Contract.restore) _ ⟨1, .ba, Contract.lReject, 0⟩ [] rfl
      (by decide) rfl rfl)

theorem handover_reachable {s : Migration.St} (h : Migration.Reachable s) :
    Contract.Reachable Contract.handover (hcfg s.phase) := by
  induction h with
  | init => exact Contract.Reachable.init
  | step _ st ih => exact Contract.reachable_steps _ ih (handover_follows st)

theorem restore_reachable {s : Migration.St} (h : Migration.Reachable s) :
    Contract.Reachable Contract.restore (rcfg s.phase) := by
  induction h with
  | init => exact Contract.Reachable.init
  | step _ st ih => exact Contract.reachable_steps _ ih (restore_follows st)

/-- En todo estado alcanzable de la migración, los canales de control de A
cumplen los teoremas de `Contract.lean`. -/
theorem migration_contract_safe {s : Migration.St} (h : Migration.Reachable s) :
    (let c := hcfg s.phase
     (∀ l r, c.qa = l :: r →
        ∃ t ∈ Contract.handover.trans, t.src = c.a ∧ t.dir = .ba ∧ t.label = l) ∧
     (∀ l r, c.qb = l :: r →
        ∃ t ∈ Contract.handover.trans, t.src = c.b ∧ t.dir = .ab ∧ t.label = l) ∧
     c.qa.length + c.qb.length ≤ 2) ∧
    (let c := rcfg s.phase
     (∀ l r, c.qa = l :: r →
        ∃ t ∈ Contract.restore.trans, t.src = c.a ∧ t.dir = .ba ∧ t.label = l) ∧
     (∀ l r, c.qb = l :: r →
        ∃ t ∈ Contract.restore.trans, t.src = c.b ∧ t.dir = .ab ∧ t.label = l) ∧
     c.qa.length + c.qb.length ≤ 1) := by
  obtain ⟨h1, h2, _, _, _, h6⟩ :=
    Contract.contract_safe Contract.handover Contract.handover_check (handover_reachable h)
  obtain ⟨r1, r2, _, _, _, r6⟩ :=
    Contract.contract_safe Contract.restore Contract.restore_check (restore_reachable h)
  exact ⟨⟨h1, h2, h6⟩, ⟨r1, r2, r6⟩⟩

end Nsos.MigrationContract
