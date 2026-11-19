import Nsos.Rights
import Nsos.Handle

/-!
# Tabla de capacidades (CSpace)

Modelo ejecutable de `nsos_kcore::cspace::CSpace<T>`. Refleja la
implementación paso a paso (incluido el orden de la lista de slots libres),
para que los handles que devuelve sean *exactamente* los mismos que en Rust.
Eso es lo que comparan los tests derivados del modelo.
-/

namespace Nsos

/-- Una capacidad: objeto + derechos. -/
structure Cap (α : Type) where
  obj : α
  rights : Rights

/-- Un slot: generación actual y, si está ocupado, la capacidad. -/
structure Slot (α : Type) where
  gen : Nat
  cap : Option (Cap α)

/-- Tabla de capacidades. `slots[0]` existe pero nunca se usa. `free` es una
pila: la cabeza es el último slot liberado (como `Vec::pop` en Rust). Un slot
cuya generación llega a `GEN_MOD` queda retirado: nunca vuelve a `free`. -/
structure CSpace (α : Type) where
  slots : List (Slot α)
  free : List Nat
  limit : Nat
  slotLimit : Nat
  live : Nat

namespace CSpace
variable {α : Type}

/-- `CSpace::new(limit)`. -/
def new (limit : Nat) : CSpace α :=
  let l := min limit (MAX_SLOTS - 1)
  ⟨[⟨0, none⟩], [], l, min (4 * l + 1) MAX_SLOTS, 0⟩

/-- Generación de un slot recién vaciado, y si se puede reusar. -/
def retire (g : Nat) : Nat × Bool := (g + 1, g + 1 < GEN_MOD)

/-- Todas las capacidades vivas. -/
def caps (cs : CSpace α) : List (Cap α) := cs.slots.filterMap (·.cap)

/-- `CSpace::insert`. Devuelve el handle nuevo. -/
def insert (cs : CSpace α) (c : Cap α) : Except Err (CSpace α × Nat) :=
  if cs.live ≥ cs.limit then .error .noMemory else
  match cs.free with
  | i :: rest =>
    let g := (cs.slots[i]?.map (·.gen)).getD 0
    .ok ({ cs with slots := cs.slots.set i ⟨g, some c⟩, free := rest, live := cs.live + 1 },
         Handle.mk i g)
  | [] =>
    if cs.slots.length ≥ cs.slotLimit then .error .noMemory else
    .ok ({ cs with slots := cs.slots ++ [⟨0, some c⟩], live := cs.live + 1 },
         Handle.mk cs.slots.length 0)

/-- `CSpace::get`. -/
def get (cs : CSpace α) (h : Nat) : Except Err (Cap α) :=
  match cs.slots[Handle.slot h]? with
  | some ⟨g, some c⟩ =>
    if Handle.slot h != 0 && g == Handle.gen h then .ok c else .error .badHandle
  | _ => .error .badHandle

/-- `CSpace::get_with`: además exige derechos. -/
def getWith (cs : CSpace α) (h : Nat) (need : Rights) : Except Err (Cap α) :=
  match cs.get h with
  | .error e => .error e
  | .ok c => if c.rights.contains need then .ok c else .error .accessDenied

/-- `CSpace::remove`: el slot queda libre y con generación nueva. -/
def remove (cs : CSpace α) (h : Nat) : Except Err (CSpace α × Cap α) :=
  match cs.get h with
  | .error e => .error e
  | .ok c =>
    let i := Handle.slot h
    let (g, reuse) := retire (Handle.gen h)
    .ok ({ cs with slots := cs.slots.set i ⟨g, none⟩,
                   free := if reuse then i :: cs.free else cs.free, live := cs.live - 1 }, c)

/-- Validación de `remove_all`: cada handle con `need` y sin repetidos. -/
def checkAll (cs : CSpace α) (need : Rights) : List Nat → List Nat → Except Err Unit
  | [], _ => .ok ()
  | h :: hs, seen =>
    match cs.getWith h need with
    | .error e => .error e
    | .ok _ => if seen.contains h then .error .invalidArgs else checkAll cs need hs (seen ++ [h])

