/-!
# Derechos de una capacidad

Modelo de `nsos_abi::cap::Rights`. En Rust los derechos son bits de un `u32`;
aquí son una función `Right → Bool`, que es más cómoda para demostrar. La
correspondencia con los bits la verifican los tests derivados del modelo
(`crates/kcore/tests/model_traces.rs`).
-/

namespace Nsos

/-- Un derecho individual. El orden coincide con los bits del ABI. -/
inductive Right where
  | send | recv | read | write | exec | grant | duplicate | control | wait
  deriving DecidableEq, Repr, Inhabited

namespace Right

/-- Bit del derecho en el ABI (`Rights::SEND = 1 << 0`, etc.). -/
def bit : Right → Nat
  | send => 0 | recv => 1 | read => 2 | write => 3 | exec => 4
  | grant => 5 | duplicate => 6 | control => 7 | wait => 8

/-- Todos los derechos. -/
def all : List Right := [send, recv, read, write, exec, grant, duplicate, control, wait]

theorem mem_all (r : Right) : r ∈ all := by cases r <;> simp [all]

end Right

/-- Conjunto de derechos. -/
structure Rights where
  has : Right → Bool

namespace Rights

/-- Ningún derecho. -/
def none : Rights := ⟨fun _ => false⟩

/-- Intersección (`a & b` en Rust). -/
def inter (a b : Rights) : Rights := ⟨fun x => a.has x && b.has x⟩

/-- `a ⊆ b`: todo derecho de `a` está en `b`. -/
def Subset (a b : Rights) : Prop := ∀ x, a.has x = true → b.has x = true

instance : HasSubset Rights := ⟨Subset⟩

/-- `Rights::contains(self, need)`: `true` si `need ⊆ self`. -/
def contains (self need : Rights) : Bool :=
  Right.all.all fun x => !need.has x || self.has x

/-- Desde bits crudos (solo se miran los 9 bits definidos). -/
def ofBits (n : Nat) : Rights := ⟨fun x => n.testBit x.bit⟩

/-- A bits crudos. -/
def bits (a : Rights) : Nat :=
  Right.all.foldr (fun x acc => if a.has x then acc + 2 ^ x.bit else acc) 0

/-- `Rights::from_bits`: rechaza bits fuera de `Rights::ALL`. -/
def validBits (n : Nat) : Bool := n < 2 ^ 9

/-- `Rights::attenuate`: `self ∩ mask`, solo si `self` tiene `DUPLICATE`. -/
def attenuate (self mask : Rights) : Option Rights :=
  if self.has .duplicate then some (inter self mask) else Option.none

theorem subset_refl (a : Rights) : a ⊆ a := fun _ h => h

theorem subset_trans {a b c : Rights} (h₁ : a ⊆ b) (h₂ : b ⊆ c) : a ⊆ c :=
  fun x h => h₂ x (h₁ x h)

theorem inter_subset_left (a b : Rights) : inter a b ⊆ a := by
  intro x h
  simp [inter] at h
  exact h.1

theorem contains_iff (self need : Rights) : self.contains need = true ↔ need ⊆ self := by
  constructor
  · intro h x hx
    simp only [contains, List.all_eq_true] at h
    have := h x (Right.mem_all x)
    simp [hx] at this
    exact this
  · intro h
    simp only [contains, List.all_eq_true]
    intro x _
    cases hx : need.has x
    · simp
    · simp [h x hx]

/-- **Atenuación**: derivar una capacidad nunca agrega derechos. -/
theorem attenuate_subset {self mask r : Rights} (h : self.attenuate mask = some r) :
    r ⊆ self := by
  unfold attenuate at h
  split at h
  · cases h; exact inter_subset_left _ _
  · cases h

/-- Sin `DUPLICATE` no se puede derivar nada. -/
theorem attenuate_needs_duplicate {self mask r : Rights} (h : self.attenuate mask = some r) :
    self.has .duplicate = true := by
  unfold attenuate at h
  split at h
  · assumption
  · cases h

end Rights
end Nsos
