/-!
# Actualización en vivo con rollback

Modelo del protocolo de supervisor de la sección 8 de
`docs/abi-ipc-capacidades.md`, tal como lo implementan `cells/init`,
`cells/contador-v1`, `cells/contador-v2` y `cells/contador-roto`.

Participantes: la versión vieja (`old`), el supervisor (`sup`) y la versión
nueva (`new`). Recursos del servicio: el objeto `Memory` con el estado
(`mem`) y el extremo de canal del cliente (`ep`).

Secuencia:
1. `sup → old`: `UPGRADE_PREPARE`.
2. `old`: deja de atender, escribe su contador en `mem` y manda
   `UPGRADE_STATE [mem, ep]` a `sup`. **No termina**: espera la decisión.
3. `sup → new`: `UPGRADE_RESTORE [mem, ep]`.
4. `new` valida el estado *sin escribirlo* y decide:
   * acepta: se queda con los recursos, responde `UPGRADE_ACCEPT` y atiende;
     `sup` manda `UPGRADE_COMMIT` a `old`, que termina;
   * rechaza: devuelve `UPGRADE_REJECT [mem, ep]`; `sup` los reenvía con
     `UPGRADE_ROLLBACK` a `old`, que relee su contador de `mem` y vuelve a
     atender. Después se puede intentar otra actualización.

Abstracciones:
* Las capacidades se *mueven* (lo garantiza el modelo del kernel:
  `Sys.send_requires_grant`, `Sys.no_forging`), así que la ubicación de
  cada recurso es una función: en un participante o viajando en el canal de
  control hacia uno. Lo que se verifica aquí es que el protocolo nunca
  actúa sin tener los recursos y que el estado viaja correctamente.
* Un pedido del cliente y su respuesta son un solo paso del servidor que
  tiene `ep`. `log` es la secuencia de respuestas que vio el cliente.
* Si `new` *falla* (excepción) con los recursos, el extremo se cierra y el
  cliente ve `PEER_CLOSED`: el rollback cubre el rechazo explícito, no la
  caída. Ver la discusión en `docs/hoja-de-ruta.md`.
-/

namespace Nsos.Upgrade

/-- Participantes del protocolo. -/
inductive Party where
  | old | sup | new
  deriving DecidableEq, Repr

/-- Dónde está un recurso: en manos de un participante o en un mensaje de
control camino a uno. -/
inductive Loc where
  | at (p : Party)
  | to (p : Party)
  deriving DecidableEq, Repr

/-- Recursos del servicio. -/
inductive Res where
  | mem | ep
  deriving DecidableEq, Repr

/-- Fase del protocolo. -/
inductive Phase where
  /-- `old` atiende; no hay actualización en curso. -/
  | idle
  /-- `UPGRADE_PREPARE` viaja hacia `old` (que sigue atendiendo). -/
  | preparing
  /-- `UPGRADE_STATE [mem, ep]` viaja hacia `sup`. -/
  | handedOver
  /-- `UPGRADE_RESTORE [mem, ep]` viaja hacia `new`. -/
  | restoring
  /-- `new` aceptó y atiende; `UPGRADE_ACCEPT` viaja hacia `sup`. -/
  | accepted
  /-- `UPGRADE_COMMIT` viaja hacia `old`; `new` atiende. -/
  | committing
  /-- `old` terminó; `new` atiende. -/
  | done
  /-- `UPGRADE_REJECT [mem, ep]` viaja hacia `sup`. -/
  | rejected
  /-- `UPGRADE_ROLLBACK [mem, ep]` viaja hacia `old`. -/
  | rollingBack
  deriving DecidableEq, Repr

/-- Estado global. -/
structure St where
  phase : Phase
  loc : Res → Loc
  /-- Contenido del objeto de estado. -/
  mem : Nat
  /-- Contador en la memoria privada de cada versión. -/
  oldCount : Nat
  newCount : Nat
  /-- Respuestas que vio el cliente, en orden. -/
  log : List Nat

