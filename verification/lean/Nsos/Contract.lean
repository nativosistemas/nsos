/-!
# Contratos de canal

Modelo de los contratos tipados de `crates/contract` (ver
`docs/contratos.md`). Un contrato es un autómata: estados, y transiciones
`(origen, quién manda, etiqueta, destino)`. Las dos puntas de un canal lo
siguen: el cliente (`a`) tal cual y el servidor (`b`) con los papeles
cambiados. En el código, la macro `contract!` genera los dos lados desde la
misma tabla, así que el dual nunca se escribe a mano.

Cada punta tiene una cola FIFO con los mensajes que **le llegan**, como los
canales del kernel (`Nsos/Ipc.lean`). Una punta puede:
* mandar `l` si su estado tiene una transición en la que ella manda `l`;
* recibir el primer mensaje de su cola si su estado tiene una transición en
  la que ella recibe esa etiqueta.

`check` es el control que hace la macro al compilar (la `const fn`
`nsos_contract::check`). Si acepta un contrato con cota `k`, para dos puntas
que lo siguen (`contract_safe`):
* **ningún mensaje inesperado**: el primero de cada cola siempre lo puede
  recibir su destinatario en el estado en que está (`head_receivable_a/b`);
* **sin bloqueo de protocolo**: alguien puede avanzar, salvo que las dos
  puntas estén en el mismo estado final y sin nada en vuelo (`progress`);
* **nada pendiente al terminar**: una punta en un estado final no tiene
  mensajes sin leer (`finished_no_pending_a/b`);
* **colas acotadas**: nunca hay más de `k` mensajes en vuelo
  (`queue_bound`). Con `k ≤ 64` (el límite de la cola del kernel) una punta
  que cumple nunca encuentra la cola llena mientras la otra también cumpla.

Cada condición de `check` hace falta: `mixed_breaks`, `nondet_breaks` y
`loop_unbounded` son contratos que la violan y rompen alguna propiedad.

Abstracciones:
* Solo se modelan etiquetas. Que los datos y capacidades de un mensaje
  tengan la forma que dice el contrato lo comprueba el decodificador
  generado (Kani: `decode_any` en `crates/contract`).
* Las dos puntas cumplen. Una punta que no cumple (cualquier ELF puede
  mandar cualquier cosa) no está modelada aquí: del lado tipado, el
  decodificador descarta lo que no tiene transición y el estado no cambia,
  así que lo que vale es lo de `docs/amenazas.md`, no estos teoremas.
* No se modela el cierre del canal; el kernel lo informa como `PeerClosed`.
-/

namespace Nsos.Contract

/-- Quién manda en una transición: el cliente o el servidor. -/
inductive Dir where
  | ab  -- cliente → servidor
  | ba  -- servidor → cliente
  deriving DecidableEq, Repr, Inhabited

/-- Una transición del contrato. -/
structure Trans where
  src : Nat
  dir : Dir
  label : Nat
  dst : Nat
  deriving DecidableEq, Repr

/-- Un contrato: estado inicial y tabla de transiciones. -/
structure Contract where
  start : Nat
  trans : List Trans
  deriving Repr

variable (C : Contract)

/-! ## Semántica -/

/-- Configuración de un canal: estado de cada punta y la cola de cada una
(`qa`: lo que le llega al cliente, `qb`: lo que le llega al servidor). -/
structure Cfg where
  a : Nat
  b : Nat
  qa : List Nat
  qb : List Nat
  deriving DecidableEq, Repr

/-- Pasos de las dos puntas. -/
inductive Step : Cfg → Cfg → Prop where
  | sendA (c : Cfg) (t : Trans) :
      t ∈ C.trans → t.src = c.a → t.dir = .ab →
      Step c { c with a := t.dst, qb := c.qb ++ [t.label] }
  | recvA (c : Cfg) (t : Trans) (r : List Nat) :
      c.qa = t.label :: r → t ∈ C.trans → t.src = c.a → t.dir = .ba →
      Step c { c with a := t.dst, qa := r }
  | sendB (c : Cfg) (t : Trans) :
      t ∈ C.trans → t.src = c.b → t.dir = .ba →
      Step c { c with b := t.dst, qa := c.qa ++ [t.label] }
  | recvB (c : Cfg) (t : Trans) (r : List Nat) :
      c.qb = t.label :: r → t ∈ C.trans → t.src = c.b → t.dir = .ab →
      Step c { c with b := t.dst, qb := r }

