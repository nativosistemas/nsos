/-!
# Migración de una cell entre dos máquinas

Modelo del protocolo del hito 10, tal como lo implementan `cells/origen`,
`cells/enlace` y `cells/destino`. Es la actualización en vivo
(`Nsos.Upgrade`) con la versión nueva en *otra* máquina:

Participantes en la máquina A: la versión vieja (`old`), el supervisor
(`sup`) y el `enlace` (`proxy`), que maneja la red (el "cable") que une A
con B.
La máquina B (su `enlace`, el supervisor `destino` y la versión nueva) se ve
desde A como un solo participante remoto: lo que cruza el cable son
*bytes*, nunca capacidades.

Secuencia:
1. `sup → old`: `UPGRADE_PREPARE`; `old` entrega `UPGRADE_STATE [mem, ep]`.
2. `sup → proxy`: `MIGRATE [mem, ep]`. El enlace se queda con las dos
   capacidades y manda por el cable una *copia* del estado.
3. B arranca la versión nueva con esa copia y decide:
   * acepta: `ACCEPT` por el cable; el enlace de A avisa a `sup`, que manda
     `UPGRADE_COMMIT` a `old`. Desde ahí el enlace es un proxy: cada pedido
     del cliente que llega por `ep` viaja por el cable, lo atiende B y la
     respuesta vuelve por el cable y por `ep`.
   * rechaza: `REJECT` por el cable; el enlace devuelve `[mem, ep]` a `sup`
     y este hace `UPGRADE_ROLLBACK` a `old`, como en una actualización local.

Lo que se verifica:
* el cliente ve el contador sin saltos ni repeticiones, con migraciones
  rechazadas y aceptadas en el medio (`client_sees_consecutive`);
* nunca atienden dos versiones a la vez (`one_server`);
* B no puede tocar el estado de A: recibe una copia, así que tras un rechazo
  `old` retoma con el estado que dejó (`rollback_resumes`);
* la versión remota arranca desde el último valor que vio el cliente
  (`remote_accept_continues`).

Abstracciones: el cable entrega lo que mandó la otra punta, una vez y en
orden. No es un supuesto sobre la red: lo garantiza el enlace autenticado,
demostrado en `Nsos.Link` (`delivered_prefix`) contra un adversario que
controla la red. Si una trama se pierde, el enlace se detiene (negación de
servicio), pero nunca entrega algo falso. El proxy atiende un pedido por
vez, igual que el código.
-/

namespace Nsos.Migration

/-- Participantes en la máquina A. -/
inductive Party where
  | old | sup | proxy
  deriving DecidableEq, Repr

/-- Dónde está un recurso: en manos de un participante o en un mensaje
local camino a uno. -/
inductive Loc where
  | at (p : Party)
  | to (p : Party)
  deriving DecidableEq, Repr

/-- Recursos del servicio (en A; nunca cruzan el cable). -/
inductive Res where
  | mem | ep
  deriving DecidableEq, Repr

inductive Phase where
  /-- `old` atiende. -/
  | idle
  /-- `UPGRADE_PREPARE` viaja hacia `old` (que sigue atendiendo). -/
  | preparing
  /-- `UPGRADE_STATE [mem, ep]` viaja hacia `sup`. -/
  | handedOver
  /-- `MIGRATE [mem, ep]` viaja hacia el enlace. -/
  | toProxy
  /-- La copia del estado viaja por el cable hacia B. -/
  | onWire
  /-- B aceptó y espera pedidos; `ACCEPT` viaja por el cable hacia A. -/
  | remoteAccepted
  /-- B rechazó; `REJECT` viaja por el cable hacia A. -/
  | remoteRejected
  /-- El enlace recibió `ACCEPT`, lo pasó a `sup` y reenvía pedidos. -/
  | accepted
  /-- `UPGRADE_COMMIT` viaja hacia `old`. -/
  | committing
  /-- `old` terminó; el servicio vive en B. -/
  | done
  /-- El enlace devolvió `REJECT [mem, ep]` a `sup`. -/
  | rejected
  /-- `UPGRADE_ROLLBACK [mem, ep]` viaja hacia `old`. -/
  | rollingBack
  deriving DecidableEq, Repr

/-- El pedido que el proxy está reenviando. -/
inductive Fwd where
  | idle
  /-- Un `INC` viaja por el cable hacia B. -/
  | req
  /-- La respuesta con el valor `v` viaja por el cable hacia A. -/
  | resp (v : Nat)
  deriving DecidableEq, Repr

