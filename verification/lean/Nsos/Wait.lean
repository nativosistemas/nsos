/-!
# Espera sobre varios handles (`wait`)

Modelo de la syscall `wait(items, n, timeout)` del hito 12. Se escribió antes
que el código del kernel (`kernel/src/syscall.rs`, `wait`).

Un hilo pide esperar a que se active alguna señal de una lista de ítems
`(objeto, señales pedidas)`. El kernel **no** lo hace de un solo paso: marca
al hilo bloqueado y después recorre los ítems uno por uno, cada uno bajo el
lock de su objeto, registrando al hilo en la lista de espera de cada señal
pedida y mirando si ya está activa (si lo está, se despierta a sí mismo).
Recién al final cede la CPU. Mientras tanto otros hilos (o el entorno: un
`send`, un cierre, el fin de una cell, una interrupción) activan y apagan
señales, y quien activa una señal despierta a todos los registrados en ella.

## Pasos del modelo y su código

| Paso | Código |
|---|---|
| `start` | `sched::block_current()` al entrar en `wait` |
| `scan` | registrar y mirar un ítem bajo el lock del objeto (`Endpoint::watch`, `Cell::watch_exit`) |
| `finish` | terminar el recorrido y ceder la CPU (`arch::yield_now`) |
| `resume` | volver de `yield_now` ya despierto |
| `raise` | `send` (`READABLE`), `recv` (`WRITABLE` del peer), cierre (`PEER_CLOSED`), `terminate` (`CELL_EXITED`): `wake_all` de la lista de la señal |
| `lower` | `recv` vacía la cola, `send` la llena |
| `wake` | despertares que no vienen de una señal: `cell_kill`, el plazo vencido, despertares espurios |

## Qué se demuestra

* `step_inv`: el invariante `Inv` se mantiene en todo paso, en cualquier
  intercalado. Dice que un hilo que sigue bloqueado está registrado en cada
  señal pedida de los ítems que ya recorrió, y que ninguna de ellas está
  activa.
* `no_lost_wakeup`: **sin despertares perdidos**: si un hilo duerme en
  `wait` y alguno de sus ítems tiene una señal pedida activa, el hilo ya
  está despierto (no puede quedarse dormido con un evento que le interesa).
* `run_inv`: lo anterior vale en toda ejecución desde un sistema sin nadie
  esperando.

## Abstracciones

* Una señal es un booleano por `(objeto, señal)`; cómo se calcula a partir
  de la cola de un canal o del estado de una cell lo cubre Kani sobre
  `nsos_kcore::wait`.
* Las listas de espera pueden tener repetidos y quedar con registros viejos
  después de que el hilo volvió: el modelo los permite (un despertar de más
  es inofensivo porque todo bucle de espera vuelve a mirar).
* No modela qué devuelve `wait` (eso es `nsos_kcore::wait::Observed`), solo
  si el hilo puede quedar dormido.

## Mutaciones que rompen la demostración

Ver `docs/hoja-de-ruta.md` (hito 12): marcar bloqueado al final del
recorrido en vez de al principio, que `raise` despierte solo al primero de
la lista, que `scan` no se despierte a sí mismo si el ítem ya está listo y
que `scan` registre solo la primera señal pedida.
-/

namespace Nsos.WaitModel

abbrev Tid := Nat
abbrev Obj := Nat
abbrev Sig := Nat

/-- Un ítem de `wait`: un objeto y las señales que interesan. -/
structure Item where
  obj : Obj
  wanted : List Sig

/-- En qué parte de `wait` está un hilo. -/
inductive Phase where
  | idle
  /-- Marcado bloqueado; ya recorrió los primeros `k` ítems. -/
  | scanning (items : List Item) (k : Nat)
  /-- Recorrió todo y cedió la CPU. -/
  | sleeping (items : List Item)

structure St where
  active : Obj → Sig → Bool
  waiters : Obj → Sig → List Tid
  /-- El scheduler lo puede correr (no está `Blocked`). -/
  runnable : Tid → Bool
  phase : Tid → Phase

/-- Alguna señal pedida del ítem está activa. -/
def St.ready (s : St) (it : Item) : Bool := it.wanted.any (s.active it.obj)

inductive Ev where
  | start (t : Tid) (items : List Item)
  | scan (t : Tid)
  | finish (t : Tid)
  | resume (t : Tid)
  | raise (o : Obj) (g : Sig)
  | lower (o : Obj) (g : Sig)
  | wake (t : Tid)

/-- Registrar a `t` en cada señal pedida de `it`. -/
def reg (w : Obj → Sig → List Tid) (t : Tid) (it : Item) : Obj → Sig → List Tid :=
  fun o g => if o = it.obj ∧ g ∈ it.wanted then t :: w o g else w o g

