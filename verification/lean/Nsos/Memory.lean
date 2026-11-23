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

Cells selladas (ninguna cell genera código ni toca el código de otra):

* `Mem.step_sealed`: el invariante `Sealed` se mantiene en cada paso. Todo
  mapeo ejecutable es de un marco propio de la cell (su imagen ELF), y ningún
  mapeo de ninguna cell a ese marco es escribible.
* `Mem.exec_is_own_code`: lo que una cell ejecuta es un marco propio de ella:
  nunca un objeto `Memory` (que se puede escribir o compartir) ni un marco
  de otra cell.
* `Mem.code_never_writable`: nadie, ni la propia cell ni otra, tiene un mapeo
  escribible del código de una cell (W^X por marco, no solo por mapeo).
* `Mem.exec_obj_breaks`: con el `mem_map` de antes (que aceptaba `EXEC` si
  la capacidad lo tenía), mapear el mismo objeto como RW y como RX rompe
  el invariante: la cell escribe código y lo ejecuta.
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

/-- Un mapeo de una página: dirección virtual, marco, si es escribible, si
es ejecutable y de qué viene. -/
structure Mapping where
  vpage : Nat
  frame : Nat
  write : Bool
  exec : Bool
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
  /-- Mapear una página propia (carga del ELF, pila; `cell::spawn`): nunca
  escribible y ejecutable a la vez (`elf::parse` rechaza los segmentos W+X),
  y cada marco una sola vez (`map_user_page` reserva un marco nuevo por
  página y los segmentos no se solapan). -/
  | mapPriv {s : MSys} {a v f : Nat} {w x : Bool} :
      s.owner f = .cell a → (w && x) = false →
      (∀ m, m ∈ (s.cells a).maps → m.frame ≠ f) →
      Step s (s.set a { s.cells a with
        maps := ⟨v, f, w, x, .priv⟩ :: (s.cells a).maps })
  /-- `mem_map`: hace falta una capacidad al objeto con los derechos
  pedidos, y el marco tiene que ser de ese objeto. Nunca es ejecutable:
  `EXEC` no es un derecho de un objeto `Memory` y `mem_map` lo rechaza
  (`nsos_kcore::paging::map_request`). -/
  | mapObj {s : MSys} {a v f o : Nat} {w : Bool} {c : MCap} :
      c ∈ (s.cells a).caps → c.allows o w = true → s.owner f = .obj o →
      Step s (s.set a { s.cells a with
        maps := ⟨v, f, w, false, .obj o⟩ :: (s.cells a).maps })
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
  obtain ⟨v, f, w, x, src⟩ := m
  cases src <;> simp only [MapOk] at h ⊢
  · rw [h]; intro h'; cases h'
  · rw [h.1]; intro h'; cases h'