/-- Quita en orden. -/
def removeSeq (cs : CSpace α) : List Nat → Except Err (CSpace α × List (Cap α))
  | [] => .ok (cs, [])
  | h :: hs =>
    match cs.remove h with
    | .error e => .error e
    | .ok (cs₁, c) =>
      match removeSeq cs₁ hs with
      | .error e => .error e
      | .ok (cs₂, cs') => .ok (cs₂, c :: cs')

/-- `CSpace::remove_all`: atómico (o se quitan todas, o ninguna). -/
def removeAll (cs : CSpace α) (hs : List Nat) (need : Rights) :
    Except Err (CSpace α × List (Cap α)) :=
  match checkAll cs need hs [] with
  | .error e => .error e
  | .ok () => removeSeq cs hs

/-- Inserta en orden. -/
def insertSeq (cs : CSpace α) : List (Cap α) → Except Err (CSpace α × List Nat)
  | [] => .ok (cs, [])
  | c :: rest =>
    match cs.insert c with
    | .error e => .error e
    | .ok (cs₁, h) =>
      match insertSeq cs₁ rest with
      | .error e => .error e
      | .ok (cs₂, hs) => .ok (cs₂, h :: hs)

/-- `CSpace::insert_all`: atómico. -/
def insertAll (cs : CSpace α) (cs' : List (Cap α)) : Except Err (CSpace α × List Nat) :=
  if cs.live + cs'.length > cs.limit ||
     cs.slots.length + (cs'.length - cs.free.length) > cs.slotLimit then .error .noMemory
  else insertSeq cs cs'

/-- Recorrido de `clear`. -/
def clearGo : Nat → List (Slot α) → List Nat → List (Cap α) →
    List (Slot α) × List Nat × List (Cap α)
  | _, [], free, out => ([], free, out)
  | i, s :: ss, free, out =>
    match s.cap with
    | some c =>
      let (g, reuse) := retire s.gen
      let r := clearGo (i + 1) ss (if reuse then i :: free else free) (out ++ [c])
      (⟨g, none⟩ :: r.1, r.2)
    | none =>
      let r := clearGo (i + 1) ss free out
      (s :: r.1, r.2)

/-- `CSpace::clear`: quita todo y devuelve las capacidades. -/
def clear (cs : CSpace α) : CSpace α × List (Cap α) :=
  let r := clearGo 0 cs.slots cs.free []
  ({ cs with slots := r.1, free := r.2.1, live := 0 }, r.2.2)

/-- `CSpace::dup`: copia con derechos atenuados (exige `DUPLICATE`). -/
def dup (cs : CSpace α) (h : Nat) (mask : Rights) : Except Err (CSpace α × Nat) :=
  match cs.get h with
  | .error e => .error e
  | .ok c =>
    match c.rights.attenuate mask with
    | none => .error .accessDenied
    | some r => cs.insert ⟨c.obj, r⟩

/-! ## Lemas sobre qué capacidades hay después de cada operación -/

theorem mem_filterMap_set {β γ : Type} (f : β → Option γ) :
    ∀ (l : List β) (i : Nat) (v : β) (x : γ),
      x ∈ (l.set i v).filterMap f → x ∈ l.filterMap f ∨ f v = some x := by
  intro l
  induction l with
  | nil => intro i v x h; simp at h
  | cons a l ih =>
    intro i v x h
    cases i with
    | zero =>
      simp only [List.set_cons_zero, List.filterMap_cons] at h
      cases hv : f v with
      | none =>
        rw [hv] at h; simp only at h
        left; simp only [List.filterMap_cons]; split <;> simp_all
      | some y =>
        rw [hv] at h; simp only [List.mem_cons] at h
        rcases h with h | h
        · right; rw [h]
        · left; simp only [List.filterMap_cons]; split <;> simp_all
    | succ i =>
      simp only [List.set_cons_succ, List.filterMap_cons] at h
      cases hfa : f a with
      | none =>
        rw [hfa] at h; simp only at h
        rcases ih i v x h with h' | h'
        · left; simp [hfa, h']
        · right; exact h'
      | some y =>
        rw [hfa] at h; simp only [List.mem_cons] at h
        rcases h with h | h
        · left; simp [hfa, h]
        · rcases ih i v x h with h' | h'
          · left; simp [hfa, h']
          · right; exact h'

theorem get_mem {cs : CSpace α} {h : Nat} {c : Cap α} (hg : cs.get h = .ok c) :
    c ∈ cs.caps := by
  unfold get at hg
  split at hg
  · rename_i g c' heq
    split at hg
    · cases hg
      simp only [caps, List.mem_filterMap]
      exact ⟨_, List.mem_of_getElem? heq, rfl⟩
    · cases hg
  · cases hg

theorem getWith_get {cs : CSpace α} {h : Nat} {need : Rights} {c : Cap α}
    (hg : cs.getWith h need = .ok c) : cs.get h = .ok c ∧ need ⊆ c.rights := by
  unfold getWith at hg
  split at hg
  · cases hg
  · rename_i c' heq
    split at hg
    · cases hg; exact ⟨heq, (Rights.contains_iff _ _).1 (by assumption)⟩
    · cases hg

/-- `insert` solo agrega la capacidad insertada. -/
theorem insert_caps {cs cs' : CSpace α} {c : Cap α} {h : Nat}
    (hi : cs.insert c = .ok (cs', h)) : ∀ x, x ∈ cs'.caps → x ∈ cs.caps ∨ x = c := by
  intro x hx
  unfold insert at hi
  split at hi
  · cases hi
  · split at hi
    · cases hi
      rcases mem_filterMap_set _ _ _ _ _ hx with h | h
      · exact .inl h
      · simp at h; exact .inr h.symm
    · split at hi
      · cases hi
      cases hi
      simp only [caps, List.filterMap_append, List.mem_append, List.filterMap_cons,
        List.filterMap_nil, List.mem_cons, List.not_mem_nil, or_false] at hx
      rcases hx with h | h
      · exact .inl h
      · exact .inr h

/-- `remove` solo quita: lo que queda ya estaba, y lo quitado también. -/
theorem remove_caps {cs cs' : CSpace α} {h : Nat} {c : Cap α}
    (hr : cs.remove h = .ok (cs', c)) :
    c ∈ cs.caps ∧ cs.get h = .ok c ∧ ∀ x, x ∈ cs'.caps → x ∈ cs.caps := by
  unfold remove at hr
  split at hr
  · cases hr
  · rename_i c' hg
    cases hr
    refine ⟨get_mem hg, hg, ?_⟩
    intro x hx
    rcases mem_filterMap_set _ _ _ _ _ hx with h | h
    · exact h
    · simp at h

theorem removeSeq_caps : ∀ {cs cs' : CSpace α} {hs : List Nat} {out : List (Cap α)},
    cs.removeSeq hs = .ok (cs', out) →
      (∀ x, x ∈ cs'.caps → x ∈ cs.caps) ∧ (∀ x, x ∈ out → x ∈ cs.caps) := by
  intro cs cs' hs
  induction hs generalizing cs with
  | nil => intro out h; simp [removeSeq] at h; obtain ⟨rfl, rfl⟩ := h; simp
  | cons h hs ih =>
    intro out hr
    simp only [removeSeq] at hr
    split at hr
    · cases hr
    · rename_i cs₁ c hrem
      split at hr
      · cases hr
      · rename_i cs₂ out' hrest
        cases hr
        obtain ⟨hc, _, hsub⟩ := remove_caps hrem
        obtain ⟨h1, h2⟩ := ih hrest
        refine ⟨fun x hx => hsub x (h1 x hx), ?_⟩
        intro x hx
        simp only [List.mem_cons] at hx
        rcases hx with rfl | hx
        · exact hc
        · exact hsub x (h2 x hx)

theorem removeAll_caps {cs cs' : CSpace α} {hs : List Nat} {need : Rights}
    {out : List (Cap α)} (hr : cs.removeAll hs need = .ok (cs', out)) :
    (∀ x, x ∈ cs'.caps → x ∈ cs.caps) ∧ (∀ x, x ∈ out → x ∈ cs.caps) := by
  unfold removeAll at hr
  split at hr
  · cases hr
  · exact removeSeq_caps hr

theorem insertSeq_caps : ∀ {cs cs' : CSpace α} {l : List (Cap α)} {hs : List Nat},
    cs.insertSeq l = .ok (cs', hs) → ∀ x, x ∈ cs'.caps → x ∈ cs.caps ∨ x ∈ l := by
  intro cs cs' l
  induction l generalizing cs with
  | nil => intro hs h; simp [insertSeq] at h; obtain ⟨rfl, rfl⟩ := h; simp
  | cons c l ih =>
    intro hs hi x hx
    simp only [insertSeq] at hi
    split at hi
    · cases hi
    · rename_i cs₁ h hins
      split at hi
      · cases hi
      · rename_i cs₂ hs' hrest
        cases hi
        rcases ih hrest x hx with h1 | h1
        · rcases insert_caps hins x h1 with h2 | h2
          · exact .inl h2
          · exact .inr (by simp [h2])
        · exact .inr (by simp [h1])

theorem insertAll_caps {cs cs' : CSpace α} {l : List (Cap α)} {hs : List Nat}
    (hi : cs.insertAll l = .ok (cs', hs)) : ∀ x, x ∈ cs'.caps → x ∈ cs.caps ∨ x ∈ l := by
  unfold insertAll at hi
  split at hi
  · cases hi
  · exact insertSeq_caps hi

/-- `dup` solo agrega una capacidad derivada de una existente, con derechos
iguales o menores. -/
theorem dup_caps {cs cs' : CSpace α} {h : Nat} {mask : Rights} {h' : Nat}
    (hd : cs.dup h mask = .ok (cs', h')) :
    ∀ x, x ∈ cs'.caps → x ∈ cs.caps ∨
      ∃ c, c ∈ cs.caps ∧ x.obj = c.obj ∧ x.rights ⊆ c.rights := by
  intro x hx
  unfold dup at hd
  split at hd
  · cases hd
  · rename_i c hg
    split at hd
    · cases hd
    · rename_i r hatt
      rcases insert_caps hd x hx with h1 | h1
      · exact .inl h1
      · right; subst h1
        exact ⟨c, get_mem hg, rfl, Rights.attenuate_subset hatt⟩

/-- Un handle quitado deja de valer de inmediato. -/
theorem get_after_remove {cs cs' : CSpace α} {h : Nat} {c : Cap α}
    (hr : cs.remove h = .ok (cs', c)) : ∃ e, cs'.get h = .error e := by
  unfold remove at hr
  split at hr
  · cases hr
  · cases hr
    refine ⟨.badHandle, ?_⟩
    unfold get
    split
    · rename_i heq
      simp only [List.getElem?_set] at heq
      repeat (first | split at heq | skip) <;> simp_all
    · rfl

/-- Quitar un handle no cambia lo que devuelven los demás. -/
theorem remove_get {cs cs₁ : CSpace α} {h₀ h : Nat} {c₀ c : Cap α}
    (hr : cs.remove h₀ = .ok (cs₁, c₀)) (hg : cs₁.get h = .ok c) : cs.get h = .ok c := by
  unfold remove at hr
  split at hr
  · cases hr
  · cases hr
    unfold get at hg ⊢
    by_cases hi : Handle.slot h₀ = Handle.slot h
    · exfalso
      simp only [List.getElem?_set, hi, ite_true] at hg
      split at hg
      · rename_i heq
        split at heq <;> simp_all
      · cases hg
    · simp only [List.getElem?_set, hi, ite_false] at hg
      exact hg

theorem removeSeq_out : ∀ {cs cs' : CSpace α} {hs : List Nat} {out : List (Cap α)},
    cs.removeSeq hs = .ok (cs', out) → ∀ x, x ∈ out → ∃ h, h ∈ hs ∧ cs.get h = .ok x := by
  intro cs cs' hs
  induction hs generalizing cs with
  | nil => intro out h; simp [removeSeq] at h; obtain ⟨rfl, rfl⟩ := h; simp
  | cons h hs ih =>
    intro out hr x hx
    simp only [removeSeq] at hr
    split at hr
    · cases hr
    · rename_i cs₁ c hrem
      split at hr
      · cases hr
      · rename_i cs₂ out' hrest
        cases hr
        simp only [List.mem_cons] at hx
        rcases hx with rfl | hx
        · exact ⟨h, by simp, (remove_caps hrem).2.1⟩
        · obtain ⟨h', hm, hg⟩ := ih hrest x hx
          exact ⟨h', by simp [hm], remove_get hrem hg⟩

theorem checkAll_ok : ∀ {cs : CSpace α} {need : Rights} {hs seen : List Nat},
    cs.checkAll need hs seen = .ok () → ∀ h, h ∈ hs → ∃ c, cs.getWith h need = .ok c := by
  intro cs need hs
  induction hs with
  | nil => intro seen _ h hm; simp at hm
  | cons h hs ih =>
    intro seen hc h' hm
    simp only [checkAll] at hc
    split at hc
    · cases hc
    · rename_i c hg
      split at hc
      · cases hc
      · simp only [List.mem_cons] at hm
        rcases hm with rfl | hm
        · exact ⟨c, hg⟩
        · exact ih hc h' hm

/-- **Transferencia**: `remove_all` solo devuelve capacidades que estaban en
la CSpace y que tenían los derechos pedidos (en el IPC, `GRANT`). -/
theorem removeAll_rights {cs cs' : CSpace α} {hs : List Nat} {need : Rights}
    {out : List (Cap α)} (hr : cs.removeAll hs need = .ok (cs', out)) :
    ∀ x, x ∈ out → x ∈ cs.caps ∧ need ⊆ x.rights := by
  intro x hx
  unfold removeAll at hr
  split at hr
  · cases hr
  · rename_i hc
    obtain ⟨h, hm, hg⟩ := removeSeq_out hr x hx
    obtain ⟨c, hw⟩ := checkAll_ok hc h hm
    obtain ⟨hg', hn⟩ := getWith_get hw
    rw [hg] at hg'
    cases hg'
    exact ⟨get_mem hg, hn⟩

/-! ## Los handles viejos no reviven

La generación de un slot solo sube, y un slot que agota sus generaciones se
retira. Entonces, una vez quitado un handle, ninguna secuencia de operaciones
lo vuelve a hacer válido. -/

/-- Slot por slot, la generación de `l'` es mayor o igual que la de `l`. -/
def GenMono (l l' : List (Slot α)) : Prop :=
  ∀ (i g : Nat), (l[i]?).map (fun s : Slot α => s.gen) = some g →
    ∃ g', (l'[i]?).map (fun s : Slot α => s.gen) = some g' ∧ g ≤ g'

theorem GenMono.refl (l : List (Slot α)) : GenMono l l :=
  fun _ g h => ⟨g, h, Nat.le_refl _⟩

theorem GenMono.trans {l₁ l₂ l₃ : List (Slot α)} (h₁ : GenMono l₁ l₂) (h₂ : GenMono l₂ l₃) :
    GenMono l₁ l₃ := by
  intro i g h
  obtain ⟨g₂, h2, le2⟩ := h₁ i g h
  obtain ⟨g₃, h3, le3⟩ := h₂ i g₂ h2
  exact ⟨g₃, h3, Nat.le_trans le2 le3⟩

theorem GenMono.set {l : List (Slot α)} {i : Nat} {v : Slot α}
    (hv : ∀ s, l[i]? = some s → s.gen ≤ v.gen) : GenMono l (l.set i v) := by
  intro j g hj
  rw [List.getElem?_set]
  by_cases hij : i = j
  · subst hij
    cases hl : l[i]? with
    | none => simp [hl] at hj
    | some s =>
      simp [hl] at hj
      have hlt : i < l.length := by
        rcases (List.getElem?_eq_some_iff.1 hl) with ⟨hlt, _⟩; exact hlt
      refine ⟨v.gen, by simp [hlt], ?_⟩
      rw [← hj]; exact hv s hl
  · simp only [hij, ↓reduceIte]; exact ⟨g, hj, Nat.le_refl _⟩

theorem GenMono.append (l r : List (Slot α)) : GenMono l (l ++ r) := by
  intro j g hj
  cases hl : l[j]? with
  | none => simp [hl] at hj
  | some s =>
    have hlt : j < l.length := by
      rcases (List.getElem?_eq_some_iff.1 hl) with ⟨hlt, _⟩; exact hlt
    refine ⟨g, ?_, Nat.le_refl _⟩
    rw [List.getElem?_append_left hlt]; exact hj

theorem GenMono.cons {a b : Slot α} {l l' : List (Slot α)} (hab : a.gen ≤ b.gen)
    (h : GenMono l l') : GenMono (a :: l) (b :: l') := by
  intro j g hj
  cases j with
  | zero => simp at hj; exact ⟨b.gen, by simp, hj ▸ hab⟩
  | succ j => simpa using h j g (by simpa using hj)

theorem insert_mono {cs cs' : CSpace α} {c : Cap α} {h : Nat}
    (hi : cs.insert c = .ok (cs', h)) : GenMono cs.slots cs'.slots := by
  unfold insert at hi
  split at hi
  · cases hi
  · split at hi
    · cases hi
      apply GenMono.set
      intro s hs; simp [hs]
    · split at hi
      · cases hi
      cases hi
      exact GenMono.append _ _

theorem get_slot {cs : CSpace α} {h : Nat} {c : Cap α} (hg : cs.get h = .ok c) :
    cs.slots[Handle.slot h]? = some ⟨Handle.gen h, some c⟩ := by
  unfold get at hg
  split at hg
  · rename_i g c' heq
    split at hg
    · rename_i hc
      cases hg
      simp at hc
      rw [heq, hc.2]
    · cases hg
  · cases hg

theorem remove_mono {cs cs' : CSpace α} {h : Nat} {c : Cap α}
    (hr : cs.remove h = .ok (cs', c)) : GenMono cs.slots cs'.slots := by
  unfold remove at hr
  split at hr
  · cases hr
  · rename_i c' hg
    cases hr
    apply GenMono.set
    intro s hs
    rw [get_slot hg] at hs
    cases hs
    simp

theorem removeSeq_mono : ∀ {cs cs' : CSpace α} {hs : List Nat} {out : List (Cap α)},
    cs.removeSeq hs = .ok (cs', out) → GenMono cs.slots cs'.slots := by
  intro cs cs' hs
  induction hs generalizing cs with
  | nil => intro out h; simp [removeSeq] at h; obtain ⟨rfl, rfl⟩ := h; exact GenMono.refl _
  | cons h hs ih =>
    intro out hr
    simp only [removeSeq] at hr
    split at hr
    · cases hr
    · rename_i cs₁ c hrem
      split at hr
      · cases hr
      · rename_i cs₂ out' hrest
        cases hr
        exact (remove_mono hrem).trans (ih hrest)

theorem insertSeq_mono : ∀ {cs cs' : CSpace α} {l : List (Cap α)} {hs : List Nat},
    cs.insertSeq l = .ok (cs', hs) → GenMono cs.slots cs'.slots := by
  intro cs cs' l
  induction l generalizing cs with
  | nil => intro hs h; simp [insertSeq] at h; obtain ⟨rfl, rfl⟩ := h; exact GenMono.refl _
  | cons c l ih =>
    intro hs hi
    simp only [insertSeq] at hi
    split at hi
    · cases hi
    · rename_i cs₁ h hins
      split at hi
      · cases hi
      · rename_i cs₂ hs' hrest
        cases hi
        exact (insert_mono hins).trans (ih hrest)

theorem clearGo_mono : ∀ (l : List (Slot α)) (i : Nat) (f : List Nat) (o : List (Cap α)),
    GenMono l (clearGo i l f o).1 := by
  intro l
  induction l with
  | nil => intro i f o; simp [clearGo]; exact GenMono.refl _
  | cons s ss ih =>
    intro i f o
    simp only [clearGo]
    split
    · exact GenMono.cons (by simp [retire]) (ih _ _ _)
    · exact GenMono.cons (Nat.le_refl _) (ih _ _ _)

/-- Operaciones sobre una CSpace (las que fallan no cambian nada). -/
inductive COp (α : Type) where
  | insert (c : Cap α)
  | remove (h : Nat)
  | dup (h : Nat) (mask : Rights)
  | removeAll (hs : List Nat) (need : Rights)
  | insertAll (cs : List (Cap α))
  | clear

/-- Aplica una operación. -/
def apply (cs : CSpace α) : COp α → CSpace α
  | .insert c => match cs.insert c with | .ok (cs', _) => cs' | .error _ => cs
  | .remove h => match cs.remove h with | .ok (cs', _) => cs' | .error _ => cs
  | .dup h m => match cs.dup h m with | .ok (cs', _) => cs' | .error _ => cs
  | .removeAll hs n => match cs.removeAll hs n with | .ok (cs', _) => cs' | .error _ => cs
  | .insertAll l => match cs.insertAll l with | .ok (cs', _) => cs' | .error _ => cs
  | .clear => cs.clear.1

theorem apply_mono (cs : CSpace α) (op : COp α) : GenMono cs.slots (cs.apply op).slots := by
  cases op with
  | insert c =>
    simp only [apply]; split
    · rename_i hi; exact insert_mono hi
    · exact GenMono.refl _
  | remove h =>
    simp only [apply]; split
    · rename_i hr; exact remove_mono hr
    · exact GenMono.refl _
  | dup h m =>
    simp only [apply]; split
    · rename_i hd
      unfold dup at hd
      split at hd
      · cases hd
      · split at hd
        · cases hd
        · exact insert_mono hd
    · exact GenMono.refl _
  | removeAll hs n =>
    simp only [apply]; split
    · rename_i hr
      unfold removeAll at hr
      split at hr
      · cases hr
      · exact removeSeq_mono hr
    · exact GenMono.refl _
  | insertAll l =>
    simp only [apply]; split
    · rename_i hi
      unfold insertAll at hi
      split at hi
      · cases hi
      · exact insertSeq_mono hi
    · exact GenMono.refl _
  | clear => exact clearGo_mono _ _ _ _

theorem applyAll_mono : ∀ (ops : List (COp α)) (cs : CSpace α),
    GenMono cs.slots (ops.foldl apply cs).slots := by
  intro ops
  induction ops with
  | nil => intro cs; exact GenMono.refl _
  | cons op ops ih => intro cs; exact (apply_mono cs op).trans (ih _)

/-- **Un handle quitado nunca revive**: después de quitar `h`, ninguna
secuencia de operaciones (por larga que sea) vuelve a hacerlo válido. -/
theorem stale_handle_never_revives {cs cs' : CSpace α} {h : Nat} {c : Cap α}
    (hr : cs.remove h = .ok (cs', c)) (ops : List (COp α)) :
    (ops.foldl apply cs').get h = .error .badHandle := by
  have hg0 := (remove_caps hr).2.1
  have hslot := get_slot hg0
  have hlt : Handle.slot h < cs.slots.length := by
    rcases (List.getElem?_eq_some_iff.1 hslot) with ⟨hlt, _⟩; exact hlt
  -- tras quitar, el slot tiene generación `gen h + 1`
  have h1 : (cs'.slots[Handle.slot h]?).map (fun s : Slot α => s.gen) =
      some (Handle.gen h + 1) := by
    unfold remove at hr
    rw [hg0] at hr
    cases hr
    simp [hlt]
  obtain ⟨g', hg', hle⟩ := applyAll_mono ops cs' _ _ h1
  unfold get
  split
  · rename_i g c' heq
    rw [heq] at hg'
    simp at hg'
    have : g ≠ Handle.gen h := by omega
    simp [this]
  · rfl

end CSpace
end Nsos
