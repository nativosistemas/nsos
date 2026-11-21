/-!
# Aislamiento de memoria

Modelo de los marcos físicos, los objetos `Memory` y los mapeos de cada cell
(`kernel/src/cell.rs`: `MemObj`, `map_memory`, `revoke_mappings`; syscalls
`mem_create`, `mem_map`, `cap_dup`, `cap_drop`, `channel_send`, `cell_spawn`).

Cada marco tiene un dueño: el kernel, nadie (libre), una cell (sus marcos
propios: código, datos, pila) o un objeto `Memory`. Los mapeos de una cell
apuntan a marcos.

Simplificaciones:
* Se mapea de a una página. El kernel mapea un objeto entero, pero página por
  página con los mismos permisos y la misma comprobación.
* Enviar una capacidad la mueve en un paso de una cell a otra. En el kernel
  pasa por una cola, pero lo que importa aquí es que el emisor la pierde.
* La tabla de páginas se abstrae en una lista de mapeos. Que la tabla real
  haga lo que dice esa lista lo comprueba Kani sobre
  `nsos_kcore::paging` (ver `verification/kani.md`).

Teoremas:

* `Mem.step_iso`: el invariante de aislamiento se mantiene en cada paso.
  Todo mapeo de una cell apunta a un marco propio de esa cell, o a un marco
  de un objeto `Memory` del que la cell tiene una capacidad con los derechos
  del mapeo (escritura solo con `WRITE`).
* `Mem.never_maps_foreign`: por lo tanto, una cell nunca mapea un marco del
  kernel, un marco libre (ni uno que se liberó: no hay uso después de liberar)
  ni un marco propio de otra cell.
* `Mem.send_revokes`: después de enviar su única capacidad a un objeto, la
  cell no tiene ningún mapeo a sus marcos.
* `Mem.no_revocation_breaks`: sin revocación (como era el kernel antes), el
  invariante se rompe: el emisor sigue viendo el objeto que entregó.
-/

namespace Nsos
namespace Mem

/-- Dueño de un marco físico. -/
inductive Owner where
  | kernel
  | free
  /-- Marco propio de una cell. -/
  | cell (a : Nat)
  /-- Marco de un objeto `Memory`. -/
  | obj (o : Nat)
  deriving DecidableEq, Repr

/-- Capacidad a un objeto `Memory` (READ siempre; `write` = WRITE). -/
structure MCap where
  obj : Nat
  write : Bool
  deriving DecidableEq, Repr

/-- De dónde viene un mapeo. -/
inductive Src where
  | priv
  | obj (o : Nat)
  deriving DecidableEq, Repr

/-- Un mapeo de una página: dirección virtual, marco, si es escribible y de
qué viene. -/
structure Mapping where
  vpage : Nat
  frame : Nat
  write : Bool
  src : Src
  deriving DecidableEq, Repr

/-- Lo que el modelo necesita de una cell. -/
structure CellM where
  caps : List MCap
  maps : List Mapping

/-- Estado. -/
structure MSys where
  owner : Nat → Owner
  cells : Nat → CellM
  nextObj : Nat

/-- `c` permite un mapeo de `o` con escritura `w`. -/
def MCap.allows (c : MCap) (o : Nat) (w : Bool) : Bool := c.obj == o && (!w || c.write)

/-- ¿Sigue valiendo el mapeo con estas capacidades? (`Mapping::allowed_by`) -/
def keep (caps : List MCap) (m : Mapping) : Bool :=
  match m.src with
  | .priv => true
  | .obj o => caps.any (·.allows o m.write)

/-- Revocación: quedan solo los mapeos que alguna capacidad sigue
permitiendo (`Cell::revoke_mappings`). -/
def revoke (c : CellM) : CellM := { c with maps := c.maps.filter (keep c.caps) }

/-- Reemplaza una cell. -/
def MSys.set (s : MSys) (a : Nat) (c : CellM) : MSys :=
  { s with cells := fun b => if b = a then c else s.cells b }