/-- `old` atiende pedidos en estas fases. -/
def oldServes : Phase → Bool
  | .idle | .preparing => true
  | _ => false

/-- `new` atiende pedidos en estas fases. -/
def newServes : Phase → Bool
  | .accepted | .committing | .done => true
  | _ => false

/-- Mueve todos los recursos a `l`. -/
def all (l : Loc) : Res → Loc := fun _ => l

/-- Pasos del sistema. Cada uno exige lo que el código correspondiente
necesita tener en la mano (por ejemplo, `ep` para atender). -/
inductive Step : St → St → Prop where
  /-- `old` atiende un `INC`. -/
  | serveOld (s : St) :
      oldServes s.phase → s.loc .ep = .at .old →
      Step s { s with oldCount := s.oldCount + 1, log := s.log ++ [s.oldCount + 1] }
  /-- `new` atiende un `INC` (y guarda el contador en `mem`). -/
  | serveNew (s : St) :
      newServes s.phase → s.loc .ep = .at .new →
      Step s { s with newCount := s.newCount + 1, mem := s.newCount + 1,
                      log := s.log ++ [s.newCount + 1] }
  /-- `sup` envía `UPGRADE_PREPARE`. -/
  | prepare (s : St) :
      s.phase = .idle → Step s { s with phase := .preparing }
  /-- `old` escribe su estado y entrega los recursos. -/
  | handOver (s : St) :
      s.phase = .preparing → s.loc .mem = .at .old → s.loc .ep = .at .old →
      Step s { s with phase := .handedOver, mem := s.oldCount, loc := all (.to .sup) }
  /-- `sup` reenvía todo a `new` con `UPGRADE_RESTORE`. -/
  | forward (s : St) :
      s.phase = .handedOver → (∀ r, s.loc r = .at .sup ∨ s.loc r = .to .sup) →
      Step s { s with phase := .restoring, loc := all (.to .new) }
  /-- `new` acepta: toma los recursos y lee el contador de `mem`. -/
  | accept (s : St) :
      s.phase = .restoring → (∀ r, s.loc r = .to .new) →
      Step s { s with phase := .accepted, loc := all (.at .new), newCount := s.mem }
  /-- `new` rechaza: devuelve los recursos sin haber escrito `mem`. -/
  | reject (s : St) :
      s.phase = .restoring → (∀ r, s.loc r = .to .new) →
      Step s { s with phase := .rejected, loc := all (.to .sup) }
  /-- `sup` recibe `UPGRADE_ACCEPT` y envía `UPGRADE_COMMIT`. -/
  | commit (s : St) :
      s.phase = .accepted → Step s { s with phase := .committing }
  /-- `old` recibe `UPGRADE_COMMIT` y termina. -/
  | oldExit (s : St) :
      s.phase = .committing → Step s { s with phase := .done }
  /-- `sup` recibe `UPGRADE_REJECT` y reenvía los recursos a `old`. -/
  | rollback (s : St) :
      s.phase = .rejected → (∀ r, s.loc r = .to .sup) →
      Step s { s with phase := .rollingBack, loc := all (.to .old) }
  /-- `old` recibe `UPGRADE_ROLLBACK`, relee `mem` y vuelve a atender. -/
  | resume (s : St) :
      s.phase = .rollingBack → (∀ r, s.loc r = .to .old) →
      Step s { s with phase := .idle, loc := all (.at .old), oldCount := s.mem }

/-- Estado inicial: `old` tiene todo y todavía no atendió nada. -/
def init : St :=
  { phase := .idle, loc := all (.at .old), mem := 0, oldCount := 0, newCount := 0, log := [] }

/-- Estados alcanzables. -/
inductive Reachable : St → Prop where
  | init : Reachable init
  | step {s t : St} : Reachable s → Step s t → Reachable t

