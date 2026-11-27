/-!
# Cuotas de memoria por supervisor

Modelo de la contabilidad de memoria que cierra el punto 4 de
`docs/amenazas.md`: hoy una cell puede dejar sin marcos a todas las demás.
Se escribe **antes** que el código del kernel.

Cada cell viva tiene una **cuota disponible** (`avail`: marcos que todavía
puede usar) y marcos **usados** (`used`: sus objetos `Memory`, tablas de
páginas, imagen). Una cell nueva la crea su supervisor con `cell_spawn`, que
le pasa parte de *su propia* cuota disponible. Al morir, lo que le quedaba
vuelve a la cuota disponible del supervisor, y lo que usaba pasa a contarse
como usado del supervisor hasta que se libere.

El kernel mantiene un único número global: los marcos físicos libres
(`free`). El invariante es que esos marcos son exactamente la suma de las
cuotas disponibles de las cells vivas, y que ningún marco se pierde.

## Pasos del modelo y su código futuro

| Paso | Código |
|---|---|
| `alloc c n` | `mem_create` (y las tablas de páginas de `mem_map`): cobra `n` marcos a `c` |
| `release c n` | soltar la última capacidad de un objeto `Memory` de `c` |
| `spawn p d q` | `cell_spawn` de `p` con cuota `q` para la hija `d` |
| `kill p d` | la hija `d` de `p` termina: su cuota disponible vuelve a `p`, y sus marcos usados pasan a ser de `p` (un objeto `Memory` que la hija mandó a otra cell puede sobrevivirla; se devuelve con `release p n` cuando se libera) |

## Qué se demuestra

* `step_inv`: en todo paso, los marcos libres son la suma de las cuotas
  disponibles de las cells vivas, y libres más usados es el total (ningún
  marco se pierde ni aparece).
* `avail_le_free`: la cuota disponible de cualquier cell siempre está
  respaldada por marcos libres.
* `alloc_ok`: una cell que pide hasta su cuota disponible siempre consigue
  los marcos.
* `step_others`: un paso que no hace `x` (ni mata a `x`) nunca baja la cuota
  disponible de `x` ni la saca de las vivas.
* `no_starvation`: haga lo que haga el resto (cualquier secuencia de pasos
  ajenos), `x` conserva al menos la cuota disponible que tenía y la puede
  usar entera. Una cell ya no puede dejar sin memoria a otra.

## Abstracciones

* La memoria se cuenta en marcos; no se modela qué marco es cuál (eso lo
  verifica Kani sobre `kcore::frame_bitmap`). El kernel lleva dos recursos
  con estas reglas: los marcos y la memoria del kernel (bytes del heap que
  gasta por cada cell). Cada uno es una instancia de este modelo, con los
  mismos pasos sobre el mismo árbol de supervisores (`cell_spawn` reparte
  los dos en proporción fija), así que los teoremas valen para cada uno.
* Solo muere una cell sin hijas vivas; matar un subárbol es matar sus
  hojas una por una.
* Quién puede pedir cada paso (capacidades `Cell` con `CONTROL`, objetos
  `Memory`) lo dan la CSpace y `System.lean`; aquí el actor de cada paso es
  la cell que paga o recibe la cuota.
* La memoria del propio kernel (pilas, colas, la CSpace) no se cuenta; queda
  como hueco en `amenazas.md`.

## Mutaciones que rompen la demostración

Cada una hace fallar `step_inv`: aceptar un pedido que se pasa por un
marco de la cuota disponible, crear una hija con una cuota que se pasa por
un marco de la del supervisor, y no pasarle al supervisor los marcos usados
de la hija cuando muere.
-/

namespace Nsos.QuotaModel

/-! ## Sumas sobre listas sin repetidos -/

/-- `f` con el valor `v` en `x`. -/
def upd {α : Type} (f : Nat → α) (x : Nat) (v : α) : Nat → α :=
  fun y => if y = x then v else f y

/-- Suma de `f` sobre `l`. -/
def sumOn (l : List Nat) (f : Nat → Nat) : Nat := (l.map f).sum

theorem sumOn_congr {l : List Nat} {f g : Nat → Nat} (h : ∀ y ∈ l, g y = f y) :
    sumOn l g = sumOn l f := by
  induction l with
  | nil => rfl
  | cons a t ih =>
    simp only [sumOn, List.map_cons, List.sum_cons] at *
    rw [h a (by simp), ih (fun y hy => h y (by simp [hy]))]

/-- Cambiar `f` en un punto que no está en la lista no cambia la suma. -/
theorem sumOn_upd_out {l : List Nat} {f : Nat → Nat} {x v : Nat} (hx : x ∉ l) :
    sumOn l (upd f x v) = sumOn l f :=
  sumOn_congr fun y hy => by
    have : y ≠ x := fun e => hx (e ▸ hy)
    simp [upd, this]

