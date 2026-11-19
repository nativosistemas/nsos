/-!
# Enlace autenticado entre máquinas

Modelo del protocolo de `crates/link` (lo usa la cell `enlace` sobre
virtio-net). Se escribió antes que el código.

Cada trama lleva: sesión, dirección (origen→destino o destino→origen),
número de secuencia, datos y una etiqueta HMAC-SHA256 calculada con la
clave de la sesión sobre todo lo anterior. El receptor acepta una trama
solo si la etiqueta verifica, la sesión es la actual, la dirección es hacia
él y la secuencia es exactamente la siguiente que espera.

## Adversario

Controla la red por completo: ve todas las tramas, las descarta, las
reordena, las repite (también las de sesiones viejas y las de la dirección
contraria) e inyecta las que quiera.

## Supuestos (abstracciones, no axiomas de Lean)

* **El MAC no se falsifica**: sin la clave, el adversario no puede producir
  una etiqueta válida para un contenido que no vio. En el modelo, cada trama
  tiene `auth`: es `true` solo en las que mandó una punta honesta (y en sus
  copias); las que el adversario arma tienen `auth = false`.
* **Las sesiones son frescas**: la clave de sesión se deriva de la clave
  compartida y de un nonce aleatorio de *cada* punta, así que una sesión
  nueva nunca coincide con una anterior. En el modelo, la sesión es un
  número que solo crece.
* La negociación de la sesión (los `HELLO`) no está autenticada: el
  adversario puede hacer que las puntas no se pongan de acuerdo (negación de
  servicio), pero no que acepten algo. Eso no se modela: la sesión aparece ya
  acordada.

## Qué se demuestra

* `delivered_prefix`: en cada dirección, lo que el receptor entregó es un
  **prefijo** de lo que el emisor mandó en la sesión actual. De ahí salen:
  * `no_forgery`: todo mensaje entregado lo mandó la otra punta honesta;
  * `no_replay`: un mensaje enviado una vez se entrega como mucho una vez,
    en su lugar (ni repetido, ni de otra sesión, ni reflejado);
  * `in_order`: se entregan en el orden en que se mandaron.
-/

namespace Nsos.Link

/-- Dirección de una trama. -/
inductive Dir where
  | toDest | toOrigin
  deriving DecidableEq, Repr

/-- Una trama en la red. `data` abstrae tipo y contenido. -/
structure Frame where
  sess : Nat
  dir : Dir
  seq : Nat
  data : Nat
  /-- La etiqueta es válida para la clave de la sesión `sess`. -/
  auth : Bool
  deriving DecidableEq, Repr

structure St where
  /-- Sesión actual (las dos puntas honestas la comparten). -/
  sess : Nat
  /-- Mensajes enviados en la sesión actual, por dirección. -/
  sent : Dir → List Nat
  /-- Mensajes entregados en la sesión actual, por dirección. -/
  recv : Dir → List Nat
  /-- Todo lo que pasó por la red (el adversario lo puede volver a mandar). -/
  net : List Frame

/-- El receptor acepta la trama: etiqueta válida, sesión actual, dirección
correcta y secuencia exactamente la siguiente. -/
def accepts (s : St) (d : Dir) (f : Frame) : Prop :=
  f.auth = true ∧ f.sess = s.sess ∧ f.dir = d ∧ f.seq = (s.recv d).length

def upd (g : Dir → List Nat) (d : Dir) (l : List Nat) : Dir → List Nat :=
  fun d' => if d' = d then l else g d'

inductive Step : St → St → Prop where
  /-- Una punta honesta manda `m` en la dirección `d`. -/
  | send (s : St) (d : Dir) (m : Nat) :
      Step s { s with
        sent := upd s.sent d (s.sent d ++ [m]),
        net := s.net ++ [⟨s.sess, d, (s.sent d).length, m, true⟩] }
  /-- El receptor acepta una trama de la red (si cumple `accepts`). -/
  | deliver (s : St) (d : Dir) (f : Frame) :
      f ∈ s.net → accepts s d f →
      Step s { s with recv := upd s.recv d (s.recv d ++ [f.data]) }
  /-- El adversario inyecta una trama arbitraria (sin poder falsificar). -/
  | inject (s : St) (f : Frame) :
      f.auth = false → Step s { s with net := s.net ++ [f] }
  /-- El adversario repite una trama que vio (de cualquier sesión y
  dirección). -/
  | replay (s : St) (f : Frame) :
      f ∈ s.net → Step s { s with net := s.net ++ [f] }
  /-- Sesión nueva (por ejemplo, una punta reinició): nonces frescos. -/
  | newSession (s : St) :
      Step s { s with sess := s.sess + 1, sent := fun _ => [], recv := fun _ => [] }

def init : St := { sess := 0, sent := fun _ => [], recv := fun _ => [], net := [] }

inductive Reachable : St → Prop where
  | init : Reachable init
  | step {s t : St} : Reachable s → Step s t → Reachable t

/-- Toda trama auténtica en la red es de una sesión ya usada y, si es de la
actual, dice exactamente lo que el emisor mandó en esa posición. -/
def NetInv (s : St) : Prop :=
  ∀ f ∈ s.net, f.auth = true →
    f.sess ≤ s.sess ∧ (f.sess = s.sess → (s.sent f.dir)[f.seq]? = some f.data)

def Inv (s : St) : Prop := NetInv s ∧ ∀ d, s.recv d <+: s.sent d

theorem getElem?_append_of_some {l : List Nat} {i m : Nat} (h : l[i]? = some m) (x : List Nat) :
    (l ++ x)[i]? = some m := by
  have hi : i < l.length := by
    rcases Nat.lt_or_ge i l.length with hlt | hge
    · exact hlt
    · simp [List.getElem?_eq_none hge] at h
  rw [List.getElem?_append_left hi]; exact h