/-- Respuestas consecutivas desde 1: `[1, 2, …, n]`. -/
def Consecutive (l : List Nat) : Prop := l = List.range' 1 l.length

/-- Dónde deben estar los recursos y qué contador es el "verdadero" en cada
fase. -/
def PhaseInv (s : St) : Prop :=
  match s.phase with
  | .idle | .preparing => s.loc = all (.at .old) ∧ s.oldCount = s.log.length
  | .handedOver => s.loc = all (.to .sup) ∧ s.mem = s.log.length
  | .restoring => s.loc = all (.to .new) ∧ s.mem = s.log.length
  | .rejected => s.loc = all (.to .sup) ∧ s.mem = s.log.length
  | .rollingBack => s.loc = all (.to .old) ∧ s.mem = s.log.length
  | .accepted | .committing | .done => s.loc = all (.at .new) ∧ s.newCount = s.log.length

/-- Invariante del protocolo. -/
def Inv (s : St) : Prop := Consecutive s.log ∧ PhaseInv s

theorem range'_succ (n : Nat) : List.range' 1 (n + 1) = List.range' 1 n ++ [n + 1] := by
  rw [List.range'_concat]
  simp [Nat.add_comm]

theorem consecutive_append {l : List Nat} (h : Consecutive l) :
    Consecutive (l ++ [l.length + 1]) := by
  unfold Consecutive at *
  rw [List.length_append, List.length_singleton, range'_succ, ← h]

theorem inv_init : Inv init := by
  refine ⟨rfl, ?_⟩
  simp [PhaseInv, init]

theorem inv_step {s t : St} (hs : Inv s) (st : Step s t) : Inv t := by
  obtain ⟨hc, hp⟩ := hs
  cases st with
  | serveOld hserve _ =>
    unfold PhaseInv at hp
    refine ⟨?_, ?_⟩
    · split at hp <;> simp_all [oldServes, consecutive_append]
    · unfold PhaseInv; split at hp <;> simp_all [oldServes]
  | serveNew hserve _ =>
    unfold PhaseInv at hp
    refine ⟨?_, ?_⟩
    · split at hp <;> simp_all [newServes, consecutive_append]
    · unfold PhaseInv; split at hp <;> simp_all [newServes]
  | prepare h =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *; simp_all
  | handOver h _ _ =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *; simp_all
  | forward h _ =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *; simp_all
  | accept h _ =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *; simp_all
  | reject h _ =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *; simp_all
  | commit h =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *; simp_all
  | oldExit h =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *; simp_all
  | rollback h _ =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *; simp_all
  | resume h _ =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *; simp_all

theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact inv_init
  | step _ st ih => exact inv_step ih st

/-! ## Teoremas -/

/-- El cliente nunca ve un salto, una repetición ni un retroceso del
contador, aunque haya actualizaciones aceptadas y rechazadas en el medio. -/
theorem client_sees_consecutive {s : St} (h : Reachable s) : Consecutive s.log :=
  (reachable_inv h).1

/-- Quien atiende tiene *todos* los recursos del servicio: nunca atienden dos
versiones a la vez y nadie atiende con recursos que están viajando. -/
theorem server_holds_everything {s : St} (h : Reachable s) :
    (oldServes s.phase → ∀ r, s.loc r = .at .old) ∧
    (newServes s.phase → ∀ r, s.loc r = .at .new) := by
  have hp := (reachable_inv h).2
  unfold PhaseInv at hp
  constructor <;> intro hs r <;> split at hp <;> simp_all [oldServes, newServes, all]

/-- Rollback: cuando `old` retoma, su contador es exactamente la cantidad de
respuestas que vio el cliente, así que la próxima respuesta sigue la cuenta. -/
theorem rollback_resumes {s t : St} (h : Reachable s) (st : Step s t)
    (_hph : s.phase = .rollingBack) (ht : t.phase = .idle) :
    t.oldCount = t.log.length ∧ ∀ r, t.loc r = .at .old := by
  have hp := (reachable_inv (Reachable.step h st)).2
  unfold PhaseInv at hp
  simp [ht] at hp
  exact ⟨hp.2, fun r => by rw [hp.1]; rfl⟩

/-- Aceptar también continúa la cuenta: `new` arranca desde el último valor
que vio el cliente. -/
theorem accept_continues {s t : St} (h : Reachable s) (st : Step s t)
    (ht : t.phase = .accepted) :
    t.newCount = t.log.length := by
  have hp := (reachable_inv (Reachable.step h st)).2
  unfold PhaseInv at hp
  simp [ht] at hp
  exact hp.2

/-- Una ejecución concreta: un rechazo, rollback, y después una actualización
aceptada; el cliente ve 1..4 sin saltos. -/
example : ∃ s, Reachable s ∧ s.phase = .done ∧ s.log = [1, 2, 3, 4] := by
  let s0 := init
  have r0 : Reachable s0 := .init
  let s1 : St := { s0 with oldCount := 1, log := [1] }
  have r1 : Reachable s1 := .step r0 (.serveOld s0 rfl rfl)
  let s2 : St := { s1 with phase := .preparing }
  have r2 : Reachable s2 := .step r1 (.prepare s1 rfl)
  let s3 : St := { s2 with phase := .handedOver, mem := 1, loc := all (.to .sup) }
  have r3 : Reachable s3 := .step r2 (.handOver s2 rfl rfl rfl)
  let s4 : St := { s3 with phase := .restoring, loc := all (.to .new) }
  have r4 : Reachable s4 := .step r3 (.forward s3 rfl (fun _ => Or.inr rfl))
  let s5 : St := { s4 with phase := .rejected, loc := all (.to .sup) }
  have r5 : Reachable s5 := .step r4 (.reject s4 rfl (fun _ => rfl))
  let s6 : St := { s5 with phase := .rollingBack, loc := all (.to .old) }
  have r6 : Reachable s6 := .step r5 (.rollback s5 rfl (fun _ => rfl))
  let s7 : St := { s6 with phase := .idle, loc := all (.at .old), oldCount := 1 }
  have r7 : Reachable s7 := .step r6 (.resume s6 rfl (fun _ => rfl))
  let s8 : St := { s7 with oldCount := 2, log := [1, 2] }
  have r8 : Reachable s8 := .step r7 (.serveOld s7 rfl rfl)
  let s9 : St := { s8 with phase := .preparing }
  have r9 : Reachable s9 := .step r8 (.prepare s8 rfl)
  let s10 : St := { s9 with phase := .handedOver, mem := 2, loc := all (.to .sup) }
  have r10 : Reachable s10 := .step r9 (.handOver s9 rfl rfl rfl)
  let s11 : St := { s10 with phase := .restoring, loc := all (.to .new) }
  have r11 : Reachable s11 := .step r10 (.forward s10 rfl (fun _ => Or.inr rfl))
  let s12 : St := { s11 with phase := .accepted, loc := all (.at .new), newCount := 2 }
  have r12 : Reachable s12 := .step r11 (.accept s11 rfl (fun _ => rfl))
  let s13 : St := { s12 with newCount := 3, mem := 3, log := [1, 2, 3] }
  have r13 : Reachable s13 := .step r12 (.serveNew s12 rfl rfl)
  let s14 : St := { s13 with phase := .committing }
  have r14 : Reachable s14 := .step r13 (.commit s13 rfl)
  let s15 : St := { s14 with phase := .done }
  have r15 : Reachable s15 := .step r14 (.oldExit s14 rfl)
  let s16 : St := { s15 with newCount := 4, mem := 4, log := [1, 2, 3, 4] }
  have r16 : Reachable s16 := .step r15 (.serveNew s15 rfl rfl)
  exact ⟨s16, r16, rfl, rfl⟩

end Nsos.Upgrade