/-- Cambiar `f` en un punto de la lista cambia la suma en la diferencia. -/
theorem sumOn_upd {l : List Nat} {f : Nat → Nat} {x v : Nat} (hnd : l.Nodup) (hx : x ∈ l) :
    sumOn l (upd f x v) + f x = sumOn l f + v := by
  induction l with
  | nil => cases hx
  | cons a t ih =>
    rw [List.nodup_cons] at hnd
    simp only [sumOn, List.map_cons, List.sum_cons] at *
    by_cases hxa : x = a
    · subst hxa
      have := sumOn_upd_out (f := f) (v := v) hnd.1
      simp only [sumOn] at this
      simp only [upd, this, ite_true]
      omega
    · have hxt : x ∈ t := by simpa [hxa] using hx
      have h1 := ih hnd.2 hxt
      have hax : a ≠ x := fun e => hxa e.symm
      simp only [upd, hax, ite_false] at h1 ⊢
      omega

theorem sumOn_cons {a : Nat} {l : List Nat} {f : Nat → Nat} :
    sumOn (a :: l) f = f a + sumOn l f := by
  simp [sumOn]

theorem le_sumOn {l : List Nat} {f : Nat → Nat} {x : Nat} (hx : x ∈ l) : f x ≤ sumOn l f := by
  induction l with
  | nil => cases hx
  | cons a t ih =>
    simp only [sumOn, List.map_cons, List.sum_cons] at *
    rcases List.mem_cons.mp hx with h | h
    · subst h; omega
    · have := ih h; omega

theorem sumOn_erase {l : List Nat} {f : Nat → Nat} {d : Nat} (hnd : l.Nodup) (hd : d ∈ l) :
    sumOn l f = sumOn (l.erase d) f + f d := by
  induction l with
  | nil => cases hd
  | cons a t ih =>
    rw [List.nodup_cons] at hnd
    by_cases hda : a = d
    · subst hda
      simp [sumOn, List.erase_cons_head]
      omega
    · have hdt : d ∈ t := by
        rcases List.mem_cons.mp hd with h | h
        · exact absurd h.symm hda
        · exact h
      rw [List.erase_cons_tail (by simpa using hda)]
      have := ih hnd.2 hdt
      simp only [sumOn, List.map_cons, List.sum_cons] at *
      omega

theorem nodup_erase {l : List Nat} {d : Nat} (h : l.Nodup) : (l.erase d).Nodup :=
  List.Nodup.sublist (List.erase_sublist) h

theorem mem_erase_of_ne' {l : List Nat} {x d : Nat} (hne : x ≠ d) (hx : x ∈ l) :
    x ∈ l.erase d :=
  (List.mem_erase_of_ne hne).mpr hx

/-! ## Estado y pasos -/

structure St where
  /-- Cells vivas (sin repetidos). -/
  cells : List Nat
  /-- Supervisor de cada cell. -/
  parent : Nat → Option Nat
  /-- Cuota disponible: marcos que la cell todavía puede usar. -/
  avail : Nat → Nat
  /-- Marcos que la cell usa. -/
  used : Nat → Nat
  /-- Marcos físicos libres. -/
  free : Nat
  /-- Marcos que gestiona el kernel para las cells (constante). -/
  total : Nat

inductive Ev where
  /-- `c` pide `n` marcos. -/
  | alloc (c n : Nat)
  /-- `c` suelta `n` marcos. -/
  | release (c n : Nat)
  /-- `p` crea la cell `d` con cuota `q`. -/
  | spawn (p d q : Nat)
  /-- Termina `d`, hija de `p`. -/
  | kill (p d : Nat)

/-- La cell que paga o recibe en cada paso. -/
def Ev.actor : Ev → Nat
  | .alloc c _ => c
  | .release c _ => c
  | .spawn p _ _ => p
  | .kill p _ => p

def St.step (s : St) : Ev → Option St
  | .alloc c n =>
    if c ∈ s.cells ∧ n ≤ s.avail c then
      some { s with avail := upd s.avail c (s.avail c - n),
                    used := upd s.used c (s.used c + n),
                    free := s.free - n }
    else none
  | .release c n =>
    if c ∈ s.cells ∧ n ≤ s.used c then
      some { s with avail := upd s.avail c (s.avail c + n),
                    used := upd s.used c (s.used c - n),
                    free := s.free + n }
    else none
  | .spawn p d q =>
    if p ∈ s.cells ∧ d ∉ s.cells ∧ q ≤ s.avail p then
      some { s with cells := d :: s.cells,
                    parent := upd s.parent d (some p),
                    avail := upd (upd s.avail p (s.avail p - q)) d q,
                    used := upd s.used d 0 }
    else none
  | .kill p d =>
    if p ∈ s.cells ∧ d ∈ s.cells ∧ p ≠ d ∧ s.parent d = some p ∧
        (∀ c ∈ s.cells, s.parent c ≠ some d) then
      some { s with cells := s.cells.erase d,
                    avail := upd s.avail p (s.avail p + s.avail d),
                    used := upd s.used p (s.used p + s.used d) }
    else none