def St.step (s : St) : Ev → Option St
  | .start t items =>
    match s.phase t with
    | .idle => some { s with
        phase := fun u => if u = t then .scanning items 0 else s.phase u
        runnable := fun u => if u = t then false else s.runnable u }
    | _ => none
  | .scan t =>
    match s.phase t with
    | .scanning items k =>
      if h : k < items.length then
        let it := items[k]
        some { s with
          waiters := reg s.waiters t it
          runnable := fun u => if u = t then s.runnable t || s.ready it else s.runnable u
          phase := fun u => if u = t then .scanning items (k + 1) else s.phase u }
      else none
    | _ => none
  | .finish t =>
    match s.phase t with
    | .scanning items k =>
      if items.length ≤ k then
        some { s with phase := fun u => if u = t then .sleeping items else s.phase u }
      else none
    | _ => none
  | .resume t =>
    match s.phase t with
    | .sleeping _ =>
      if s.runnable t then
        some { s with phase := fun u => if u = t then .idle else s.phase u }
      else none
    | _ => none
  | .raise o g => some { s with
      active := fun o' g' => if o' = o ∧ g' = g then true else s.active o' g'
      waiters := fun o' g' => if o' = o ∧ g' = g then [] else s.waiters o' g'
      runnable := fun u => s.runnable u || decide (u ∈ s.waiters o g) }
  | .lower o g => some { s with
      active := fun o' g' => if o' = o ∧ g' = g then false else s.active o' g' }
  | .wake t => some { s with runnable := fun u => if u = t then true else s.runnable u }

/-- `t` está registrado en todas las señales pedidas de `it` y ninguna está
activa. -/
def St.Covered (s : St) (t : Tid) (it : Item) : Prop :=
  s.ready it = false ∧ ∀ g ∈ it.wanted, t ∈ s.waiters it.obj g

/-- Invariante: todo hilo bloqueado cubre los ítems que ya recorrió. -/
def St.Inv (s : St) : Prop :=
  ∀ t, s.runnable t = false →
    (∀ items k, s.phase t = .scanning items k → ∀ it ∈ items.take k, s.Covered t it) ∧
    (∀ items, s.phase t = .sleeping items → ∀ it ∈ items, s.Covered t it)

theorem ready_false_iff {s : St} {it : Item} :
    s.ready it = false ↔ ∀ g ∈ it.wanted, s.active it.obj g = false := by
  simp [St.ready, List.any_eq_false]

/-- Agregar registros no descubre nada. -/
theorem covered_reg {s : St} {t u : Tid} {it it' : Item} (h : s.Covered u it') :
    St.Covered { s with waiters := reg s.waiters t it } u it' := by
  refine ⟨h.1, fun g hg => ?_⟩
  simp only [reg]
  split
  · exact List.mem_cons_of_mem _ (h.2 g hg)
  · exact h.2 g hg