structure St where
  phase : Phase
  loc : Res → Loc
  /-- Contenido del objeto de estado en A. -/
  mem : Nat
  /-- La copia del estado que viaja por el cable. -/
  wire : Nat
  oldCount : Nat
  /-- Contador de la versión que corre en B. -/
  remoteCount : Nat
  fwd : Fwd
  /-- Respuestas que vio el cliente, en orden. -/
  log : List Nat

def oldServes : Phase → Bool
  | .idle | .preparing => true
  | _ => false

/-- Fases en que el enlace de A reenvía pedidos a B. -/
def proxying : Phase → Bool
  | .accepted | .committing | .done => true
  | _ => false

def all (l : Loc) : Res → Loc := fun _ => l

inductive Step : St → St → Prop where
  | serveOld (s : St) :
      oldServes s.phase → s.loc .ep = .at .old →
      Step s { s with oldCount := s.oldCount + 1, log := s.log ++ [s.oldCount + 1] }
  | prepare (s : St) :
      s.phase = .idle → Step s { s with phase := .preparing }
  | handOver (s : St) :
      s.phase = .preparing → s.loc .mem = .at .old → s.loc .ep = .at .old →
      Step s { s with phase := .handedOver, mem := s.oldCount, loc := all (.to .sup) }
  /-- `sup` pasa los recursos al enlace con `MIGRATE`. -/
  | toProxy (s : St) :
      s.phase = .handedOver → (∀ r, s.loc r = .at .sup ∨ s.loc r = .to .sup) →
      Step s { s with phase := .toProxy, loc := all (.to .proxy) }
  /-- El enlace toma los recursos y manda una copia del estado. -/
  | send (s : St) :
      s.phase = .toProxy → (∀ r, s.loc r = .to .proxy) →
      Step s { s with phase := .onWire, loc := all (.at .proxy), wire := s.mem }
  /-- B acepta la copia. No toca nada de A. -/
  | remoteAccept (s : St) :
      s.phase = .onWire → Step s { s with phase := .remoteAccepted, remoteCount := s.wire }
  /-- B rechaza la copia. No toca nada de A. -/
  | remoteReject (s : St) :
      s.phase = .onWire → Step s { s with phase := .remoteRejected }
  /-- El enlace recibe `ACCEPT` y se lo pasa a `sup`. -/
  | proxyAccept (s : St) :
      s.phase = .remoteAccepted → Step s { s with phase := .accepted }
  | commit (s : St) :
      s.phase = .accepted → Step s { s with phase := .committing }
  | oldExit (s : St) :
      s.phase = .committing → Step s { s with phase := .done }
  /-- El enlace recibe `REJECT` y devuelve los recursos a `sup`. -/
  | proxyReject (s : St) :
      s.phase = .remoteRejected → (∀ r, s.loc r = .at .proxy) →
      Step s { s with phase := .rejected, loc := all (.to .sup) }
  | rollback (s : St) :
      s.phase = .rejected → (∀ r, s.loc r = .to .sup) →
      Step s { s with phase := .rollingBack, loc := all (.to .old) }
  | resume (s : St) :
      s.phase = .rollingBack → (∀ r, s.loc r = .to .old) →
      Step s { s with phase := .idle, loc := all (.at .old), oldCount := s.mem }
  /-- El enlace saca un pedido de `ep` y lo manda por el cable. -/
  | forwardReq (s : St) :
      proxying s.phase → s.loc .ep = .at .proxy → s.fwd = .idle →
      Step s { s with fwd := .req }
  /-- B atiende el pedido y manda la respuesta. -/
  | serveRemote (s : St) :
      proxying s.phase → s.fwd = .req →
      Step s { s with remoteCount := s.remoteCount + 1, fwd := .resp (s.remoteCount + 1) }
  /-- El enlace entrega la respuesta al cliente por `ep`. -/
  | deliver (s : St) (v : Nat) :
      s.fwd = .resp v → s.loc .ep = .at .proxy →
      Step s { s with fwd := .idle, log := s.log ++ [v] }

def init : St :=
  { phase := .idle, loc := all (.at .old), mem := 0, wire := 0, oldCount := 0,
    remoteCount := 0, fwd := .idle, log := [] }

inductive Reachable : St → Prop where
  | init : Reachable init
  | step {s t : St} : Reachable s → Step s t → Reachable t

def Consecutive (l : List Nat) : Prop := l = List.range' 1 l.length

/-- Mientras el proxy reenvía: el contador remoto va un paso adelante del
cliente solo cuando la respuesta está viajando, y esa respuesta es la
siguiente. -/
def FwdInv (s : St) : Prop :=
  match s.fwd with
  | .idle | .req => s.remoteCount = s.log.length
  | .resp v => s.remoteCount = s.log.length + 1 ∧ v = s.log.length + 1