/-- El invariante: cells sin repetidos, los marcos libres son la suma de las
cuotas disponibles, y libres más usados es el total. -/
def St.Inv (s : St) : Prop :=
  s.cells.Nodup ∧ s.free = sumOn s.cells s.avail ∧ s.free + sumOn s.cells s.used = s.total

/-! ## El invariante se mantiene -/

theorem step_inv {s s' : St} {e : Ev} (h : s.Inv) (hs : s.step e = some s') : s'.Inv := by
  obtain ⟨hnd, hfree, htot⟩ := h
  cases e with
  | alloc c n =>
    simp only [St.step] at hs
    split at hs
    · rename_i hc
      cases hs
      have ha := sumOn_upd (f := s.avail) (v := s.avail c - n) hnd hc.1
      have hu := sumOn_upd (f := s.used) (v := s.used c + n) hnd hc.1
      have hle := le_sumOn (f := s.avail) hc.1
      refine ⟨hnd, ?_, ?_⟩ <;> simp only <;> omega
    · cases hs
  | release c n =>
    simp only [St.step] at hs
    split at hs
    · rename_i hc
      cases hs
      have ha := sumOn_upd (f := s.avail) (v := s.avail c + n) hnd hc.1
      have hu := sumOn_upd (f := s.used) (v := s.used c - n) hnd hc.1
      have hle := le_sumOn (f := s.used) hc.1
      refine ⟨hnd, ?_, ?_⟩ <;> simp only <;> omega
    · cases hs
  | spawn p d q =>
    simp only [St.step] at hs
    split at hs
    · rename_i hc
      obtain ⟨hp, hd, hq⟩ := hc
      cases hs
      have hpd : p ≠ d := fun e => hd (e ▸ hp)
      -- Sobre las cells viejas, el cambio en `d` no se ve.
      have h1 : sumOn s.cells (upd (upd s.avail p (s.avail p - q)) d q) =
          sumOn s.cells (upd s.avail p (s.avail p - q)) := sumOn_upd_out hd
      have h2 := sumOn_upd (f := s.avail) (v := s.avail p - q) hnd hp
      have h3 : sumOn s.cells (upd s.used d 0) = sumOn s.cells s.used := sumOn_upd_out hd
      have hvd : upd (upd s.avail p (s.avail p - q)) d q d = q := by simp [upd]
      have hud : upd s.used d 0 d = 0 := by simp [upd]
      refine ⟨List.nodup_cons.mpr ⟨hd, hnd⟩, ?_, ?_⟩
      · show s.free = sumOn (d :: s.cells) _
        rw [sumOn_cons, h1]; simp only [hvd]; omega
      · show s.free + sumOn (d :: s.cells) _ = s.total
        rw [sumOn_cons, h3]; simp only [hud]; omega
    · cases hs
  | kill p d =>
    simp only [St.step] at hs
    split at hs
    · rename_i hc
      obtain ⟨hp, hd, hpd, _, _⟩ := hc
      cases hs
      have hnd' := nodup_erase (d := d) hnd
      have hp' := mem_erase_of_ne' hpd hp
      have e1 := sumOn_erase (f := s.avail) hnd hd
      have e2 := sumOn_erase (f := s.used) hnd hd
      have h2 := sumOn_upd (f := s.avail) (v := s.avail p + s.avail d) hnd' hp'
      have h3 := sumOn_upd (f := s.used) (v := s.used p + s.used d) hnd' hp'
      refine ⟨hnd', ?_, ?_⟩ <;> simp only <;> omega
    · cases hs

/-! ## Consecuencias -/

/-- La cuota disponible de una cell viva está respaldada por marcos libres. -/
theorem avail_le_free {s : St} (h : s.Inv) {c : Nat} (hc : c ∈ s.cells) : s.avail c ≤ s.free := by
  rw [h.2.1]; exact le_sumOn hc

/-- Pedir hasta la cuota disponible siempre funciona, y hay marcos libres
para darlos. -/
theorem alloc_ok {s : St} (h : s.Inv) {c n : Nat} (hc : c ∈ s.cells) (hn : n ≤ s.avail c) :
    (s.step (.alloc c n)).isSome ∧ n ≤ s.free := by
  refine ⟨?_, Nat.le_trans hn (avail_le_free h hc)⟩
  simp [St.step, hc, hn]

