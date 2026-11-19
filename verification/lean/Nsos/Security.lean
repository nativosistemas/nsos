import Nsos.System

/-!
# Propiedades de seguridad del sistema de capacidades

Teoremas principales:

* `Sys.step_other_cells`: una syscall de la cell `a` no cambia la CSpace de
  ninguna otra cell.
* `Sys.step_no_amplification`: después de cualquier syscall, toda capacidad
  que exista (en una CSpace o en una cola) viene de una que ya existía, con
  derechos iguales o menores, o apunta a un objeto recién creado.
* `Sys.send_requires_grant`: lo que se envía por un canal sale de la CSpace
  del emisor, y cada capacidad enviada tenía `GRANT`. Viaja con los mismos
  derechos.
* `Sys.no_forging`: en cualquier ejecución, una cell que nunca recibe
  mensajes termina solo con capacidades a objetos que ya tenía (con derechos
  iguales o menores) o a objetos nuevos. Es decir: nadie puede fabricar una
  capacidad, y nadie la recibe sin un `recv` propio.
* `Sys.step_no_leak`: después de cada syscall, todo extremo que ya nadie puede
  alcanzar está cerrado y con la cola vacía (no hay fugas por ciclos).
-/

namespace Nsos
namespace Sys

/-- La cell `a` tiene la capacidad `c`. -/
def Held (s : Sys) (a : Nat) (c : Cap Obj) : Prop :=
  ∃ cs, s.cells[a]? = some cs ∧ c ∈ cs.caps

/-- La capacidad `c` viaja en algún mensaje en cola. -/
def Queued (s : Sys) (c : Cap Obj) : Prop :=
  ∃ o m, m ∈ s.queue o ∧ c ∈ m.caps

/-- La capacidad `c` existe en algún lado. -/
def Exists (s : Sys) (c : Cap Obj) : Prop := (∃ a, s.Held a c) ∨ s.Queued c

/-- `c` se puede explicar a partir de `s`: hay una capacidad al mismo objeto
con derechos iguales o mayores, o el objeto es nuevo. -/
def Derived (s : Sys) (c : Cap Obj) : Prop :=
  (∃ c₀, s.Exists c₀ ∧ c₀.obj = c.obj ∧ c.rights ⊆ c₀.rights) ∨ s.nextPair ≤ c.obj.pair

theorem derived_of_exists {s : Sys} {c : Cap Obj} (h : s.Exists c) : s.Derived c :=
  .inl ⟨c, h, rfl, Rights.subset_refl _⟩

/-! ## La recolección solo quita -/

theorem gc_cells (s : Sys) : s.gc.cells = s.cells := rfl
theorem gc_nextPair (s : Sys) : s.gc.nextPair = s.nextPair := rfl
theorem gc_queued {s : Sys} {c : Cap Obj} (h : s.gc.Queued c) : s.Queued c := by
  obtain ⟨o, m, hm, hc⟩ := h
  simp only [gc] at hm
  split at hm
  · simp at hm
  · exact ⟨o, m, hm, hc⟩

theorem gc_held {s : Sys} {a : Nat} {c : Cap Obj} : s.gc.Held a c ↔ s.Held a c := by
  simp [Held, gc_cells]

/-! ## Lemas sobre `cellAt` y `setCell` -/

theorem cellAt_ok {s : Sys} {a : Nat} {cs : CSpace Obj} (h : s.cellAt a = .ok cs) :
    s.cells[a]? = some cs := by
  unfold cellAt at h
  split at h <;> simp_all

theorem setCell_other {s : Sys} {a b : Nat} {cs : CSpace Obj} (hne : b ≠ a) :
    (s.setCell a cs).cells[b]? = s.cells[b]? := by
  simp [setCell, Ne.symm hne]