/-- Pasos posibles. Cada uno es una syscall (o la creación/terminación de
una cell); los que fallan no cambian el estado y no se listan. -/
inductive Step : MSys → MSys → Prop
  /-- `cell_spawn`: marcos libres pasan a ser propios de la cell `a`. -/
  | spawn {s : MSys} {a : Nat} {fs : List Nat} :
      (∀ f, f ∈ fs → s.owner f = .free) →
      Step s { s with owner := fun f => if f ∈ fs then .cell a else s.owner f }
  /-- `mem_create`: marcos libres pasan a un objeto nuevo; la cell recibe
  una capacidad con `WRITE`. -/
  | create {s : MSys} {a : Nat} {fs : List Nat} :
      (∀ f, f ∈ fs → s.owner f = .free) →
      Step s { s.set a { s.cells a with caps := ⟨s.nextObj, true⟩ :: (s.cells a).caps } with
               owner := fun f => if f ∈ fs then .obj s.nextObj else s.owner f
               nextObj := s.nextObj + 1 }
  /-- Mapear una página propia (carga del ELF, pila). -/
  | mapPriv {s : MSys} {a v f : Nat} {w : Bool} :
      s.owner f = .cell a →
      Step s (s.set a { s.cells a with
        maps := ⟨v, f, w, .priv⟩ :: (s.cells a).maps })
  /-- `mem_map`: hace falta una capacidad al objeto con los derechos
  pedidos, y el marco tiene que ser de ese objeto. -/
  | mapObj {s : MSys} {a v f o : Nat} {w : Bool} {c : MCap} :
      c ∈ (s.cells a).caps → c.allows o w = true → s.owner f = .obj o →
      Step s (s.set a { s.cells a with
        maps := ⟨v, f, w, .obj o⟩ :: (s.cells a).maps })
  /-- Quitar el mapeo de una página. -/
  | unmap {s : MSys} {a v : Nat} :
      Step s (s.set a { s.cells a with
        maps := (s.cells a).maps.filter (·.vpage != v) })
  /-- `cap_dup`: copia con derechos iguales o menores. -/
  | dup {s : MSys} {a : Nat} {c : MCap} {w : Bool} :
      c ∈ (s.cells a).caps →
      Step s (s.set a { s.cells a with
        caps := ⟨c.obj, w && c.write⟩ :: (s.cells a).caps })
  /-- `cap_drop`, seguido de la revocación. -/
  | drop {s : MSys} {a : Nat} {c : MCap} :
      c ∈ (s.cells a).caps →
      Step s (s.set a (revoke { s.cells a with caps := (s.cells a).caps.erase c }))
  /-- `channel_send` o `cell_spawn` con una capacidad de memoria: pasa de
  `a` a `b`, y a `a` se le revocan los mapeos. -/
  | send {s : MSys} {a b : Nat} {c : MCap} :
      a ≠ b → c ∈ (s.cells a).caps →
      Step s ((s.set a (revoke { s.cells a with caps := (s.cells a).caps.erase c })).set b
        { s.cells b with caps := c :: (s.cells b).caps })
  /-- Muere la última referencia a un objeto (`Drop for MemObj`): sus marcos
  vuelven a estar libres. Solo si ninguna cell tiene una capacidad a él. -/
  | freeObj {s : MSys} {o : Nat} :
      (∀ a c, c ∈ (s.cells a).caps → c.obj ≠ o) →
      Step s { s with owner := fun f => if s.owner f = Owner.obj o then Owner.free else s.owner f }
  /-- Termina la cell `a`: pierde capacidades y mapeos, y sus marcos propios
  quedan libres. -/
  | kill {s : MSys} {a : Nat} :
      Step s ({ s with owner := fun f => if s.owner f = Owner.cell a then Owner.free else s.owner f }.set a
        ⟨[], []⟩)

/-! ## Invariante -/

/-- El mapeo `m` de la cell `a` es legítimo. -/
def MapOk (s : MSys) (a : Nat) (m : Mapping) : Prop :=
  match m.src with
  | .priv => s.owner m.frame = .cell a
  | .obj o => s.owner m.frame = .obj o ∧ ∃ c, c ∈ (s.cells a).caps ∧ c.allows o m.write = true

/-- Aislamiento: todos los mapeos de todas las cells son legítimos. -/
def Iso (s : MSys) : Prop := ∀ a m, m ∈ (s.cells a).maps → MapOk s a m

theorem set_cells (s : MSys) (a : Nat) (c : CellM) (b : Nat) :
    (s.set a c).cells b = if b = a then c else s.cells b := rfl

theorem mapOk_not_free {s : MSys} {a : Nat} {m : Mapping} (h : MapOk s a m) :
    s.owner m.frame ≠ .free := by
  obtain ⟨v, f, w, src⟩ := m
  cases src <;> simp only [MapOk] at h ⊢
  · rw [h]; intro h'; cases h'
  · rw [h.1]; intro h'; cases h'