def init : Cfg := ⟨C.start, C.start, [], []⟩

inductive Reachable : Cfg → Prop where
  | init : Reachable (init C)
  | step {c d : Cfg} : Reachable c → Step C c d → Reachable d

/-- Cero o más pasos. -/
inductive Steps : Cfg → Cfg → Prop where
  | refl (c : Cfg) : Steps c c
  | step {c d e : Cfg} : Step C c d → Steps d e → Steps c e

theorem reachable_steps {c d : Cfg} (h : Reachable C c) (hs : Steps C c d) :
    Reachable C d := by
  induction hs with
  | refl => exact h
  | step st _ ih => exact ih (Reachable.step h st)

/-- Un estado sin transiciones salientes. -/
def Terminal (s : Nat) : Prop := ∀ t ∈ C.trans, t.src ≠ s

/-! ## Condiciones de buena formación -/

/-- Ningún estado mezcla direcciones: en cada uno manda una sola punta. -/
def NoMixed : Prop :=
  ∀ t₁ ∈ C.trans, ∀ t₂ ∈ C.trans, t₁.src = t₂.src → t₁.dir = t₂.dir

/-- Determinismo: una etiqueta lleva a un solo estado. -/
def Det : Prop :=
  ∀ t₁ ∈ C.trans, ∀ t₂ ∈ C.trans, t₁.src = t₂.src → t₁.label = t₂.label →
    t₁.dst = t₂.dst

/-- Camino de transiciones de la misma dirección: son mensajes que una punta
puede mandar seguidos sin que la otra conteste. -/
inductive Path (d : Dir) : Nat → List Nat → Nat → Prop where
  | nil (s : Nat) : Path d s [] s
  | cons (t : Trans) {w : List Nat} {u : Nat} :
      t ∈ C.trans → t.dir = d → Path d t.dst w u → Path d t.src (t.label :: w) u

/-- Ningún camino de una sola dirección es más largo que `k`. -/
def Bounded (k : Nat) : Prop := ∀ d s w u, Path C d s w u → w.length ≤ k

/-! ## El control (lo que hace `nsos_contract::check`) -/

def wfB : Bool :=
  C.trans.all fun t₁ => C.trans.all fun t₂ =>
    t₁.src != t₂.src || (t₁.dir == t₂.dir && (t₁.label != t₂.label || t₁.dst == t₂.dst))

def srcs : List Nat := C.trans.map (·.src)

/-- Estados a un paso en dirección `d` desde alguno de `ss`. -/
def stepsFrom (d : Dir) (ss : List Nat) : List Nat :=
  (C.trans.filter fun t => t.dir == d && ss.contains t.src).map (·.dst)

/-- Estados a exactamente `n` pasos en dirección `d`. -/
def reachN (d : Dir) : Nat → List Nat → List Nat
  | 0, ss => ss
  | n + 1, ss => reachN d n (stepsFrom C d ss)

def boundB (k : Nat) : Bool :=
  (reachN C .ab (k + 1) (srcs C)).isEmpty && (reachN C .ba (k + 1) (srcs C)).isEmpty

/-- El control: `some k` con la menor cota `k` si el contrato está bien
formado y es acotado; `none` si no. Un camino de una dirección más largo
que la tabla repite una transición, así que alcanza con probar hasta ahí. -/
def check : Option Nat :=
  if wfB C then (List.range (C.trans.length + 1)).find? (boundB C) else none

/-! ## Corrección del control -/

theorem wfB_sound (h : wfB C = true) : NoMixed C ∧ Det C := by
  simp only [wfB, List.all_eq_true] at h
  constructor
  · intro t₁ h₁ t₂ h₂ hs
    have := h t₁ h₁ t₂ h₂
    simp [hs] at this
    exact this.1
  · intro t₁ h₁ t₂ h₂ hs hl
    have := h t₁ h₁ t₂ h₂
    simp [hs, hl] at this
    exact this.2