def PhaseInv (s : St) : Prop :=
  match s.phase with
  | .idle | .preparing =>
    s.loc = all (.at .old) ∧ s.oldCount = s.log.length ∧ s.fwd = .idle
  | .handedOver | .rejected => s.loc = all (.to .sup) ∧ s.mem = s.log.length ∧ s.fwd = .idle
  | .toProxy => s.loc = all (.to .proxy) ∧ s.mem = s.log.length ∧ s.fwd = .idle
  | .onWire =>
    s.loc = all (.at .proxy) ∧ s.mem = s.log.length ∧ s.wire = s.log.length ∧ s.fwd = .idle
  | .remoteAccepted => s.loc = all (.at .proxy) ∧ s.remoteCount = s.log.length ∧ s.fwd = .idle
  | .remoteRejected => s.loc = all (.at .proxy) ∧ s.mem = s.log.length ∧ s.fwd = .idle
  | .rollingBack => s.loc = all (.to .old) ∧ s.mem = s.log.length ∧ s.fwd = .idle
  | .accepted | .committing | .done => s.loc = all (.at .proxy) ∧ FwdInv s

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
  | serveOld hserve hep =>
    unfold PhaseInv at hp
    refine ⟨?_, ?_⟩
    · split at hp <;> simp_all [oldServes, consecutive_append]
    · unfold PhaseInv; split at hp <;> simp_all [oldServes]
  | forwardReq hpx hep hf =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *
    split at hp <;> simp_all [proxying, FwdInv]
  | serveRemote hpx hf =>
    refine ⟨hc, ?_⟩
    unfold PhaseInv at *
    split at hp <;> simp_all [proxying, FwdInv]
  | deliver v hf hep =>
    have hv : s.remoteCount = s.log.length + 1 ∧ v = s.log.length + 1 := by
      unfold PhaseInv at hp
      split at hp <;> simp_all [FwdInv]
    obtain ⟨hrc, rfl⟩ := hv
    refine ⟨consecutive_append hc, ?_⟩
    unfold PhaseInv at *
    split at hp <;> simp_all [FwdInv]
  | prepare h => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all
  | handOver h _ _ => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all
  | toProxy h _ => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all
  | send h _ => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all
  | remoteAccept h => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all
  | remoteReject h => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all
  | proxyAccept h => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all [FwdInv]
  | commit h => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all [FwdInv]
  | oldExit h => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all [FwdInv]
  | proxyReject h _ => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all
  | rollback h _ => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all
  | resume h _ => refine ⟨hc, ?_⟩; unfold PhaseInv at *; simp_all

theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact inv_init
  | step _ st ih => exact inv_step ih st

/-! ## Teoremas -/

/-- El cliente nunca ve un salto, una repetición ni un retroceso del
contador, aunque el servicio se mude de máquina (o el intento se rechace). -/
theorem client_sees_consecutive {s : St} (h : Reachable s) : Consecutive s.log :=
  (reachable_inv h).1

/-- Nunca atienden dos versiones a la vez: si `old` atiende tiene todo; si
B atiende, `ep` y el estado de A están en el enlace (no en `old`). -/
theorem one_server {s : St} (h : Reachable s) :
    (oldServes s.phase → ∀ r, s.loc r = .at .old) ∧
    (proxying s.phase → ∀ r, s.loc r = .at .proxy) := by
  have hp := (reachable_inv h).2
  unfold PhaseInv at hp
  constructor <;> intro hs r <;> split at hp <;> simp_all [oldServes, proxying, all]

/-- Rollback tras un rechazo remoto: `old` retoma con exactamente la cuenta
que vio el cliente. B solo tuvo una copia, así que no pudo cambiarla. -/
theorem rollback_resumes {s t : St} (h : Reachable s) (st : Step s t)
    (_hph : s.phase = .rollingBack) (ht : t.phase = .idle) :
    t.oldCount = t.log.length ∧ ∀ r, t.loc r = .at .old := by
  have hp := (reachable_inv (Reachable.step h st)).2
  unfold PhaseInv at hp
  simp [ht] at hp
  exact ⟨hp.2.1, fun r => by rw [hp.1]; rfl⟩

/-- Los pasos de B no cambian nada de A: ni el estado ni dónde están las
capacidades. -/
theorem remote_steps_local_untouched {s t : St} (st : Step s t)
    (hr : s.phase = .onWire) : t.mem = s.mem ∧ t.loc = s.loc := by
  cases st <;> simp_all [oldServes, proxying]