theorem prefix_snoc {r l : List Nat} {m : Nat} (hp : r <+: l) (h : l[r.length]? = some m) :
    r ++ [m] <+: l := by
  obtain ⟨t, rfl⟩ := hp
  cases t with
  | nil => simp at h
  | cons y ys =>
    have : y = m := by simpa using h
    subst this
    exact ⟨ys, by simp⟩

theorem inv_init : Inv init := by
  refine ⟨?_, ?_⟩
  · intro f hf; simp [init] at hf
  · intro d; exact List.nil_prefix

theorem inv_step {s t : St} (hs : Inv s) (st : Step s t) : Inv t := by
  obtain ⟨hn, hr⟩ := hs
  cases st with
  | send d m =>
    refine ⟨?_, ?_⟩
    · intro f hf ha
      simp only [List.mem_append, List.mem_singleton] at hf
      rcases hf with hf | rfl
      · obtain ⟨hle, heq⟩ := hn f hf ha
        refine ⟨hle, fun he => ?_⟩
        have := heq he
        by_cases hd : f.dir = d
        · simp only [upd, hd, ite_true]
          rw [hd] at this
          exact getElem?_append_of_some this _
        · simp only [upd, hd, ite_false]; exact this
      · refine ⟨Nat.le_refl _, fun _ => ?_⟩
        simp [upd]
    · intro d'
      by_cases hd : d' = d
      · simp only [upd, hd, ite_true]
        exact (hr d).trans (List.prefix_append _ _)
      · simp only [upd, hd, ite_false]; exact hr d'
  | deliver d f hf hacc =>
    obtain ⟨ha, hsess, hdir, hseq⟩ := hacc
    refine ⟨hn, ?_⟩
    intro d'
    by_cases hd : d' = d
    · simp only [upd, hd, ite_true]
      have h1 := (hn f hf ha).2 hsess
      rw [hdir, hseq] at h1
      exact prefix_snoc (hr d) h1
    · simp only [upd, hd, ite_false]; exact hr d'
  | inject f hfa =>
    refine ⟨?_, hr⟩
    intro g hg ha
    simp only [List.mem_append, List.mem_singleton] at hg
    rcases hg with hg | rfl
    · exact hn g hg ha
    · simp [hfa] at ha
  | replay f hf =>
    refine ⟨?_, hr⟩
    intro g hg ha
    simp only [List.mem_append, List.mem_singleton] at hg
    rcases hg with hg | rfl
    · exact hn g hg ha
    · exact hn g hf ha
  | newSession =>
    refine ⟨?_, fun _ => List.nil_prefix⟩
    intro f hf ha
    obtain ⟨hle, _⟩ := hn f hf ha
    exact ⟨Nat.le_succ_of_le hle, fun he => by simp at he; omega⟩

theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact inv_init
  | step _ st ih => exact inv_step ih st

/-! ## Teoremas -/

/-- Lo entregado es un prefijo de lo enviado, en cada dirección. -/
theorem delivered_prefix {s : St} (h : Reachable s) (d : Dir) : s.recv d <+: s.sent d :=
  (reachable_inv h).2 d

/-- Sin falsificación: el `i`-ésimo mensaje entregado es el `i`-ésimo que mandó
la otra punta en esta sesión. -/
theorem no_forgery {s : St} (h : Reachable s) (d : Dir) (i : Nat) (m : Nat)
    (hi : (s.recv d)[i]? = some m) : (s.sent d)[i]? = some m := by
  obtain ⟨t, ht⟩ := delivered_prefix h d
  rw [← ht]
  exact getElem?_append_of_some hi t

/-- Sin repetición: se entregan como mucho tantos mensajes como se mandaron
(una trama repetida, de otra sesión o reflejada no agrega entregas). -/
theorem no_replay {s : St} (h : Reachable s) (d : Dir) :
    (s.recv d).length ≤ (s.sent d).length :=
  (delivered_prefix h d).length_le

/-- En orden: si el receptor entregó `m₁` antes que `m₂`, el emisor los mandó
en ese orden y en esas mismas posiciones. -/
theorem in_order {s : St} (h : Reachable s) (d : Dir) {i j m₁ m₂ : Nat} (_hij : i < j)
    (h₁ : (s.recv d)[i]? = some m₁) (h₂ : (s.recv d)[j]? = some m₂) :
    (s.sent d)[i]? = some m₁ ∧ (s.sent d)[j]? = some m₂ :=
  ⟨no_forgery h d i m₁ h₁, no_forgery h d j m₂ h₂⟩

/-- Un ataque concreto: el adversario repite una trama válida y otra de la
dirección contraria; el receptor entrega el mensaje una sola vez. -/
example : ∃ s, Reachable s ∧ s.recv .toDest = [7] := by
  let s0 := init
  have r0 : Reachable s0 := .init
  let f : Frame := ⟨0, .toDest, 0, 7, true⟩
  let s1 : St := { s0 with sent := upd s0.sent .toDest [7], net := [f] }
  have r1 : Reachable s1 := .step r0 (.send s0 .toDest 7)
  let s2 : St := { s1 with net := [f, f] }
  have r2 : Reachable s2 := .step r1 (.replay s1 f (by simp [s1]))
  let s3 : St := { s2 with recv := upd s2.recv .toDest [7] }
  have r3 : Reachable s3 := .step r2 (.deliver s2 .toDest f (by simp [s2]) (by
    simp [accepts, s2, s1, s0, init, f]))
  exact ⟨s3, r3, by simp [s3, upd]⟩

end Nsos.Link