theorem mem_stepsFrom {d : Dir} {ss : List Nat} {t : Trans}
    (ht : t ∈ C.trans) (hd : t.dir = d) (hs : t.src ∈ ss) :
    t.dst ∈ stepsFrom C d ss := by
  simp only [stepsFrom, List.mem_map, List.mem_filter]
  exact ⟨t, ⟨ht, by simp [hd, hs]⟩, rfl⟩

theorem path_reach {d : Dir} {s : Nat} {w : List Nat} {u : Nat}
    (hp : Path C d s w u) : ∀ ss, s ∈ ss → u ∈ reachN C d w.length ss := by
  induction hp with
  | nil s => intro ss h; simpa [reachN] using h
  | cons t ht hd _ ih =>
    intro ss h
    exact ih _ (mem_stepsFrom C ht hd h)

theorem path_take {d : Dir} {s : Nat} {w : List Nat} {u : Nat}
    (hp : Path C d s w u) : ∀ n, ∃ m, Path C d s (w.take n) m := by
  induction hp with
  | nil s => intro n; exact ⟨s, by simpa using Path.nil s⟩
  | cons t ht hd _ ih =>
    intro n
    cases n with
    | zero => exact ⟨t.src, by simpa using Path.nil t.src⟩
    | succ n =>
      obtain ⟨m, hm⟩ := ih n
      exact ⟨m, by simpa using Path.cons t ht hd hm⟩

theorem path_src {d : Dir} {s : Nat} {l : Nat} {w : List Nat} {u : Nat}
    (hp : Path C d s (l :: w) u) : s ∈ srcs C := by
  cases hp with
  | cons t ht _ _ => exact List.mem_map.mpr ⟨t, ht, rfl⟩