/-- La versión remota arranca desde el último valor que vio el cliente. -/
theorem remote_accept_continues {s t : St} (h : Reachable s) (st : Step s t)
    (ht : t.phase = .remoteAccepted) :
    t.remoteCount = t.log.length := by
  have hp := (reachable_inv (Reachable.step h st)).2
  unfold PhaseInv at hp
  simp [ht] at hp
  exact hp.2.1

/-- Una ejecución concreta: una migración rechazada (rollback) y una
aceptada; el cliente ve 1..3 sin saltos, el último valor atendido en B. -/
example : ∃ s, Reachable s ∧ s.phase = .done ∧ s.log = [1, 2, 3] := by
  let s0 := init
  have r0 : Reachable s0 := .init
  let s1 : St := { s0 with oldCount := 1, log := [1] }
  have r1 : Reachable s1 := .step r0 (.serveOld s0 rfl rfl)
  let s2 : St := { s1 with phase := .preparing }
  have r2 : Reachable s2 := .step r1 (.prepare s1 rfl)
  let s3 : St := { s2 with phase := .handedOver, mem := 1, loc := all (.to .sup) }
  have r3 : Reachable s3 := .step r2 (.handOver s2 rfl rfl rfl)
  let s4 : St := { s3 with phase := .toProxy, loc := all (.to .proxy) }
  have r4 : Reachable s4 := .step r3 (.toProxy s3 rfl (fun _ => Or.inr rfl))
  let s5 : St := { s4 with phase := .onWire, loc := all (.at .proxy), wire := 1 }
  have r5 : Reachable s5 := .step r4 (.send s4 rfl (fun _ => rfl))
  let s6 : St := { s5 with phase := .remoteRejected }
  have r6 : Reachable s6 := .step r5 (.remoteReject s5 rfl)
  let s7 : St := { s6 with phase := .rejected, loc := all (.to .sup) }
  have r7 : Reachable s7 := .step r6 (.proxyReject s6 rfl (fun _ => rfl))
  let s8 : St := { s7 with phase := .rollingBack, loc := all (.to .old) }
  have r8 : Reachable s8 := .step r7 (.rollback s7 rfl (fun _ => rfl))
  let s9 : St := { s8 with phase := .idle, loc := all (.at .old), oldCount := 1 }
  have r9 : Reachable s9 := .step r8 (.resume s8 rfl (fun _ => rfl))
  let s10 : St := { s9 with oldCount := 2, log := [1, 2] }
  have r10 : Reachable s10 := .step r9 (.serveOld s9 rfl rfl)
  let s11 : St := { s10 with phase := .preparing }
  have r11 : Reachable s11 := .step r10 (.prepare s10 rfl)
  let s12 : St := { s11 with phase := .handedOver, mem := 2, loc := all (.to .sup) }
  have r12 : Reachable s12 := .step r11 (.handOver s11 rfl rfl rfl)
  let s13 : St := { s12 with phase := .toProxy, loc := all (.to .proxy) }
  have r13 : Reachable s13 := .step r12 (.toProxy s12 rfl (fun _ => Or.inr rfl))
  let s14 : St := { s13 with phase := .onWire, loc := all (.at .proxy), wire := 2 }
  have r14 : Reachable s14 := .step r13 (.send s13 rfl (fun _ => rfl))
  let s15 : St := { s14 with phase := .remoteAccepted, remoteCount := 2 }
  have r15 : Reachable s15 := .step r14 (.remoteAccept s14 rfl)
  let s16 : St := { s15 with phase := .accepted }
  have r16 : Reachable s16 := .step r15 (.proxyAccept s15 rfl)
  let s17 : St := { s16 with phase := .committing }
  have r17 : Reachable s17 := .step r16 (.commit s16 rfl)
  let s18 : St := { s17 with phase := .done }
  have r18 : Reachable s18 := .step r17 (.oldExit s17 rfl)
  let s19 : St := { s18 with fwd := .req }
  have r19 : Reachable s19 := .step r18 (.forwardReq s18 rfl rfl rfl)
  let s20 : St := { s19 with remoteCount := 3, fwd := .resp 3 }
  have r20 : Reachable s20 := .step r19 (.serveRemote s19 rfl rfl)
  let s21 : St := { s20 with fwd := .idle, log := [1, 2, 3] }
  have r21 : Reachable s21 := .step r20 (.deliver s20 3 rfl rfl)
  exact ⟨s21, r21, rfl, rfl⟩

end Nsos.Migration