/-- Un paso que no hace `x` ni mata a `x` no baja la cuota disponible de `x`
ni la saca de las vivas. -/
theorem step_others {s s' : St} {e : Ev} {x : Nat} (hs : s.step e = some s') (hx : x ∈ s.cells)
    (hact : e.actor ≠ x) (hkill : ∀ p, e ≠ .kill p x) :
    x ∈ s'.cells ∧ s.avail x ≤ s'.avail x := by
  cases e with
  | alloc c n =>
    simp only [St.step] at hs
    split at hs
    · cases hs
      have : x ≠ c := fun e => hact (by simp [Ev.actor, e])
      exact ⟨hx, by simp [upd, this]⟩
    · cases hs
  | release c n =>
    simp only [St.step] at hs
    split at hs
    · cases hs
      have : x ≠ c := fun e => hact (by simp [Ev.actor, e])
      exact ⟨hx, by simp [upd, this]⟩
    · cases hs
  | spawn p d q =>
    simp only [St.step] at hs
    split at hs
    · rename_i hc
      cases hs
      have hxp : x ≠ p := fun e => hact (by simp [Ev.actor, e])
      have hxd : x ≠ d := fun e => hc.2.1 (e ▸ hx)
      exact ⟨List.mem_cons_of_mem _ hx, by simp [upd, hxp, hxd]⟩
    · cases hs
  | kill p d =>
    simp only [St.step] at hs
    split at hs
    · cases hs
      have hxp : x ≠ p := fun e => hact (by simp [Ev.actor, e])
      have hxd : x ≠ d := fun e => hkill p (by rw [e])
      exact ⟨mem_erase_of_ne' hxd hx, by simp [upd, hxp]⟩
    · cases hs

/-! ## Ejecuciones -/

inductive Run : St → List Ev → St → Prop
  | nil (s : St) : Run s [] s
  | cons {s s' s'' : St} {e : Ev} {es : List Ev} :
      s.step e = some s' → Run s' es s'' → Run s (e :: es) s''

theorem run_inv {s s' : St} {es : List Ev} (hr : Run s es s') (h : s.Inv) : s'.Inv := by
  induction hr with
  | nil => exact h
  | cons hs _ ih => exact ih (step_inv h hs)

/-- **Sin inanición**: si ninguno de los pasos lo hace `x` ni mata a `x`,
al final `x` sigue viva, con al menos la cuota disponible que tenía, y la
puede pedir entera. Ninguna cell deja sin memoria a otra. -/
theorem no_starvation {s s' : St} {es : List Ev} {x : Nat} (hr : Run s es s') (h : s.Inv)
    (hx : x ∈ s.cells) (hes : ∀ e ∈ es, e.actor ≠ x ∧ ∀ p, e ≠ .kill p x) :
    x ∈ s'.cells ∧ s.avail x ≤ s'.avail x ∧
      ∀ n ≤ s.avail x, (s'.step (.alloc x n)).isSome := by
  have key : x ∈ s'.cells ∧ s.avail x ≤ s'.avail x := by
    induction hr with
    | nil => exact ⟨hx, Nat.le_refl _⟩
    | cons hs _ ih =>
      have ⟨a, b⟩ := hes _ (List.mem_cons_self ..)
      have ⟨hx1, hle1⟩ := step_others hs hx a b
      have ⟨hx2, hle2⟩ := ih (step_inv h hs) hx1 (fun e he => hes e (List.mem_cons_of_mem _ he))
      exact ⟨hx2, Nat.le_trans hle1 hle2⟩
  refine ⟨key.1, key.2, fun n hn => (alloc_ok (run_inv hr h) key.1 (Nat.le_trans hn key.2)).1⟩

/-- Al arrancar, una sola cell (`init`) con toda la memoria. -/
def init (total : Nat) : St :=
  { cells := [0], parent := fun _ => none, avail := fun c => if c = 0 then total else 0,
    used := fun _ => 0, free := total, total }

theorem inv_init (total : Nat) : (init total).Inv := by
  refine ⟨by simp [init], ?_, ?_⟩ <;> simp [init, sumOn]

theorem step_total {s s' : St} {e : Ev} (hs : s.step e = some s') : s'.total = s.total := by
  cases e <;> simp only [St.step] at hs <;> split at hs <;> cases hs <;> rfl

theorem run_total {s s' : St} {es : List Ev} (hr : Run s es s') : s'.total = s.total := by
  induction hr with
  | nil => rfl
  | cons hs _ ih => rw [ih, step_total hs]

/-- Desde el arranque, en toda ejecución, ningún marco se pierde. -/
theorem run_conserves {total : Nat} {s : St} {es : List Ev} (hr : Run (init total) es s) :
    s.free + sumOn s.cells s.used = total := by
  rw [(run_inv hr (inv_init total)).2.2, run_total hr]; rfl

end Nsos.QuotaModel