theorem boundB_sound {k : Nat} (h : boundB C k = true) : Bounded C k := by
  intro d s w u hp
  apply Classical.byContradiction
  intro hlt
  have hlen : k + 1 ≤ w.length := by omega
  obtain ⟨m, hm⟩ := path_take C hp (k + 1)
  have hl : (w.take (k + 1)).length = k + 1 := by simp; omega
  -- el prefijo no es vacío, así que arranca en un origen de la tabla
  obtain ⟨l, w', hw'⟩ : ∃ l w', w.take (k + 1) = l :: w' := by
    cases hw : w.take (k + 1) with
    | nil => rw [hw] at hl; simp at hl
    | cons l w' => exact ⟨l, w', rfl⟩
  have hs : s ∈ srcs C := path_src C (hw' ▸ hm)
  have hr := path_reach C hm (srcs C) hs
  rw [hl] at hr
  simp only [boundB, Bool.and_eq_true, List.isEmpty_iff] at h
  cases d with
  | ab => rw [h.1] at hr; simp at hr
  | ba => rw [h.2] at hr; simp at hr

theorem check_sound {k : Nat} (h : check C = some k) :
    NoMixed C ∧ Det C ∧ Bounded C k := by
  unfold check at h
  split at h
  · rename_i hw
    exact ⟨(wfB_sound C hw).1, (wfB_sound C hw).2,
      boundB_sound C (List.find?_some h)⟩
  · simp at h

/-! ## Invariante

En todo momento una punta va "adelantada" por un camino de una sola
dirección y la otra la sigue: los mensajes en vuelo son exactamente las
etiquetas de ese camino, y la cola de la que va adelante está vacía. -/

def Inv (c : Cfg) : Prop :=
  (c.qa = [] ∧ Path C .ab c.b c.qb c.a) ∨ (c.qb = [] ∧ Path C .ba c.a c.qa c.b)

theorem path_snoc {d : Dir} {s : Nat} {w : List Nat} {u : Nat}
    (hp : Path C d s w u) {t : Trans} (ht : t ∈ C.trans) (hs : t.src = u)
    (hd : t.dir = d) : Path C d s (w ++ [t.label]) t.dst := by
  induction hp with
  | nil s =>
    subst hs
    exact Path.cons t ht hd (Path.nil _)
  | cons t' ht' hd' _ ih => exact Path.cons t' ht' hd' (ih hs)

theorem path_nil_eq {d : Dir} {s u : Nat} (hp : Path C d s [] u) : s = u := by
  cases hp; rfl

/-- El primer paso de un camino. -/
theorem path_head {d : Dir} {s l : Nat} {w : List Nat} {u : Nat}
    (hp : Path C d s (l :: w) u) :
    ∃ t ∈ C.trans, t.src = s ∧ t.dir = d ∧ t.label = l ∧ Path C d t.dst w u := by
  cases hp with
  | cons t ht hd hp' => exact ⟨t, ht, rfl, hd, rfl, hp'⟩

theorem inv_init : Inv C (init C) := Or.inl ⟨rfl, Path.nil _⟩

theorem inv_step (hm : NoMixed C) (hdet : Det C) {c d : Cfg}
    (hi : Inv C c) (hs : Step C c d) : Inv C d := by
  cases hs with
  | sendA t ht hsrc hdir =>
    rcases hi with ⟨hqa, hp⟩ | ⟨hqb, hp⟩
    · exact Or.inl ⟨hqa, path_snoc C hp ht hsrc hdir⟩
    · -- el servidor iba adelante: la cola del cliente tiene que estar vacía
      cases hq : c.qa with
      | nil =>
        rw [hq] at hp
        have := path_nil_eq C hp
        refine Or.inl ⟨rfl, ?_⟩
        simp only [hqb, List.nil_append]
        have h := Path.cons t ht hdir (Path.nil (C := C) (d := .ab) t.dst)
        rw [hsrc, this] at h
        exact h
      | cons l w =>
        rw [hq] at hp
        obtain ⟨t₀, ht₀, hs₀, hd₀, _, _⟩ := path_head C hp
        have := hm t ht t₀ ht₀ (by rw [hsrc, hs₀])
        rw [hdir, hd₀] at this
        cases this
  | recvA t r hq ht hsrc hdir =>
    rcases hi with ⟨hqa, _⟩ | ⟨hqb, hp⟩
    · rw [hqa] at hq; cases hq
    · rw [hq] at hp
      obtain ⟨t₀, ht₀, hs₀, _, hl₀, hp'⟩ := path_head C hp
      have := hdet t ht t₀ ht₀ (by rw [hsrc, hs₀]) hl₀.symm
      exact Or.inr ⟨hqb, by rw [this]; exact hp'⟩
  | sendB t ht hsrc hdir =>
    rcases hi with ⟨hqa, hp⟩ | ⟨hqb, hp⟩
    · cases hq : c.qb with
      | nil =>
        rw [hq] at hp
        have := path_nil_eq C hp
        refine Or.inr ⟨rfl, ?_⟩
        simp only [hqa, List.nil_append]
        have h := Path.cons t ht hdir (Path.nil (C := C) (d := .ba) t.dst)
        rw [hsrc, this] at h
        exact h
      | cons l w =>
        rw [hq] at hp
        obtain ⟨t₀, ht₀, hs₀, hd₀, _, _⟩ := path_head C hp
        have := hm t ht t₀ ht₀ (by rw [hsrc, hs₀])
        rw [hdir, hd₀] at this
        cases this
    · exact Or.inr ⟨hqb, path_snoc C hp ht hsrc hdir⟩
  | recvB t r hq ht hsrc hdir =>
    rcases hi with ⟨hqa, hp⟩ | ⟨hqb, _⟩
    · rw [hq] at hp
      obtain ⟨t₀, ht₀, hs₀, _, hl₀, hp'⟩ := path_head C hp
      have := hdet t ht t₀ ht₀ (by rw [hsrc, hs₀]) hl₀.symm
      exact Or.inl ⟨hqa, by rw [this]; exact hp'⟩
    · rw [hqb] at hq; cases hq

theorem reachable_inv (hm : NoMixed C) (hdet : Det C) {c : Cfg}
    (h : Reachable C c) : Inv C c := by
  induction h with
  | init => exact inv_init C
  | step _ hs ih => exact inv_step C hm hdet ih hs

/-! ## Propiedades -/

section
variable {C}
variable (hm : NoMixed C) (hdet : Det C)
include hm hdet

/-- Ningún mensaje inesperado (servidor): el primero de su cola tiene una
transición de recepción en su estado. -/
theorem head_receivable_b {c : Cfg} (h : Reachable C c) {l : Nat} {r : List Nat}
    (hq : c.qb = l :: r) :
    ∃ t ∈ C.trans, t.src = c.b ∧ t.dir = .ab ∧ t.label = l := by
  rcases reachable_inv C hm hdet h with ⟨_, hp⟩ | ⟨hqb, _⟩
  · rw [hq] at hp
    obtain ⟨t, ht, hs, hd, hl, _⟩ := path_head C hp
    exact ⟨t, ht, hs, hd, hl⟩
  · rw [hqb] at hq; cases hq

/-- Ningún mensaje inesperado (cliente). -/
theorem head_receivable_a {c : Cfg} (h : Reachable C c) {l : Nat} {r : List Nat}
    (hq : c.qa = l :: r) :
    ∃ t ∈ C.trans, t.src = c.a ∧ t.dir = .ba ∧ t.label = l := by
  rcases reachable_inv C hm hdet h with ⟨hqa, _⟩ | ⟨_, hp⟩
  · rw [hqa] at hq; cases hq
  · rw [hq] at hp
    obtain ⟨t, ht, hs, hd, hl, _⟩ := path_head C hp
    exact ⟨t, ht, hs, hd, hl⟩

/-- Sin bloqueo de protocolo: o alguna punta puede dar un paso, o las dos
están en el mismo estado final sin nada en vuelo. -/
theorem progress {c : Cfg} (h : Reachable C c) :
    (∃ d, Step C c d) ∨ (c.qa = [] ∧ c.qb = [] ∧ c.a = c.b ∧ Terminal C c.a) := by
  -- si hay un mensaje en una cola, su destinatario lo puede recibir
  cases hqb : c.qb with
  | cons l r =>
    obtain ⟨t, ht, hs, hd, hl⟩ := head_receivable_b hm hdet h hqb
    exact Or.inl ⟨_, Step.recvB c t r (by rw [hqb, hl]) ht hs hd⟩
  | nil =>
    cases hqa : c.qa with
    | cons l r =>
      obtain ⟨t, ht, hs, hd, hl⟩ := head_receivable_a hm hdet h hqa
      exact Or.inl ⟨_, Step.recvA c t r (by rw [hqa, hl]) ht hs hd⟩
    | nil =>
      -- colas vacías: las dos puntas están en el mismo estado
      have hab : c.a = c.b := by
        rcases reachable_inv C hm hdet h with ⟨_, hp⟩ | ⟨_, hp⟩
        · rw [hqb] at hp; exact (path_nil_eq C hp).symm
        · rw [hqa] at hp; exact path_nil_eq C hp
      by_cases hex : ∃ t ∈ C.trans, t.src = c.a
      · obtain ⟨t, ht, hs⟩ := hex
        cases hd : t.dir with
        | ab => exact Or.inl ⟨_, Step.sendA c t ht hs hd⟩
        | ba => exact Or.inl ⟨_, Step.sendB c t ht (hs.trans hab) hd⟩
      · refine Or.inr ⟨rfl, rfl, hab, ?_⟩
        intro t ht hs
        exact hex ⟨t, ht, hs⟩

/-- Una punta que terminó (estado final) no tiene mensajes sin leer. -/
theorem finished_no_pending_a {c : Cfg} (h : Reachable C c) (ht : Terminal C c.a) :
    c.qa = [] := by
  rcases reachable_inv C hm hdet h with ⟨hqa, _⟩ | ⟨_, hp⟩
  · exact hqa
  · cases hq : c.qa with
    | nil => rfl
    | cons l w =>
      rw [hq] at hp
      obtain ⟨t, ht', hs, _⟩ := path_head C hp
      exact absurd hs (ht t ht')

theorem finished_no_pending_b {c : Cfg} (h : Reachable C c) (ht : Terminal C c.b) :
    c.qb = [] := by
  rcases reachable_inv C hm hdet h with ⟨_, hp⟩ | ⟨hqb, _⟩
  · cases hq : c.qb with
    | nil => rfl
    | cons l w =>
      rw [hq] at hp
      obtain ⟨t, ht', hs, _⟩ := path_head C hp
      exact absurd hs (ht t ht')
  · exact hqb

/-- Colas acotadas: nunca hay más de `k` mensajes en vuelo. -/
theorem queue_bound {k : Nat} (hb : Bounded C k) {c : Cfg} (h : Reachable C c) :
    c.qa.length + c.qb.length ≤ k := by
  rcases reachable_inv C hm hdet h with ⟨hqa, hp⟩ | ⟨hqb, hp⟩
  · have := hb _ _ _ _ hp; rw [hqa]; simpa using this
  · have := hb _ _ _ _ hp; rw [hqb]; simpa using this

end

/-- Teorema principal: todo contrato que acepta `check` es seguro. -/
theorem contract_safe {k : Nat} (hc : check C = some k) {c : Cfg} (h : Reachable C c) :
    (∀ l r, c.qa = l :: r → ∃ t ∈ C.trans, t.src = c.a ∧ t.dir = .ba ∧ t.label = l) ∧
    (∀ l r, c.qb = l :: r → ∃ t ∈ C.trans, t.src = c.b ∧ t.dir = .ab ∧ t.label = l) ∧
    ((∃ d, Step C c d) ∨ (c.qa = [] ∧ c.qb = [] ∧ c.a = c.b ∧ Terminal C c.a)) ∧
    (Terminal C c.a → c.qa = []) ∧ (Terminal C c.b → c.qb = []) ∧
    c.qa.length + c.qb.length ≤ k := by
  obtain ⟨hm, hdet, hb⟩ := check_sound C hc
  exact ⟨fun _ _ hq => head_receivable_a hm hdet h hq,
    fun _ _ hq => head_receivable_b hm hdet h hq,
    progress hm hdet h,
    finished_no_pending_a hm hdet h, finished_no_pending_b hm hdet h,
    queue_bound hm hdet hb h⟩

/-! ## Cada condición del control hace falta -/

/-- Un estado mixto: en el estado 0 pueden mandar las dos puntas. Si lo
hacen a la vez, el cliente recibe un `2` en el estado 1, que no lo espera. -/
def mixed : Contract := ⟨0, [⟨0, .ab, 1, 1⟩, ⟨0, .ba, 2, 2⟩]⟩

theorem mixed_breaks :
    check mixed = none ∧
    ∃ c, Reachable mixed c ∧ c.qa = [2] ∧
      ¬ ∃ t ∈ mixed.trans, t.src = c.a ∧ t.dir = .ba ∧ t.label = 2 := by
  refine ⟨by decide, ⟨1, 2, [2], [1]⟩, ?_, rfl, by decide⟩
  have h1 : Reachable mixed ⟨1, 0, [], [1]⟩ :=
    Reachable.step Reachable.init
      (Step.sendA (init mixed) ⟨0, .ab, 1, 1⟩ (by decide) rfl rfl)
  exact Reachable.step h1 (Step.sendB _ ⟨0, .ba, 2, 2⟩ (by decide) rfl rfl)

/-- No determinista: con la etiqueta `1` el cliente puede ir al estado 2 y
el servidor, al recibirla, al 1. Quedan desfasados y el cliente recibe un
`5` en un estado en el que le toca mandar. -/
def nondet : Contract :=
  ⟨0, [⟨0, .ab, 1, 1⟩, ⟨0, .ab, 1, 2⟩, ⟨1, .ba, 5, 0⟩, ⟨2, .ab, 7, 0⟩]⟩

theorem nondet_breaks :
    check nondet = none ∧
    ∃ c, Reachable nondet c ∧ c.qa = [5] ∧
      ¬ ∃ t ∈ nondet.trans, t.src = c.a ∧ t.dir = .ba ∧ t.label = 5 := by
  refine ⟨by decide, ⟨2, 0, [5], []⟩, ?_, rfl, by decide⟩
  have h1 : Reachable nondet ⟨2, 0, [], [1]⟩ :=
    Reachable.step Reachable.init
      (Step.sendA (init nondet) ⟨0, .ab, 1, 2⟩ (by decide) rfl rfl)
  have h2 : Reachable nondet ⟨2, 1, [], []⟩ :=
    Reachable.step h1 (Step.recvB _ ⟨0, .ab, 1, 1⟩ [] rfl (by decide) rfl rfl)
  exact Reachable.step h2 (Step.sendB _ ⟨1, .ba, 5, 0⟩ (by decide) rfl rfl)

/-- Sin cota: el cliente puede mandar `1` para siempre sin que el servidor
lea, y la cola crece sin límite (en el kernel, se llena). -/
def flood : Contract := ⟨0, [⟨0, .ab, 1, 0⟩]⟩

theorem loop_unbounded :
    check flood = none ∧ ∀ n, Reachable flood ⟨0, 0, [], List.replicate n 1⟩ := by
  refine ⟨by decide, ?_⟩
  intro n
  induction n with
  | zero => exact Reachable.init
  | succ n ih =>
    have := Reachable.step ih
      (Step.sendA _ ⟨0, .ab, 1, 0⟩ (by decide) rfl rfl)
    simpa [List.replicate_succ'] using this

/-! ## Los contratos de `nsos_contract::protocols`

Los estados se numeran como los numera la macro: primero los estados en que
manda el cliente, después los del servidor y al final los estados finales,
cada grupo en el orden en que se declaró. `tracegen` exporta estas tablas y
`crates/contract/tests/contract_model.rs` exige que sean las de la macro. -/

/-- `echo` (ping ↔ pong). Estados: `Ready` 0, `Waiting` 1, `Adopting` 2,
`End` 3. Etiquetas: `Echo` 1, `EchoReply` 2, `NewChannel` 100, `Adopted` 101,
`Bye` 999. -/
def echo : Contract :=
  ⟨0, [⟨0, .ab, 1, 1⟩, ⟨0, .ab, 100, 2⟩, ⟨0, .ab, 999, 3⟩,
       ⟨1, .ba, 2, 0⟩, ⟨2, .ba, 101, 0⟩]⟩

/-- `note` (el canal que `ping` le pasa a `pong`). Estados: `Start` 0,
`Waiting` 1, `End` 2. Etiquetas: `Note` 0, `Ack` 2. -/
def note : Contract := ⟨0, [⟨0, .ab, 0, 1⟩, ⟨1, .ba, 2, 2⟩]⟩

theorem echo_check : check echo = some 1 := by decide
theorem note_check : check note = some 1 := by decide

/-- Con la tabla de antes (`NEW_CHANNEL` sin respuesta, de vuelta a `Ready`)
el control la rechaza: `ping` podía llenar la cola de `pong`. -/
def echoOld : Contract :=
  ⟨0, [⟨0, .ab, 1, 1⟩, ⟨0, .ab, 100, 0⟩, ⟨0, .ab, 999, 2⟩, ⟨1, .ba, 2, 0⟩]⟩

theorem echoOld_unbounded : check echoOld = none := by decide

/-! ### Actualización en vivo (etapa 2)

Etiquetas de `nsos_abi::msg::labels`. -/

def systemBase : Nat := 0xFFFF000000000000
def lPrepare : Nat := systemBase + 1
def lState : Nat := systemBase + 2
def lRestore : Nat := systemBase + 3
def lAccept : Nat := systemBase + 6
def lReject : Nat := systemBase + 7
def lCommit : Nat := systemBase + 8
def lRollback : Nat := systemBase + 9

/-- `counter` (`cliente` ↔ contador). Estados: `Serving` 0, `Waiting` 1.
Etiquetas: `Inc` 1, `Count` 2. -/
def counter : Contract := ⟨0, [⟨0, .ab, 1, 1⟩, ⟨1, .ba, 2, 0⟩]⟩

/-- `handover` (supervisor ↔ versión vieja). Estados: `Serving` 0,
`Deciding` 1 (manda el supervisor), `Handing` 2 (manda la versión vieja),
`End` 3. -/
def handover : Contract :=
  ⟨0, [⟨0, .ab, lPrepare, 2⟩, ⟨1, .ab, lCommit, 3⟩, ⟨1, .ab, lRollback, 0⟩,
       ⟨2, .ba, lState, 1⟩]⟩

/-- `restore` (supervisor ↔ versión nueva). Estados: `Start` 0, `Deciding` 1,
`End` 2. Tras un rechazo vuelve a `Start`: `origen` y `destino` usan el
mismo canal con el enlace para varios intentos. -/
def restore : Contract :=
  ⟨0, [⟨0, .ab, lRestore, 1⟩, ⟨1, .ba, lAccept, 2⟩, ⟨1, .ba, lReject, 0⟩]⟩

theorem counter_check : check counter = some 1 := by decide
/-- Cota 2: el supervisor puede mandar `Rollback` y enseguida otro `Prepare`. -/
theorem handover_check : check handover = some 2 := by decide
theorem restore_check : check restore = some 1 := by decide

end Nsos.Contract