/-- Un mapeo legítimo sigue siéndolo si su marco no cambia de dueño y la
cell no perdió capacidades. -/
theorem mapOk_congr {s s' : MSys} {a : Nat} {m : Mapping} (h : MapOk s a m)
    (ho : s'.owner m.frame = s.owner m.frame)
    (hc : ∀ c, c ∈ (s.cells a).caps → c ∈ (s'.cells a).caps) : MapOk s' a m := by
  obtain ⟨v, f, w, src⟩ := m
  simp only at ho
  cases src <;> simp only [MapOk] at h ⊢
  · rw [ho]; exact h
  · obtain ⟨h1, c, hc1, hc2⟩ := h
    exact ⟨ho.trans h1, c, hc _ hc1, hc2⟩

theorem mem_revoke {c : CellM} {m : Mapping} :
    m ∈ (revoke c).maps ↔ m ∈ c.maps ∧ keep c.caps m = true := by
  simp [revoke, List.mem_filter]

/-- Lo que deja la revocación es legítimo con las capacidades que quedan. -/
theorem mapOk_revoke {s s' : MSys} {a : Nat} {m : Mapping} (h : MapOk s a m)
    (ho : s'.owner m.frame = s.owner m.frame) (hk : keep (s'.cells a).caps m = true) :
    MapOk s' a m := by
  obtain ⟨v, f, w, src⟩ := m
  simp only at ho
  cases src <;> simp only [MapOk, keep] at h hk ⊢
  · rw [ho]; exact h
  · obtain ⟨c, hc1, hc2⟩ := List.any_eq_true.1 hk
    exact ⟨ho.trans h.1, c, hc1, hc2⟩

theorem allows_obj {c : MCap} {o : Nat} {w : Bool} (h : c.allows o w = true) : c.obj = o := by
  unfold MCap.allows at h
  simp only [Bool.and_eq_true, beq_iff_eq] at h
  exact h.1

/-! ## El invariante se mantiene -/

/-- **Aislamiento de memoria**: ningún paso rompe el invariante. -/
theorem step_iso {s s' : MSys} (h : Iso s) (hs : Step s s') : Iso s' := by
  cases hs with
  | @spawn a fs hf =>
    intro b m hm
    have hok := h b m hm
    refine mapOk_congr hok ?_ (fun _ x => x)
    have hn : m.frame ∉ fs := fun hin => mapOk_not_free hok (hf _ hin)
    simp [hn]
  | @create a fs hf =>
    intro b m hm
    simp only [set_cells] at hm
    have hm' : m ∈ (s.cells b).maps := by
      split at hm
      · rename_i hb; subst hb; exact hm
      · exact hm
    have hok := h b m hm'
    have hn : m.frame ∉ fs := fun hin => mapOk_not_free hok (hf _ hin)
    refine mapOk_congr hok (by simp [hn]) ?_
    intro c hc
    simp only [MSys.set]
    split
    · exact List.mem_cons_of_mem _ (by rename_i hb; subst hb; exact hc)
    · exact hc
  | @mapPriv a v f w hf =>
    intro b m hm
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb
      simp only [List.mem_cons] at hm
      rcases hm with rfl | hm
      · exact hf
      · exact mapOk_congr (h b m hm) rfl (fun c hc => by simp [MSys.set]; exact hc)
    · exact mapOk_congr (h b m hm) rfl (fun c hc => by simp [MSys.set, *])
  | @mapObj a v f o w c hc hal hf =>
    intro b m hm
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb
      simp only [List.mem_cons] at hm
      rcases hm with rfl | hm
      · exact ⟨hf, c, by simp [MSys.set]; exact hc, hal⟩
      · exact mapOk_congr (h b m hm) rfl (fun c hc => by simp [MSys.set]; exact hc)
    · exact mapOk_congr (h b m hm) rfl (fun c hc => by simp [MSys.set, *])
  | @unmap a v =>
    intro b m hm
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb
      exact mapOk_congr (h b m (List.mem_filter.1 hm).1) rfl
        (fun c hc => by simp [MSys.set]; exact hc)
    · exact mapOk_congr (h b m hm) rfl (fun c hc => by simp [MSys.set, *])
  | @dup a c w hc =>
    intro b m hm
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb
      exact mapOk_congr (h b m hm) rfl
        (fun c hc => by simp [MSys.set]; exact .inr hc)
    · exact mapOk_congr (h b m hm) rfl (fun c hc => by simp [MSys.set, *])
  | @drop a c hc =>
    intro b m hm
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb
      obtain ⟨hm, hk⟩ := mem_revoke.1 hm
      exact mapOk_revoke (h b m hm) rfl (by simpa [MSys.set, revoke] using hk)
    · exact mapOk_congr (h b m hm) rfl (fun c hc => by simp [MSys.set, *])
  | @send a a' c hne hc =>
    intro b m hm
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb
      have hm' : m ∈ (s.cells b).maps := by
        simp only at hm ⊢
        exact hm
      exact mapOk_congr (h b m hm') rfl
        (fun c hc => by simp [MSys.set]; exact .inr hc)
    · split at hm
      · rename_i hb' hb; subst hb
        obtain ⟨hm, hk⟩ := mem_revoke.1 hm
        exact mapOk_revoke (h b m hm) rfl (by simpa [MSys.set, revoke, hb'] using hk)
      · exact mapOk_congr (h b m hm) rfl (fun c hc => by simp [MSys.set, *])
  | @freeObj o hno =>
    intro b m hm
    have hok := h b m hm
    refine mapOk_congr hok ?_ (fun _ x => x)
    have : s.owner m.frame ≠ .obj o := by
      obtain ⟨v, f, w, src⟩ := m
      cases src <;> simp only [MapOk] at hok ⊢
      · rw [hok]; intro h'; cases h'
      · rename_i o'
        obtain ⟨h1, c, hc1, hc2⟩ := hok
        rw [h1]; intro h'; cases h'
        exact hno b c hc1 (allows_obj hc2)
    simp [this]
  | @kill a =>
    intro b m hm
    simp only [set_cells] at hm
    split at hm
    · simp at hm
    · rename_i hb
      have hok := h b m hm
      refine mapOk_congr hok ?_ (fun c hc => by simp [MSys.set, hb]; exact hc)
      have : s.owner m.frame ≠ .cell a := by
        obtain ⟨v, f, w, src⟩ := m
        cases src <;> simp only [MapOk] at hok ⊢
        · rw [hok]; intro h'; cases h'; exact hb rfl
        · rw [hok.1]; intro h'; cases h'
      simp [MSys.set, this]

/-! ## Consecuencias -/

/-- **Una cell nunca ve memoria ajena**: ni del kernel, ni libre (tampoco
memoria ya liberada), ni propia de otra cell. -/
theorem never_maps_foreign {s : MSys} (h : Iso s) {a : Nat} {m : Mapping}
    (hm : m ∈ (s.cells a).maps) :
    s.owner m.frame ≠ .kernel ∧ s.owner m.frame ≠ .free ∧
      ∀ b, b ≠ a → s.owner m.frame ≠ .cell b := by
  have hok := h a m hm
  refine ⟨?_, mapOk_not_free hok, ?_⟩
  · obtain ⟨v, f, w, src⟩ := m
    cases src <;> simp only [MapOk] at hok ⊢
    · rw [hok]; intro h'; cases h'
    · rw [hok.1]; intro h'; cases h'
  · intro b hb
    obtain ⟨v, f, w, src⟩ := m
    cases src <;> simp only [MapOk] at hok ⊢
    · rw [hok]; intro h'; cases h'; exact hb rfl
    · rw [hok.1]; intro h'; cases h'

/-- Escribir exige `WRITE`: un mapeo escribible de un objeto viene de una
capacidad con `WRITE` que la cell todavía tiene. -/
theorem write_needs_cap {s : MSys} (h : Iso s) {a : Nat} {m : Mapping} {o : Nat}
    (hm : m ∈ (s.cells a).maps) (hs : m.src = .obj o) (hw : m.write = true) :
    ∃ c, c ∈ (s.cells a).caps ∧ c.obj = o ∧ c.write = true := by
  have hok := h a m hm
  obtain ⟨v, f, w, src⟩ := m
  simp only at hs hw
  subst hs hw
  simp only [MapOk] at hok
  obtain ⟨_, c, hc1, hc2⟩ := hok
  refine ⟨c, hc1, allows_obj hc2, ?_⟩
  unfold MCap.allows at hc2
  simp at hc2
  exact hc2.2

/-- Los marcos del kernel nunca cambian de dueño. -/
theorem step_kernel {s s' : MSys} (hs : Step s s') {f : Nat} (hk : s.owner f = .kernel) :
    s'.owner f = .kernel := by
  cases hs with
  | @spawn a fs hf =>
    have : f ∉ fs := fun hin => by rw [hf _ hin] at hk; cases hk
    simp [this, hk]
  | @create a fs hf =>
    have : f ∉ fs := fun hin => by rw [hf _ hin] at hk; cases hk
    simp [this, hk]
  | freeObj => simp [hk]
  | kill => simp [MSys.set, hk]
  | _ => exact hk

/-- **Revocación**: si `a` envía su única capacidad a un objeto, no le queda
ningún mapeo de ese objeto. -/
theorem send_revokes {s : MSys} {a b : Nat} {c : MCap} (hne : a ≠ b)
    (hlast : ∀ c', c' ∈ (s.cells a).caps.erase c → c'.obj ≠ c.obj) {m : Mapping}
    (hm : m ∈ (((s.set a (revoke { s.cells a with caps := (s.cells a).caps.erase c })).set b
        { s.cells b with caps := c :: (s.cells b).caps }).cells a).maps) :
    m.src ≠ .obj c.obj := by
  simp only [set_cells, hne, ite_false, ite_true] at hm
  obtain ⟨_, hk⟩ := mem_revoke.1 hm
  intro hs
  unfold keep at hk
  simp only [hs] at hk
  obtain ⟨c', hc', hal⟩ := List.any_eq_true.1 hk
  exact hlast c' hc' (allows_obj hal)

/-! ## `mem_unmap`

`mem_unmap` (hito 13) es una sucesión de pasos `unmap`, uno por página del
objeto. Como todo paso, conserva el aislamiento (`step_iso`). Además: -/

/-- Quitar la página `v` a la cell `a`. -/
def unmapPage (s : MSys) (a v : Nat) : MSys :=
  s.set a { s.cells a with maps := (s.cells a).maps.filter (·.vpage != v) }

/-- Después de `unmap`, la cell no tiene ningún mapeo en esa página: un acceso
ahí es un fallo de página. -/
theorem unmap_removes (s : MSys) (a v : Nat) :
    ∀ m, m ∈ ((unmapPage s a v).cells a).maps → m.vpage ≠ v := by
  intro m hm
  simp only [unmapPage, set_cells, ite_true, List.mem_filter] at hm
  simpa using hm.2

/-- `unmap` solo achica: no agrega mapeos, no toca capacidades, ni a otras
cells, ni a los dueños de los marcos. -/
theorem unmap_only_shrinks (s : MSys) (a v : Nat) :
    (∀ m, m ∈ ((unmapPage s a v).cells a).maps → m ∈ (s.cells a).maps) ∧
    ((unmapPage s a v).cells a).caps = (s.cells a).caps ∧
    (∀ b, b ≠ a → (unmapPage s a v).cells b = s.cells b) ∧
    (unmapPage s a v).owner = s.owner := by
  refine ⟨fun m hm => ?_, ?_, fun b hb => ?_, rfl⟩
  · simp only [unmapPage, set_cells, ite_true, List.mem_filter] at hm
    exact hm.1
  · simp [unmapPage, set_cells]
  · simp [unmapPage, set_cells, hb]

/-- `unmapPage` es un paso del modelo, así que conserva el aislamiento. -/
theorem unmap_iso {s : MSys} (h : Iso s) (a v : Nat) : Iso (unmapPage s a v) :=
  step_iso h Step.unmap

/-- Un sistema sin mapeos cumple el invariante. -/
theorem iso_empty {s : MSys} (h : ∀ a, (s.cells a).maps = []) : Iso s := by
  intro a m hm
  rw [h a] at hm
  cases hm

/-! ## Sin revocación se rompe

Así era `channel_send` antes: la capacidad se va, el mapeo queda. -/

/-- `send` sin revocación. -/
def sendOld (s : MSys) (a b : Nat) (c : MCap) : MSys :=
  (s.set a { s.cells a with caps := (s.cells a).caps.erase c }).set b
    { s.cells b with caps := c :: (s.cells b).caps }

/-- La cell 0 creó el objeto 0 (marco 0) y lo mapeó con escritura. -/
def before : MSys where
  owner f := if f = 0 then .obj 0 else .free
  cells a := if a = 0 then ⟨[⟨0, true⟩], [⟨1, 0, true, .obj 0⟩]⟩ else ⟨[], []⟩
  nextObj := 1

theorem no_revocation_breaks :
    Iso before ∧ ¬ Iso (sendOld before 0 1 ⟨0, true⟩) := by
  constructor
  · intro a m hm
    by_cases ha : a = 0
    · subst ha
      simp [before] at hm
      subst hm
      simp [MapOk, before, MCap.allows]
    · simp [before, ha] at hm
  · intro h
    have := h 0 ⟨1, 0, true, .obj 0⟩ (by simp [sendOld, set_cells, before])
    simp [MapOk, sendOld, set_cells, before] at this

end Mem
end Nsos