theorem step_inv {s s' : St} {e : Ev} (h : s.Inv) (hs : s.step e = some s') : s'.Inv := by
  cases e with
  | start t items =>
    simp only [St.step] at hs
    split at hs
    · rename_i hp
      cases hs
      intro u hu
      by_cases hut : u = t
      · subst hut
        refine ⟨fun its k hph it hit => ?_, fun its hph => ?_⟩
        · simp at hph; obtain ⟨rfl, rfl⟩ := hph; simp at hit
        · simp at hph
      · simp only [hut, ite_false] at hu ⊢
        exact h u hu
    · cases hs
  | scan t =>
    simp only [St.step] at hs
    split at hs
    · rename_i items k hp
      split at hs
      · rename_i hk
        cases hs
        intro u hu
        by_cases hut : u = t
        · subst hut
          simp only [ite_true, Bool.or_eq_false_iff] at hu
          obtain ⟨hr, hready⟩ := hu
          refine ⟨fun its k' hph it hit => ?_, fun its hph => ?_⟩
          · simp only [ite_true, Phase.scanning.injEq] at hph
            obtain ⟨rfl, rfl⟩ := hph
            rw [List.take_succ_eq_append_getElem hk] at hit
            rcases List.mem_append.1 hit with hit | hit
            · exact covered_reg ((h u hr).1 items k hp it hit)
            · simp only [List.mem_singleton] at hit
              subst hit
              refine ⟨hready, fun g hg => ?_⟩
              simp [reg, hg]
          · simp at hph
        · simp only [hut, ite_false] at hu ⊢
          obtain ⟨h1, h2⟩ := h u hu
          exact ⟨fun its k' hph it hit => covered_reg (h1 its k' hph it hit),
            fun its hph it hit => covered_reg (h2 its hph it hit)⟩
      · cases hs
    · cases hs
  | finish t =>
    simp only [St.step] at hs
    split at hs
    · rename_i items k hp
      split at hs
      · rename_i hk
        cases hs
        intro u hu
        by_cases hut : u = t
        · subst hut
          refine ⟨fun its k' hph => by simp at hph, fun its hph it hit => ?_⟩
          simp only [ite_true, Phase.sleeping.injEq] at hph
          subst hph
          apply (h u hu).1 _ k hp
          rwa [List.take_of_length_le hk]
        · simp only [hut, ite_false] at hu ⊢
          exact h u hu
      · cases hs
    · cases hs
  | resume t =>
    simp only [St.step] at hs
    split at hs
    · rename_i items hp
      split at hs
      · rename_i hr
        cases hs
        intro u hu
        by_cases hut : u = t
        · subst hut
          simp only at hu
          rw [hr] at hu
          cases hu
        · simp only [hut, ite_false] at hu ⊢
          exact h u hu
      · cases hs
    · cases hs
  | raise o g =>
    simp only [St.step] at hs
    cases hs
    intro u hu
    simp only [Bool.or_eq_false_iff, decide_eq_false_iff_not] at hu
    obtain ⟨hr, hnw⟩ := hu
    -- Un ítem cubierto no puede pedir `(o, g)`: `u` estaría registrado ahí.
    have keep : ∀ it, s.Covered u it → St.Covered { s with
        active := fun o' g' => if o' = o ∧ g' = g then true else s.active o' g'
        waiters := fun o' g' => if o' = o ∧ g' = g then [] else s.waiters o' g'
        runnable := fun u => s.runnable u || decide (u ∈ s.waiters o g) } u it := by
      intro it ⟨hrd, hw⟩
      have hno : ∀ g' ∈ it.wanted, ¬ (it.obj = o ∧ g' = g) := by
        rintro g' hg' ⟨rfl, rfl⟩
        exact hnw (hw g' hg')
      refine ⟨?_, fun g' hg' => ?_⟩
      · rw [ready_false_iff] at hrd ⊢
        intro g' hg'
        simp only [hno g' hg', ite_false]
        exact hrd g' hg'
      · simp only [hno g' hg', ite_false]
        exact hw g' hg'
    obtain ⟨h1, h2⟩ := h u hr
    exact ⟨fun its k hph it hit => keep it (h1 its k hph it hit),
      fun its hph it hit => keep it (h2 its hph it hit)⟩
  | lower o g =>
    simp only [St.step] at hs
    cases hs
    intro u hu
    have keep : ∀ it, s.Covered u it → St.Covered { s with
        active := fun o' g' => if o' = o ∧ g' = g then false else s.active o' g' } u it := by
      intro it ⟨hrd, hw⟩
      refine ⟨?_, hw⟩
      rw [ready_false_iff] at hrd ⊢
      intro g' hg'
      dsimp only
      split
      · rfl
      · exact hrd g' hg'
    obtain ⟨h1, h2⟩ := h u hu
    exact ⟨fun its k hph it hit => keep it (h1 its k hph it hit),
      fun its hph it hit => keep it (h2 its hph it hit)⟩
  | wake t =>
    simp only [St.step] at hs
    cases hs
    intro u hu
    by_cases hut : u = t
    · subst hut; simp at hu
    · simp only [hut, ite_false] at hu
      exact h u hu

/-- **Sin despertares perdidos**: un hilo dormido en `wait` con un ítem listo
ya está despierto. -/
theorem no_lost_wakeup {s : St} (h : s.Inv) {t : Tid} {items : List Item} {it : Item}
    (hp : s.phase t = .sleeping items) (hit : it ∈ items) (hr : s.ready it = true) :
    s.runnable t = true := by
  cases hb : s.runnable t
  · have := ((h t hb).2 items hp it hit).1
    rw [hr] at this
    cases this
  · rfl

/-- Ejecuciones: secuencias de pasos habilitados. -/
inductive Run : St → List Ev → St → Prop
  | nil (s : St) : Run s [] s
  | cons {s s' s'' : St} {e : Ev} {es : List Ev} :
      s.step e = some s' → Run s' es s'' → Run s (e :: es) s''

/-- Nadie esperando al arrancar. -/
theorem inv_init {s : St} (h : ∀ t, s.phase t = .idle) : s.Inv := by
  intro t _
  constructor
  · intro items k hp; rw [h t] at hp; cases hp
  · intro items hp; rw [h t] at hp; cases hp

theorem run_inv {s s' : St} {es : List Ev} (hr : Run s es s') (h : s.Inv) : s'.Inv := by
  induction hr with
  | nil => exact h
  | cons hs _ ih => exact ih (step_inv h hs)

/-- En toda ejecución desde un sistema sin nadie esperando, ningún hilo se
queda dormido con un evento que le interesa. -/
theorem run_no_lost_wakeup {s s' : St} {es : List Ev} (h0 : ∀ t, s.phase t = .idle)
    (hr : Run s es s') {t : Tid} {items : List Item} {it : Item}
    (hp : s'.phase t = .sleeping items) (hit : it ∈ items) (hrd : s'.ready it = true) :
    s'.runnable t = true :=
  no_lost_wakeup (run_inv hr (inv_init h0)) hp hit hrd

end Nsos.WaitModel