/-- Un mapeo legítimo sigue siéndolo si su marco no cambia de dueño y la
cell no perdió capacidades. -/
theorem mapOk_congr {s s' : MSys} {a : Nat} {m : Mapping} (h : MapOk s a m)
    (ho : s'.owner m.frame = s.owner m.frame)
    (hc : ∀ c, c ∈ (s.cells a).caps → c ∈ (s'.cells a).caps) : MapOk s' a m := by
  obtain ⟨v, f, w, x, src⟩ := m
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
  obtain ⟨v, f, w, x, src⟩ := m
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
  | @mapPriv a v f w x hf _ _ =>
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
      obtain ⟨v, f, w, x, src⟩ := m
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
        obtain ⟨v, f, w, x, src⟩ := m
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
  · obtain ⟨v, f, w, x, src⟩ := m
    cases src <;> simp only [MapOk] at hok ⊢
    · rw [hok]; intro h'; cases h'
    · rw [hok.1]; intro h'; cases h'
  · intro b hb
    obtain ⟨v, f, w, x, src⟩ := m
    cases src <;> simp only [MapOk] at hok ⊢
    · rw [hok]; intro h'; cases h'; exact hb rfl
    · rw [hok.1]; intro h'; cases h'

/-- Escribir exige `WRITE`: un mapeo escribible de un objeto viene de una
capacidad con `WRITE` que la cell todavía tiene. -/
theorem write_needs_cap {s : MSys} (h : Iso s) {a : Nat} {m : Mapping} {o : Nat}
    (hm : m ∈ (s.cells a).maps) (hs : m.src = .obj o) (hw : m.write = true) :
    ∃ c, c ∈ (s.cells a).caps ∧ c.obj = o ∧ c.write = true := by
  have hok := h a m hm
  obtain ⟨v, f, w, x, src⟩ := m
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

/-! ## Cells selladas

Una cell solo ejecuta el código de su imagen. El kernel no tiene una
operación que cree código: los marcos ejecutables salen de `cell::spawn`
(segmentos del ELF, nunca W+X) y `mem_map` nunca mapea un objeto como
ejecutable. -/

/-- Sellado: todo mapeo ejecutable es de un marco propio, y ningún mapeo (de
ninguna cell, incluido el mismo) a ese marco es escribible. -/
def Sealed (s : MSys) : Prop :=
  ∀ a m, m ∈ (s.cells a).maps → m.exec = true →
    m.src = .priv ∧ ∀ b m', m' ∈ (s.cells b).maps → m'.frame = m.frame → m'.write = false

/-- Si los mapeos solo se achican, el sellado se mantiene. -/
theorem sealed_mono {s s' : MSys} (h : Sealed s)
    (hsub : ∀ b m, m ∈ (s'.cells b).maps → m ∈ (s.cells b).maps) : Sealed s' := by
  intro a m hm hx
  obtain ⟨hp, hw⟩ := h a m (hsub a m hm) hx
  exact ⟨hp, fun b m' hm' hf => hw b m' (hsub b m' hm') hf⟩

/-- Agregar un mapeo `n` a la cell `a` mantiene el sellado si `n`, de ser
ejecutable, es propio, no escribible y nadie escribe su marco; y si ningún
mapeo ejecutable comparte marco con `n` cuando `n` es escribible. -/
theorem sealed_add {s s' : MSys} {a : Nat} {n : Mapping} (h : Sealed s)
    (hsub : ∀ b m, m ∈ (s'.cells b).maps → (b = a ∧ m = n) ∨ m ∈ (s.cells b).maps)
    (hnew : n.exec = true → n.src = .priv ∧ n.write = false ∧
      ∀ b m, m ∈ (s.cells b).maps → m.frame = n.frame → m.write = false)
    (hold : ∀ b m, m ∈ (s.cells b).maps → m.exec = true → m.frame = n.frame →
      n.write = false) : Sealed s' := by
  intro b m hm hx
  rcases hsub b m hm with ⟨_, rfl⟩ | hm
  · obtain ⟨hp, hw, ho⟩ := hnew hx
    refine ⟨hp, fun c m' hm' hf => ?_⟩
    rcases hsub c m' hm' with ⟨_, rfl⟩ | hm'
    · exact hw
    · exact ho c m' hm' hf
  · obtain ⟨hp, hw⟩ := h b m hm hx
    refine ⟨hp, fun c m' hm' hf => ?_⟩
    rcases hsub c m' hm' with ⟨_, rfl⟩ | hm'
    · exact hold b m hm hx hf.symm
    · exact hw c m' hm' hf

/-- Un marco propio de `a` que `a` no mapeó no lo mapea nadie (por el
aislamiento, nadie más puede mapear marcos propios de `a`). -/
theorem unmapped_frame {s : MSys} (h : Iso s) {a f : Nat} (ho : s.owner f = .cell a)
    (hfr : ∀ m, m ∈ (s.cells a).maps → m.frame ≠ f) :
    ∀ b m, m ∈ (s.cells b).maps → m.frame ≠ f := by
  intro b m hm hf
  have hok := h b m hm
  obtain ⟨v, f', w, x, src⟩ := m
  simp only at hf
  subst hf
  cases src with
  | priv =>
    simp only [MapOk] at hok
    rw [ho] at hok
    cases hok
    exact hfr _ hm rfl
  | obj o =>
    simp only [MapOk] at hok
    rw [ho] at hok
    exact Owner.noConfusion hok.1

/-- Un mapeo ejecutable de un sistema aislado y sellado es de un marco propio. -/
theorem exec_owner {s : MSys} (h : Iso s) (hs : Sealed s) {a : Nat} {m : Mapping}
    (hm : m ∈ (s.cells a).maps) (hx : m.exec = true) : s.owner m.frame = .cell a := by
  have hok := h a m hm
  obtain ⟨v, f, w, x, src⟩ := m
  have hp := (hs a _ hm hx).1
  simp only at hp
  subst hp
  exact hok

/-- **Sellado**: ningún paso rompe el invariante (sobre un sistema aislado,
que lo es siempre por `step_iso`). -/
theorem step_sealed {s s' : MSys} (hi : Iso s) (h : Sealed s) (hs : Step s s') :
    Sealed s' := by
  cases hs with
  | spawn _ => exact h
  | create _ =>
    refine sealed_mono h fun b m hm => ?_
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb; exact hm
    · exact hm
  | @mapPriv a v f w x hf hwx hfr =>
    have hnone := unmapped_frame hi hf hfr
    refine sealed_add (a := a) (n := ⟨v, f, w, x, .priv⟩) h ?_ ?_ ?_
    · intro b m hm
      simp only [set_cells] at hm
      split at hm
      · rename_i hb; subst hb
        simp only [List.mem_cons] at hm
        rcases hm with rfl | hm
        · exact .inl ⟨rfl, rfl⟩
        · exact .inr hm
      · exact .inr hm
    · intro hx
      simp only at hx
      subst hx
      refine ⟨rfl, ?_, fun b m hm hmf => absurd hmf (hnone b m hm)⟩
      simpa using hwx
    · intro b m hm _ hmf
      exact absurd hmf (hnone b m hm)
  | @mapObj a v f o w c hc hal hf =>
    refine sealed_add (a := a) (n := ⟨v, f, w, false, .obj o⟩) h ?_ ?_ ?_
    · intro b m hm
      simp only [set_cells] at hm
      split at hm
      · rename_i hb; subst hb
        simp only [List.mem_cons] at hm
        rcases hm with rfl | hm
        · exact .inl ⟨rfl, rfl⟩
        · exact .inr hm
      · exact .inr hm
    · intro hx; cases hx
    · intro b m hm hx hmf
      have := exec_owner hi h hm hx
      simp only at hmf
      rw [hmf, hf] at this
      cases this
  | unmap =>
    refine sealed_mono h fun b m hm => ?_
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb; exact (List.mem_filter.1 hm).1
    · exact hm
  | dup _ =>
    refine sealed_mono h fun b m hm => ?_
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb; exact hm
    · exact hm
  | drop _ =>
    refine sealed_mono h fun b m hm => ?_
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb; exact (mem_revoke.1 hm).1
    · exact hm
  | @send a a' c hne hc =>
    refine sealed_mono h fun b m hm => ?_
    simp only [set_cells] at hm
    split at hm
    · rename_i hb; subst hb; exact hm
    · split at hm
      · rename_i hb' hb; subst hb; exact (mem_revoke.1 hm).1
      · exact hm
  | freeObj _ => exact h
  | kill =>
    refine sealed_mono h fun b m hm => ?_
    simp only [set_cells] at hm
    split at hm
    · simp at hm
    · exact hm

/-- Aislamiento y sellado juntos se mantienen en cada paso. -/
theorem step_iso_sealed {s s' : MSys} (h : Iso s ∧ Sealed s) (hs : Step s s') :
    Iso s' ∧ Sealed s' :=
  ⟨step_iso h.1 hs, step_sealed h.1 h.2 hs⟩

/-- **Lo que una cell ejecuta es código propio**: un marco suyo, nunca un
objeto `Memory`, ni un marco del kernel, libre o de otra cell. -/
theorem exec_is_own_code {s : MSys} (h : Iso s) (hs : Sealed s) {a : Nat} {m : Mapping}
    (hm : m ∈ (s.cells a).maps) (hx : m.exec = true) :
    m.src = .priv ∧ s.owner m.frame = .cell a := ⟨(hs a m hm hx).1, exec_owner h hs hm hx⟩

/-- **Nadie escribe código**: ningún mapeo de ninguna cell (tampoco de la
dueña) al marco de un mapeo ejecutable es escribible. -/
theorem code_never_writable {s : MSys} (hs : Sealed s) {a b : Nat} {m m' : Mapping}
    (hm : m ∈ (s.cells a).maps) (hx : m.exec = true) (hm' : m' ∈ (s.cells b).maps)
    (hf : m'.frame = m.frame) : m'.write = false :=
  (hs a m hm hx).2 b m' hm' hf

/-- Un objeto `Memory` nunca se ejecuta. -/
theorem obj_never_exec {s : MSys} (hs : Sealed s) {a o : Nat} {m : Mapping}
    (hm : m ∈ (s.cells a).maps) (ho : m.src = .obj o) : m.exec = false := by
  cases hx : m.exec
  · rfl
  · rw [(hs a m hm hx).1] at ho; cases ho

/-- Un sistema sin mapeos está sellado. -/
theorem sealed_empty {s : MSys} (h : ∀ a, (s.cells a).maps = []) : Sealed s := by
  intro a m hm
  rw [h a] at hm
  cases hm

/-! ### Con el `mem_map` de antes se rompe

Antes `mem_map` aceptaba `EXEC` si la capacidad lo tenía, y W^X se miraba en
cada mapeo, no en cada objeto: con una capacidad `READ | WRITE | EXEC`, la
cell mapeaba el objeto como RW en una dirección y como RX en otra. -/

/-- `mem_map` de antes: el mapeo de un objeto puede ser ejecutable (nunca
W+X en el mismo mapeo). -/
def mapObjOld (s : MSys) (a v f o : Nat) (w x : Bool) : MSys :=
  s.set a { s.cells a with maps := ⟨v, f, w, x, .obj o⟩ :: (s.cells a).maps }

/-- La cell 0 creó el objeto 0 (marco 0) y lo mapeó RW en la página 1. -/
def rwMapped : MSys where
  owner f := if f = 0 then .obj 0 else .free
  cells a := if a = 0 then ⟨[⟨0, true⟩], [⟨1, 0, true, false, .obj 0⟩]⟩ else ⟨[], []⟩
  nextObj := 1

theorem exec_obj_breaks :
    Iso rwMapped ∧ Sealed rwMapped ∧ ¬ Sealed (mapObjOld rwMapped 0 2 0 0 false true) := by
  refine ⟨?_, ?_, ?_⟩
  · intro a m hm
    by_cases ha : a = 0
    · subst ha
      simp [rwMapped] at hm
      subst hm
      simp [MapOk, rwMapped, MCap.allows]
    · simp [rwMapped, ha] at hm
  · intro a m hm hx
    by_cases ha : a = 0
    · subst ha
      simp [rwMapped] at hm
      subst hm
      cases hx
    · simp [rwMapped, ha] at hm
  · intro h
    have := (h 0 ⟨2, 0, false, true, .obj 0⟩ (by simp [mapObjOld, set_cells]) rfl).1
    cases this

/-! ## Sin revocación se rompe

Así era `channel_send` antes: la capacidad se va, el mapeo queda. -/

/-- `send` sin revocación. -/
def sendOld (s : MSys) (a b : Nat) (c : MCap) : MSys :=
  (s.set a { s.cells a with caps := (s.cells a).caps.erase c }).set b
    { s.cells b with caps := c :: (s.cells b).caps }

/-- La cell 0 creó el objeto 0 (marco 0) y lo mapeó con escritura. -/
def before : MSys where
  owner f := if f = 0 then .obj 0 else .free
  cells a := if a = 0 then ⟨[⟨0, true⟩], [⟨1, 0, true, false, .obj 0⟩]⟩ else ⟨[], []⟩
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
    have := h 0 ⟨1, 0, true, false, .obj 0⟩ (by simp [sendOld, set_cells, before])
    simp [MapOk, sendOld, set_cells, before] at this

end Mem
end Nsos