theorem held_setCell {s : Sys} {a b : Nat} {cs : CSpace Obj} {c : Cap Obj}
    (h : (s.setCell a cs).Held b c) : (b = a ∧ c ∈ cs.caps) ∨ (b ≠ a ∧ s.Held b c) := by
  obtain ⟨cs', hcs, hc⟩ := h
  by_cases hb : b = a
  · subst hb
    simp only [setCell, List.getElem?_set_self'] at hcs
    left; refine ⟨rfl, ?_⟩
    cases hlen : s.cells[b]? with
    | none => simp [hlen] at hcs
    | some _ => simp [hlen] at hcs; subst hcs; exact hc
  · right; refine ⟨hb, cs', ?_, hc⟩
    rw [setCell_other hb] at hcs; exact hcs

/-! ## Especificación de cada syscall -/

/-- Lo que garantiza una syscall exitosa de la cell `a`: solo cambia su
propia CSpace; cada capacidad nueva en ella es derivada de una propia, o es de
un objeto nuevo, o (solo en `recv`) estaba en una cola; y las colas solo
contienen capacidades que ya existían. -/
structure Spec (s s' : Sys) (a : Nat) (isRecv : Bool) : Prop where
  frame : ∃ cs cs', s.cells[a]? = some cs ∧ s'.cells = s.cells.set a cs' ∧
    ∀ c, c ∈ cs'.caps →
      (∃ c₀, c₀ ∈ cs.caps ∧ c₀.obj = c.obj ∧ c.rights ⊆ c₀.rights) ∨
      (s.nextPair ≤ c.obj.pair ∧ c.obj.pair < s'.nextPair) ∨ (isRecv = true ∧ s.Queued c)
  next : s.nextPair ≤ s'.nextPair
  queues : ∀ c, s'.Queued c → s.Exists c

theorem same_cap {cs : CSpace Obj} {c : Cap Obj} (h : c ∈ cs.caps) :
    ∃ c₀, c₀ ∈ cs.caps ∧ c₀.obj = c.obj ∧ c.rights ⊆ c₀.rights :=
  ⟨c, h, rfl, Rights.subset_refl _⟩

theorem exists_of_held {s : Sys} {a : Nat} {cs : CSpace Obj} {c : Cap Obj}
    (hcs : s.cells[a]? = some cs) (h : c ∈ cs.caps) : s.Exists c :=
  .inl ⟨a, cs, hcs, h⟩

theorem spec_dup {s s' : Sys} {a h mask : Nat} {r : List Nat}
    (he : s.exec' a (.dup h mask) = .ok (s', r)) : Spec s s' a false := by
  simp only [exec'] at he
  split at he
  · cases he
  split at he
  · cases he
  rename_i cs hcs
  split at he
  · cases he
  rename_i cs' h' hd
  cases he
  refine ⟨⟨cs, cs', cellAt_ok hcs, rfl, ?_⟩, Nat.le_refl _, ?_⟩
  · intro c hc
    rcases CSpace.dup_caps hd c hc with h1 | h1
    · exact .inl (same_cap h1)
    · obtain ⟨c₀, h0, ho, hr⟩ := h1
      exact .inl ⟨c₀, h0, ho.symm, hr⟩
  · intro c hq; exact .inr hq

theorem spec_drop {s s' : Sys} {a h : Nat} {r : List Nat}
    (he : s.exec' a (.drop h) = .ok (s', r)) : Spec s s' a false := by
  simp only [exec'] at he
  split at he
  · cases he
  rename_i cs hcs
  split at he
  · cases he
  rename_i cs' c' hrm
  cases he
  refine ⟨⟨cs, cs', cellAt_ok hcs, rfl, ?_⟩, Nat.le_refl _, ?_⟩
  · intro c hc
    exact .inl (same_cap ((CSpace.remove_caps hrm).2.2 c hc))
  · intro c hq; exact .inr hq

theorem spec_create {s s' : Sys} {a : Nat} {r : List Nat}
    (he : s.exec' a .create = .ok (s', r)) : Spec s s' a false := by
  simp only [exec'] at he
  split at he
  · cases he
  rename_i cs hcs
  split at he
  · cases he
  rename_i cs' hs hins
  cases he
  refine ⟨⟨cs, cs', cellAt_ok hcs, rfl, ?_⟩, Nat.le_succ _, ?_⟩
  · intro c hc
    rcases CSpace.insertAll_caps hins c hc with h1 | h1
    · exact .inl (same_cap h1)
    · right; left
      simp only [List.mem_cons, List.not_mem_nil, or_false] at h1
      rcases h1 with rfl | rfl <;> simp [Obj.pair]
  · intro c hq; exact .inr hq

theorem spec_send {s s' : Sys} {a h l : Nat} {hs : List Nat} {r : List Nat}
    (he : s.exec' a (.send h l hs) = .ok (s', r)) : Spec s s' a false := by
  simp only [exec'] at he
  split at he
  · cases he
  split at he
  · cases he
  rename_i cs hcs
  split at he
  · cases he
  rename_i c hg
  split at he
  · cases he
  split at he
  · cases he
  split at he
  · cases he
  rename_i cs' moved hrm
  cases he
  have hcs' := cellAt_ok hcs
  obtain ⟨hsub, hmoved⟩ := CSpace.removeAll_caps hrm
  refine ⟨⟨cs, cs', hcs', rfl, ?_⟩, Nat.le_refl _, ?_⟩
  · intro x hx; exact .inl (same_cap (hsub x hx))
  · intro x ⟨o, m, hm, hx⟩
    rcases mem_update hm with hm | hm
    · simp only [List.mem_append, List.mem_cons, List.not_mem_nil, or_false] at hm
      rcases hm with hm | rfl
      · exact .inr ⟨_, m, hm, hx⟩
      · exact exists_of_held hcs' (hmoved x hx)
    · exact .inr ⟨o, m, hm, hx⟩

theorem spec_recv {s s' : Sys} {a h : Nat} {r : List Nat}
    (he : s.exec' a (.recv h) = .ok (s', r)) : Spec s s' a true := by
  simp only [exec'] at he
  split at he
  · cases he
  rename_i cs hcs
  split at he
  · cases he
  rename_i c hg
  split at he
  · rename_i m rest hq
    split at he
    · cases he
    rename_i cs' hs hins
    cases he
    refine ⟨⟨cs, cs', cellAt_ok hcs, rfl, ?_⟩, Nat.le_refl _, ?_⟩
    · intro x hx
      rcases CSpace.insertAll_caps hins x hx with h1 | h1
      · exact .inl (same_cap h1)
      · right; right
        exact ⟨rfl, c.obj, m, by simp [hq], h1⟩
    · intro x ⟨o, m', hm, hx⟩
      rcases mem_update hm with hm | hm
      · exact .inr ⟨c.obj, m', by simp [hq, hm], hx⟩
      · exact .inr ⟨o, m', hm, hx⟩
  · split at he <;> cases he

theorem cells_set_self {l : List (CSpace Obj)} {a : Nat} {cs : CSpace Obj}
    (h : l[a]? = some cs) : l.set a cs = l := by
  apply List.ext_getElem?
  intro i
  rw [List.getElem?_set]
  split
  · rename_i hi; subst hi
    obtain ⟨hlt, hget⟩ := List.getElem?_eq_some_iff.1 h
    simp [hlt, hget]
  · rfl

/-- Las esperas no tocan CSpaces ni colas. -/
theorem spec_wait {s s' : Sys} {a : Nat} {op : Op} {r : List Nat}
    (hw : op = .waitRecv h ∨ op = .waitSend h)
    (he : s.exec' a op = .ok (s', r)) : Spec s s' a false := by
  rcases hw with rfl | rfl <;> simp only [exec'] at he <;>
  · split at he
    · cases he
    rename_i cs hcs
    split at he
    · cases he
    split at he
    · cases he
    cases he
    refine ⟨⟨cs, cs, cellAt_ok hcs, (cells_set_self (cellAt_ok hcs)).symm, ?_⟩,
      Nat.le_refl _, ?_⟩
    · intro c hc; exact .inl (same_cap hc)
    · intro c hq; exact .inr hq

theorem exec_spec {s s' : Sys} {a : Nat} {op : Op} {r : List Nat}
    (he : s.exec a op = .ok (s', r)) : Spec s s' a op.isRecv := by
  unfold exec at he
  split at he
  · cases he
  cases op with
  | dup => exact spec_dup he
  | drop => exact spec_drop he
  | create => exact spec_create he
  | send => exact spec_send he
  | recv => exact spec_recv he
  | waitRecv h => exact spec_wait (.inl rfl) he
  | waitSend h => exact spec_wait (.inr rfl) he

theorem step_spec {s s' : Sys} {a : Nat} {op : Op} {r : List Nat}
    (hs : s.step a op = .ok (s', r)) : Spec s s' a op.isRecv := by
  unfold step at hs
  split at hs
  · cases hs
  rename_i s₁ r₁ he
  cases hs
  obtain ⟨⟨cs, cs', h1, h2, h3⟩, h4, h5⟩ := exec_spec he
  exact ⟨⟨cs, cs', h1, by rw [gc_cells, h2], h3⟩, by rw [gc_nextPair]; exact h4,
    fun c hq => h5 c (gc_queued hq)⟩

/-! ## Teoremas -/

/-- **Aislamiento**: una syscall de la cell `a` no toca la CSpace de otra cell. -/
theorem step_other_cells {s s' : Sys} {a b : Nat} {op : Op} {r : List Nat}
    (hs : s.step a op = .ok (s', r)) (hne : b ≠ a) : s'.cells[b]? = s.cells[b]? := by
  obtain ⟨⟨cs, cs', _, h2, _⟩, _, _⟩ := step_spec hs
  rw [h2, List.getElem?_set_ne (Ne.symm hne)]

theorem step_nextPair_le {s s' : Sys} {a : Nat} {op : Op} {r : List Nat}
    (hs : s.step a op = .ok (s', r)) : s.nextPair ≤ s'.nextPair :=
  (step_spec hs).next

/-- Qué puede tener la cell `b` después de una syscall de `a`. -/
theorem step_held {s s' : Sys} {a b : Nat} {op : Op} {r : List Nat}
    (hs : s.step a op = .ok (s', r)) {c : Cap Obj} (hc : s'.Held b c) :
    (∃ c₀, s.Held b c₀ ∧ c₀.obj = c.obj ∧ c.rights ⊆ c₀.rights) ∨
    s.nextPair ≤ c.obj.pair ∨ (b = a ∧ op.isRecv = true ∧ s.Queued c) := by
  obtain ⟨⟨cs, cs', h1, h2, h3⟩, _, _⟩ := step_spec hs
  obtain ⟨csb, hb, hcb⟩ := hc
  by_cases hab : b = a
  · subst hab
    rw [h2, List.getElem?_set_self', h1] at hb
    cases hb
    rcases h3 c hcb with ⟨c₀, h0, ho, hr⟩ | h | ⟨hr, hq⟩
    · exact .inl ⟨c₀, ⟨cs, h1, h0⟩, ho, hr⟩
    · exact .inr (.inl h.1)
    · exact .inr (.inr ⟨rfl, hr, hq⟩)
  · rw [h2, List.getElem?_set_ne (Ne.symm hab)] at hb
    exact .inl ⟨c, ⟨csb, hb, hcb⟩, rfl, Rights.subset_refl _⟩

/-- **Sin amplificación**: toda capacidad que existe después de una syscall
se explica por una que ya existía (mismo objeto, derechos iguales o
mayores) o apunta a un objeto recién creado. -/
theorem step_no_amplification {s s' : Sys} {a : Nat} {op : Op} {r : List Nat}
    (hs : s.step a op = .ok (s', r)) {c : Cap Obj} (hc : s'.Exists c) : s.Derived c := by
  rcases hc with ⟨b, hb⟩ | hq
  · rcases step_held hs hb with ⟨c₀, h0, ho, hr⟩ | h | ⟨_, _, hq⟩
    · exact .inl ⟨c₀, .inl ⟨b, h0⟩, ho, hr⟩
    · exact .inr h
    · exact derived_of_exists (.inr hq)
  · exact derived_of_exists ((step_spec hs).queues c hq)

/-- **Enviar exige `GRANT` y conserva derechos**: un `send` exitoso agrega
exactamente un mensaje al final de la cola del otro extremo, y cada capacidad
de ese mensaje estaba en la CSpace del emisor con el derecho `GRANT`. Las
capacidades viajan tal cual: no ganan derechos en el camino. -/
theorem send_requires_grant {s s' : Sys} {a h l : Nat} {hs r : List Nat}
    (he : s.exec' a (.send h l hs) = .ok (s', r)) :
    ∃ cs c msg, s.cells[a]? = some cs ∧ cs.get h = .ok c ∧ c.rights.has .send = true ∧
      s'.queue c.obj.peer = s.queue c.obj.peer ++ [msg] ∧ msg.label = l ∧
      ∀ x, x ∈ msg.caps → x ∈ cs.caps ∧ x.rights.has .grant = true := by
  simp only [exec'] at he
  split at he
  · cases he
  split at he
  · cases he
  rename_i cs hcs
  split at he
  · cases he
  rename_i c hg
  split at he
  · cases he
  split at he
  · cases he
  split at he
  · cases he
  rename_i cs' moved hrm
  cases he
  obtain ⟨hget, hneed⟩ := CSpace.getWith_get hg
  refine ⟨cs, c, ⟨s.nextMsg, l, moved⟩, cellAt_ok hcs, hget, hneed .send (by decide), ?_, rfl, ?_⟩
  · simp [update]
  · intro x hx
    obtain ⟨h1, h2⟩ := CSpace.removeAll_rights hrm x hx
    exact ⟨h1, h2 .grant (by decide)⟩

/-! ## Ejecuciones -/

/-- Una ejecución: secuencia de syscalls `(cell, op)`. Las que fallan no
cambian el estado. -/
inductive Run : Sys → List (Nat × Op) → Sys → Prop
  | nil (s : Sys) : Run s [] s
  | ok {s s' s'' : Sys} {a : Nat} {op : Op} {r : List Nat} {rest : List (Nat × Op)} :
      s.step a op = .ok (s', r) → Run s' rest s'' → Run s ((a, op) :: rest) s''
  | err {s s'' : Sys} {a : Nat} {op : Op} {e : Err} {rest : List (Nat × Op)} :
      s.step a op = .error e → Run s rest s'' → Run s ((a, op) :: rest) s''

theorem run_nextPair_le {s s' : Sys} {tr : List (Nat × Op)} (h : Run s tr s') :
    s.nextPair ≤ s'.nextPair := by
  induction h with
  | nil => exact Nat.le_refl _
  | ok hs _ ih => exact Nat.le_trans (step_nextPair_le hs) ih
  | err _ _ ih => exact ih

/-- **No falsificación**: si la cell `b` nunca hace `recv` en una ejecución,
al final solo tiene capacidades a objetos que ya tenía al principio (con
derechos iguales o menores) o a objetos creados durante la ejecución. En
particular, no puede conseguir ningún objeto preexistente que nadie le pasó. -/
theorem no_forging {s s' : Sys} {tr : List (Nat × Op)} (h : Run s tr s') (b : Nat)
    (hnr : ∀ op, (b, op) ∈ tr → op.isRecv = false) {c : Cap Obj} (hc : s'.Held b c) :
    (∃ c₀, s.Held b c₀ ∧ c₀.obj = c.obj ∧ c.rights ⊆ c₀.rights) ∨ s.nextPair ≤ c.obj.pair := by
  induction h generalizing c with
  | nil => exact .inl ⟨c, hc, rfl, Rights.subset_refl _⟩
  | @ok s₀ s₁ s₂ a op r rest hs hrun ih =>
    have hnr' : ∀ op, (b, op) ∈ rest → op.isRecv = false :=
      fun op hm => hnr op (List.mem_cons_of_mem _ hm)
    rcases ih hnr' hc with ⟨c₁, h1, ho1, hr1⟩ | hfresh
    · rcases step_held hs h1 with ⟨c₀, h0, ho0, hr0⟩ | hf | ⟨hba, hrecv, _⟩
      · exact .inl ⟨c₀, h0, ho0.trans ho1, Rights.subset_trans hr1 hr0⟩
      · exact .inr (ho1 ▸ hf)
      · subst hba
        have := hnr op (List.mem_cons_self ..)
        simp [hrecv] at this
    · exact .inr (Nat.le_trans (step_nextPair_le hs) hfresh)
  | err _ _ ih =>
    exact ih (fun op hm => hnr op (List.mem_cons_of_mem _ hm)) hc

/-- **Sin amplificación en ejecuciones**: toda capacidad al final de una
ejecución se explica por una del estado inicial o es de un objeto nuevo. -/
theorem run_no_amplification {s s' : Sys} {tr : List (Nat × Op)} (h : Run s tr s')
    {c : Cap Obj} (hc : s'.Exists c) : s.Derived c := by
  induction h generalizing c with
  | nil => exact derived_of_exists hc
  | ok hs _ ih =>
    rcases ih hc with ⟨c₁, h1, ho1, hr1⟩ | hfresh
    · rcases step_no_amplification hs h1 with ⟨c₀, h0, ho0, hr0⟩ | hf
      · exact .inl ⟨c₀, h0, ho0.trans ho1, Rights.subset_trans hr1 hr0⟩
      · exact .inr (ho1 ▸ hf)
    · exact .inr (Nat.le_trans (step_nextPair_le hs) hfresh)
  | err _ _ ih => exact ih hc

/-! ## Sin fugas de canales -/

/-- `e` es alcanzable: alguna cell lo tiene, o viaja en la cola de un extremo
alcanzable (y entonces el dueño de ese extremo lo puede recibir). -/
inductive Reach (s : Sys) : Obj → Prop
  | held {cs : CSpace Obj} {c : Cap Obj} : cs ∈ s.cells → c ∈ cs.caps → Reach s c.obj
  | queued {x : Obj} {m : Msg} {c : Cap Obj} :
      Reach s x → m ∈ s.queue x → c ∈ m.caps → Reach s c.obj

theorem frontier_reach (s : Sys) {acc : List Obj} (h : ∀ x, x ∈ acc → s.Reach x)
    {y : Obj} (hy : y ∈ s.frontier acc) : s.Reach y := by
  simp only [frontier, List.mem_append, roots, queuedObjs, List.mem_flatMap,
    List.mem_map] at hy
  rcases hy with ⟨cs, hcs, c, hc, rfl⟩ | ⟨z, hz, m, hm, c, hc, rfl⟩
  · exact .held hcs hc
  · exact .queued (h z hz) hm hc

theorem reachGo_sound (s : Sys) : ∀ (n : Nat) (acc : List Obj),
    (∀ x, x ∈ acc → s.Reach x) → ∀ x, x ∈ s.reachGo n acc → s.Reach x := by
  intro n
  induction n with
  | zero => intro acc h x hx; exact h x hx
  | succ n ih =>
    intro acc h x hx
    simp only [reachGo] at hx
    split at hx
    · exact h x hx
    · rename_i y hf
      apply ih _ _ x hx
      intro z hz
      simp only [List.mem_cons] at hz
      rcases hz with rfl | hz
      · exact frontier_reach s h (List.mem_of_find?_eq_some hf)
      · exact h z hz

theorem reachSet_sound (s : Sys) {x : Obj} (hx : x ∈ s.reachSet) : s.Reach x :=
  reachGo_sound s _ _ (fun _ h => by simp at h) x hx

/-- **La recolección no deja fugas**: todo extremo que ya nadie puede alcanzar
queda cerrado y con la cola vacía (sus mensajes, y las capacidades que
llevaban, se liberan). -/
theorem gc_collects {s : Sys} {e : Obj} (hp : e.pair < s.nextPair) (h : ¬ s.Reach e) :
    s.gc.closed e = true ∧ s.gc.queue e = [] := by
  have hn : e ∉ s.reachSet := fun hm => h (reachSet_sound s hm)
  simp [gc, dead, hn, hp]

/-- **Sin fugas tras cada syscall**: si después del efecto de una syscall un
extremo (que existe) quedó inalcanzable (por ejemplo, porque viaja solo en su propia cola,
o en un ciclo de colas), al terminar la syscall está cerrado y su cola vacía. -/
theorem step_no_leak {s s₁ s' : Sys} {a : Nat} {op : Op} {r : List Nat} {e : Obj}
    (he : s.exec a op = .ok (s₁, r)) (hs : s.step a op = .ok (s', r))
    (hp : e.pair < s₁.nextPair) (hu : ¬ s₁.Reach e) :
    s'.closed e = true ∧ s'.queue e = [] := by
  unfold step at hs
  rw [he] at hs
  cases hs
  exact gc_collects hp hu

end Sys
end Nsos
