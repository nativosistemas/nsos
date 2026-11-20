/-!
# Almacenamiento de objetos: consistencia ante caídas y aislamiento entre dueños

Modelo de la lógica de `crates/store` (`nsos-store`), que usa la cell
`almacen` sobre el driver `virtblk`. El modelo se escribió antes que el
código.

## Parte A: copy-on-write y caídas

El disco es una función de direcciones de bloque a bloques. Los bloques 0 y
1 son las dos ranuras de superbloque; el resto guarda tablas (catálogos) y
datos de objetos. Un superbloque válido dice: número de secuencia, dónde está
la tabla y *qué* tiene que haber en ella (en el código, un SHA-256; aquí, el
contenido mismo: un hash ideal sin colisiones). Cada entrada de la tabla
dice dónde están los datos del objeto y qué tienen que contener.

Escribir un objeto (`Store::put` en el código) nunca toca lo que el estado
confirmado usa:
1. los datos nuevos van a bloques libres (`Fresh`);
2. la tabla nueva va a otro bloque libre;
3. el superbloque nuevo, con secuencia + 1, va a la *otra* ranura.

Una caída puede pasar en cualquier momento, y el disco puede reordenar las
escrituras: cada una de las tres puede no haber ocurrido, haber ocurrido
entera o haber quedado a medias (basura, que el hash detecta). Al montar se
elige la ranura válida de mayor secuencia, donde "válida" exige que la tabla
y todos los datos coincidan con lo que el superbloque promete (`TreeOk`).

Teoremas:
* `Store.crash_safe`: después de una caída en cualquier punto de una
  escritura, montar devuelve el catálogo viejo o el nuevo, nunca una mezcla.
* `Store.commit_good`: si la escritura termina, el catálogo nuevo queda
  confirmado y el invariante (`Good`) vale para la escritura siguiente.
* `Store.mount_good`: montar cualquier disco que tenga una ranura válida deja
  el invariante, gracias a la limpieza de la otra ranura (si tiene un
  superbloque de secuencia igual o mayor que no valida, se borra: si no, una
  escritura posterior podría completar su árbol y "resucitar" un catálogo que
  nunca se confirmó).
* `Store.crash_then_mount`: juntando todo, caída + montaje deja el viejo o el
  nuevo, y se puede seguir escribiendo.
* `Store.format_good`: un disco recién formateado cumple el invariante.

Por qué hace falta cada regla (ejemplos concretos, evaluados con `decide`):
`overwrite_breaks` (datos sobre un bloque confirmado), `same_slot_breaks`
(superbloque en la ranura actual), `same_seq_loses` (sin subir la secuencia)
y `stale_resurrects` (montar sin limpiar la otra ranura).

Abstracciones:
* Un objeto ocupa un "bloque" de datos. En el código es un rango contiguo de
  bloques con un solo hash: si una parte quedó vieja o a medias, el hash no
  coincide, igual que un bloque basura aquí.
* El hash no tiene colisiones (SHA-256). Un atacante que puede escribir el
  disco a voluntad queda fuera del modelo (ver `docs/amenazas.md`).
* La secuencia no da la vuelta (64 bits).

## Parte B: aislamiento entre dueños

La cell `almacen` recibe del supervisor un canal "raíz" por dueño. Un
cliente abre un objeto por la raíz y recibe un *handle*: un extremo de canal
nuevo, que es una capacidad a ese objeto y nada más. Se puede pedir una
copia de solo lectura de un handle (`DUP_RO`) y pasarla a otra cell. El
almacén no sabe quién usa un handle: tenerlo es la autoridad (el kernel
garantiza que no se falsifican: `Sys.no_forging`).

Teoremas:
* `Store.rw_handle_from_owner`: todo handle escribible a un objeto del dueño
  `o` lo abrió, con escritura, la raíz de `o`. `DUP_RO` nunca da escritura.
* `Store.change_needs_owner`: si cambia un objeto de `o`, fue por una
  escritura con un handle escribible que abrió la raíz de `o`.
* `Store.untouched_without_owner`: si la raíz de `o` nunca abrió el objeto
  con escritura, nadie lo cambió (ni otro dueño, ni quien tenga copias de
  solo lectura).
-/

namespace Nsos
namespace Store

/-- Entrada del catálogo: objeto `name` del dueño `owner`. -/
structure Entry where
  owner : Nat
  name : Nat
  version : Nat
  addr : Nat
  data : List Nat
  deriving DecidableEq, Repr

/-- Contenido de un bloque. `junk` es cualquier cosa que no pasa el hash
(nunca escrito, escrito a medias, ajeno). -/
inductive Block where
  | junk
  | data (d : List Nat)
  | table (es : List Entry)
  | super (seq : Nat) (t : Nat) (es : List Entry)
  deriving DecidableEq, Repr

abbrev Disk := Nat → Block

/-- Escribe `b` en `a`. -/
def upd (d : Disk) (a : Nat) (b : Block) : Disk := fun x => if x = a then b else d x

/-- La tabla y los datos que promete un superbloque están en el disco, y no
apuntan a las ranuras de superbloque (el decodificador del código rechaza
esos punteros). -/
def TreeOk (d : Disk) (t : Nat) (es : List Entry) : Prop :=
  2 ≤ t ∧ d t = .table es ∧ ∀ e ∈ es, 2 ≤ e.addr ∧ d e.addr = .data e.data

instance (d : Disk) (t : Nat) (es : List Entry) : Decidable (TreeOk d t es) := by
  unfold TreeOk; infer_instance

/-- Lo que se lee de la ranura `i`: secuencia, tabla y catálogo, si valida. -/
def view (d : Disk) (i : Nat) : Option (Nat × Nat × List Entry) :=
  match d i with
  | .super s t es => if TreeOk d t es then some (s, t, es) else none
  | _ => none

/-- Recuperación: la ranura válida de mayor secuencia. -/
def recover (d : Disk) : Option (Nat × Nat × Nat × List Entry) :=
  match view d 0, view d 1 with
  | some (s0, t0, e0), some (s1, t1, e1) =>
      if s0 < s1 then some (1, s1, t1, e1) else some (0, s0, t0, e0)
  | some (s0, t0, e0), none => some (0, s0, t0, e0)
  | none, some (s1, t1, e1) => some (1, s1, t1, e1)
  | none, none => none

/-- Montar: recuperar y borrar la otra ranura si tiene un superbloque de
secuencia igual o mayor (que entonces no valida). -/
def mount (d : Disk) : Option (Disk × Nat × Nat × Nat × List Entry) :=
  match recover d with
  | none => none
  | some (i, s, t, es) =>
      let d' := match d (1 - i) with
        | .super s' _ _ => if s ≤ s' then upd d (1 - i) .junk else d
        | _ => d
      some (d', i, s, t, es)

/-- Invariante entre escrituras: la ranura `i` tiene el estado confirmado
(secuencia `seq`, tabla en `t`, catálogo `es`) y la otra ranura no tiene un
superbloque de secuencia igual o mayor. -/
structure Good (d : Disk) (i seq t : Nat) (es : List Entry) : Prop where
  slot : i = 0 ∨ i = 1
  cur : d i = .super seq t es
  tree : TreeOk d t es
  other : ∀ s t' es', d (1 - i) = .super s t' es' → s < seq

/-- Un bloque libre para la escritura siguiente: no es una ranura, ni la
tabla confirmada, ni datos de un objeto confirmado. -/
def Fresh (t : Nat) (es : List Entry) (a : Nat) : Prop :=
  2 ≤ a ∧ a ≠ t ∧ ∀ e ∈ es, e.addr ≠ a

/-- ¿La entrada es el objeto `n` del dueño `o`? -/
def isKey (o n : Nat) (e : Entry) : Bool := e.owner == o && e.name == n

/-- Busca el objeto `n` del dueño `o`. -/
def find (es : List Entry) (o n : Nat) : Option Entry := es.find? (isKey o n)

/-- Versión que tendrá el objeto al escribirlo. -/
def nextVersion (es : List Entry) (o n : Nat) : Nat :=
  match find es o n with
  | some e => e.version + 1
  | none => 1

/-- Catálogo después de escribir `D` como objeto `n` de `o`, con los datos
en `a`. Los demás objetos no se tocan (siguen en sus bloques). -/
def put (es : List Entry) (o n : Nat) (D : List Nat) (a : Nat) : List Entry :=
  { owner := o, name := n, version := nextVersion es o n, addr := a, data := D }
    :: es.filter (fun e => !isKey o n e)

/-- Qué pasó con una escritura cuando se cayó la máquina. -/
inductive Out where
  | skip | full | torn

/-- Aplica una escritura según cómo terminó. -/
def apply (d : Disk) (a : Nat) (b : Block) : Out → Disk
  | .skip => d
  | .full => upd d a b
  | .torn => upd d a .junk

/-- Las tres escrituras de `put` (datos en `a`, tabla en `t'`, superbloque
en la otra ranura), cada una con su resultado. -/
def commit (d : Disk) (i seq : Nat) (es : List Entry) (o n : Nat) (D : List Nat)
    (a t' : Nat) (o1 o2 o3 : Out) : Disk :=
  apply (apply (apply d a (.data D) o1) t' (.table (put es o n D a)) o2)
    (1 - i) (.super (seq + 1) t' (put es o n D a)) o3

-- ------------------------------------------------------------- lemas

theorem apply_skip (d : Disk) (a : Nat) (b : Block) : apply d a b .skip = d := rfl
theorem apply_full (d : Disk) (a : Nat) (b : Block) : apply d a b .full = upd d a b := rfl
theorem apply_torn (d : Disk) (a : Nat) (b : Block) : apply d a b .torn = upd d a .junk := rfl

theorem upd_ne {d : Disk} {a x : Nat} (b : Block) (h : x ≠ a) : upd d a b x = d x := by
  simp [upd, h]

theorem upd_eq {d : Disk} {a : Nat} (b : Block) : upd d a b a = b := by
  simp [upd]

theorem apply_ne {d : Disk} {a x : Nat} (b : Block) (o : Out) (h : x ≠ a) :
    apply d a b o x = d x := by
  cases o <;> simp [apply, upd, h]

theorem view_some {d : Disk} {i s t} {es : List Entry} (h : view d i = some (s, t, es)) :
    d i = .super s t es ∧ TreeOk d t es := by
  unfold view at h
  split at h
  · rename_i s' t' es' hd
    by_cases ht : TreeOk d t' es'
    · simp [ht] at h
      obtain ⟨rfl, rfl, rfl⟩ := h
      exact ⟨hd, ht⟩
    · simp [ht] at h
  · simp at h

theorem view_of {d : Disk} {i s t} {es : List Entry} (hd : d i = .super s t es)
    (ht : TreeOk d t es) : view d i = some (s, t, es) := by
  unfold view
  rw [hd]
  simp [ht]

theorem view_none_of_not_super {d : Disk} {i} (h : ∀ s t es, d i ≠ .super s t es) :
    view d i = none := by
  unfold view
  split
  · rename_i s t es hd
    exact absurd hd (h s t es)
  · rfl

/-- Si la ranura `i` valida y la otra solo puede tener secuencias menores,
se recupera `i`. -/
theorem recover_cur {d : Disk} {i s t} {es : List Entry} (hi : i = 0 ∨ i = 1)
    (hv : view d i = some (s, t, es))
    (ho : ∀ s' t' e', view d (1 - i) = some (s', t', e') → s' < s) :
    recover d = some (i, s, t, es) := by
  unfold recover
  rcases hi with rfl | rfl
  · rw [hv]
    cases h1 : view d 1 with
    | none => rfl
    | some v =>
      obtain ⟨s1, t1, e1⟩ := v
      have := ho s1 t1 e1 h1
      simp [Nat.not_lt.mpr (Nat.le_of_lt this)]
  · rw [hv]
    cases h0 : view d 0 with
    | none => rfl
    | some v =>
      obtain ⟨s0, t0, e0⟩ := v
      have := ho s0 t0 e0 h0
      simp [this]

/-- Si la ranura `j` valida con secuencia mayor que la otra (que también
valida), se recupera `j`. -/
theorem recover_newer {d : Disk} {j s t s' t'} {es es' : List Entry} (hj : j = 0 ∨ j = 1)
    (hv : view d j = some (s, t, es)) (hv' : view d (1 - j) = some (s', t', es'))
    (hlt : s' < s) : recover d = some (j, s, t, es) :=
  recover_cur hj hv (fun a b c h => by rw [hv'] at h; cases h; exact hlt)

theorem recover_slot {d : Disk} {i s t} {es : List Entry} (h : recover d = some (i, s, t, es)) :
    (i = 0 ∨ i = 1) ∧ view d i = some (s, t, es) := by
  unfold recover at h
  split at h
  · rename_i s0 t0 e0 s1 t1 e1 h0 h1
    by_cases hl : s0 < s1
    · simp [hl] at h
      obtain ⟨rfl, rfl, rfl, rfl⟩ := h
      exact ⟨Or.inr rfl, h1⟩
    · simp [hl] at h
      obtain ⟨rfl, rfl, rfl, rfl⟩ := h
      exact ⟨Or.inl rfl, h0⟩
  · rename_i s0 t0 e0 h0 _
    simp at h
    obtain ⟨rfl, rfl, rfl, rfl⟩ := h
    exact ⟨Or.inl rfl, h0⟩
  · rename_i _ s1 t1 e1 h1
    simp at h
    obtain ⟨rfl, rfl, rfl, rfl⟩ := h
    exact ⟨Or.inr rfl, h1⟩
  · simp at h

theorem recover_max {d : Disk} {i s t} {es : List Entry} (h : recover d = some (i, s, t, es)) :
    ∀ s' t' e', view d (1 - i) = some (s', t', e') → s' ≤ s := by
  intro s' t' e' hv
  unfold recover at h
  cases h0 : view d 0 with
  | none =>
    cases h1 : view d 1 with
    | none => simp [h0, h1] at h
    | some v1 =>
      obtain ⟨s1, t1, e1⟩ := v1
      simp [h0, h1] at h
      obtain ⟨rfl, rfl, rfl, rfl⟩ := h
      simp [h0] at hv
  | some v0 =>
    obtain ⟨s0, t0, e0⟩ := v0
    cases h1 : view d 1 with
    | none =>
      simp [h0, h1] at h
      obtain ⟨rfl, rfl, rfl, rfl⟩ := h
      simp [h1] at hv
    | some v1 =>
      obtain ⟨s1, t1, e1⟩ := v1
      by_cases hl : s0 < s1
      · simp [h0, h1, hl] at h
        obtain ⟨rfl, rfl, rfl, rfl⟩ := h
        simp [h0] at hv
        omega
      · simp [h0, h1, hl] at h
        obtain ⟨rfl, rfl, rfl, rfl⟩ := h
        simp [h1] at hv
        omega

theorem other_ne {i : Nat} (hi : i = 0 ∨ i = 1) : 1 - i ≠ i := by
  rcases hi with rfl | rfl <;> decide

theorem other_other {i : Nat} (hi : i = 0 ∨ i = 1) : 1 - (1 - i) = i := by
  rcases hi with rfl | rfl <;> rfl

theorem other_slot {i : Nat} (hi : i = 0 ∨ i = 1) : (1 - i = 0 ∨ 1 - i = 1) := by
  rcases hi with rfl | rfl <;> simp

theorem slot_lt_two {i : Nat} (hi : i = 0 ∨ i = 1) : i < 2 := by
  rcases hi with rfl | rfl <;> decide

/-- Si un disco coincide con otro en la tabla y los datos de un árbol, el
árbol sigue valiendo. -/
theorem treeOk_of_agree {d d' : Disk} {t} {es : List Entry} (h : TreeOk d t es)
    (ht : d' t = d t) (he : ∀ e ∈ es, d' e.addr = d e.addr) : TreeOk d' t es := by
  obtain ⟨h2, htab, hdat⟩ := h
  refine ⟨h2, by rw [ht, htab], fun e he' => ?_⟩
  obtain ⟨a2, ad⟩ := hdat e he'
  exact ⟨a2, by rw [he e he', ad]⟩

theorem mem_put {es : List Entry} {o n D a} {e : Entry} (h : e ∈ put es o n D a) :
    e = { owner := o, name := n, version := nextVersion es o n, addr := a, data := D } ∨ e ∈ es := by
  simp only [put, List.mem_cons, List.mem_filter] at h
  rcases h with h | ⟨h, _⟩
  · exact Or.inl h
  · exact Or.inr h

-- ------------------------------------------------------------- parte A

/-- **Consistencia ante caídas.** Con el invariante y bloques libres para
los datos y la tabla nueva, cualquier combinación de escrituras hechas,
no hechas o a medias se recupera como el catálogo viejo o el nuevo. -/
theorem crash_safe {d : Disk} {i seq t : Nat} {es : List Entry} {o n : Nat} {D : List Nat}
    {a t' : Nat} (hg : Good d i seq t es) (ha : Fresh t es a) (ht : Fresh t es t')
    (hat : a ≠ t') (o1 o2 o3 : Out) :
    recover (commit d i seq es o n D a t' o1 o2 o3) = some (i, seq, t, es) ∨
    recover (commit d i seq es o n D a t' o1 o2 o3) =
      some (1 - i, seq + 1, t', put es o n D a) := by
  have hi2 := slot_lt_two hg.slot
  have ho2 : 1 - i < 2 := by omega
  -- Lo que el estado confirmado usa no cambió.
  have keep : ∀ x, x ≠ a → x ≠ t' → x ≠ 1 - i →
      commit d i seq es o n D a t' o1 o2 o3 x = d x := by
    intro x h1 h2 h3
    simp only [commit]
    rw [apply_ne _ _ h3, apply_ne _ _ h2, apply_ne _ _ h1]
  have a2 := ha.1
  have t2 := ht.1
  have hcur : commit d i seq es o n D a t' o1 o2 o3 i = d i :=
    keep i (by omega) (by omega) (other_ne hg.slot).symm
  have htree : TreeOk (commit d i seq es o n D a t' o1 o2 o3) t es := by
    obtain ⟨t2, _, hdat⟩ := hg.tree
    refine treeOk_of_agree hg.tree (keep t (Ne.symm ha.2.1) (Ne.symm ht.2.1) (by omega)) ?_
    intro e he
    have e2 := (hdat e he).1
    exact keep e.addr (ha.2.2 e he) (ht.2.2 e he) (by omega)
  have hv : view (commit d i seq es o n D a t' o1 o2 o3) i = some (seq, t, es) :=
    view_of (by rw [hcur]; exact hg.cur) htree
  -- La otra ranura: lo de antes, el superbloque nuevo o basura.
  cases o3 with
  | skip =>
    left
    refine recover_cur hg.slot hv ?_
    intro s' t'' e' hv'
    have hd := (view_some hv').1
    have : commit d i seq es o n D a t' o1 o2 .skip (1 - i) = d (1 - i) := by
      simp only [commit, apply_skip]
      rw [apply_ne _ _ (by have := ht.1; omega), apply_ne _ _ (by have := ha.1; omega)]
    rw [this] at hd
    exact hg.other _ _ _ hd
  | torn =>
    left
    refine recover_cur hg.slot hv ?_
    intro s' t'' e' hv'
    have hd := (view_some hv').1
    simp only [commit, apply_torn, upd_eq] at hd
    cases hd
  | full =>
    by_cases hnew : view (commit d i seq es o n D a t' o1 o2 .full) (1 - i) =
        some (seq + 1, t', put es o n D a)
    · right
      have := recover_newer (other_slot hg.slot) hnew
        (by rw [other_other hg.slot]; exact hv) (Nat.lt_succ_self seq)
      exact this
    · left
      refine recover_cur hg.slot hv ?_
      intro s' t'' e' hv'
      have hd := view_some hv'
      have h1 : commit d i seq es o n D a t' o1 o2 .full (1 - i) =
          .super (seq + 1) t' (put es o n D a) := by
        simp only [commit, apply_full, upd_eq]
      rw [h1] at hd
      obtain ⟨hd, htr⟩ := hd
      cases hd
      exact absurd (view_of h1 htr) hnew

/-- **Escritura completa.** Si las tres escrituras terminan, el catálogo
nuevo queda confirmado en la otra ranura y el invariante se mantiene. -/
theorem commit_good {d : Disk} {i seq t : Nat} {es : List Entry} {o n : Nat} {D : List Nat}
    {a t' : Nat} (hg : Good d i seq t es) (ha : Fresh t es a) (ht : Fresh t es t')
    (hat : a ≠ t') :
    Good (commit d i seq es o n D a t' .full .full .full) (1 - i) (seq + 1) t'
      (put es o n D a) := by
  have hi2 := slot_lt_two hg.slot
  have a2 := ha.1
  have t2 := ht.1
  let d' := commit d i seq es o n D a t' .full .full .full
  have eq_a : d' a = .data D := by
    simp only [d', commit, apply_full]
    rw [upd_ne _ (by omega), upd_ne _ hat, upd_eq]
  have eq_t : d' t' = .table (put es o n D a) := by
    simp only [d', commit, apply_full]
    rw [upd_ne _ (by omega), upd_eq]
  have eq_o : d' (1 - i) = .super (seq + 1) t' (put es o n D a) := by
    simp only [d', commit, apply_full, upd_eq]
  have keep : ∀ x, x ≠ a → x ≠ t' → x ≠ 1 - i → d' x = d x := by
    intro x h1 h2 h3
    simp only [d', commit, apply_full]
    rw [upd_ne _ h3, upd_ne _ h2, upd_ne _ h1]
  refine ⟨other_slot hg.slot, eq_o, ⟨t2, eq_t, ?_⟩, ?_⟩
  · intro e he
    rcases mem_put he with rfl | hmem
    · exact ⟨a2, eq_a⟩
    · obtain ⟨e2, ed⟩ := hg.tree.2.2 e hmem
      refine ⟨e2, ?_⟩
      show d' e.addr = _
      rw [keep e.addr (ha.2.2 e hmem) (ht.2.2 e hmem) (by omega)]
      exact ed
  · intro s t'' es' h
    rw [other_other hg.slot] at h
    change d' i = _ at h
    rw [keep i (by omega) (by omega) (other_ne hg.slot).symm, hg.cur] at h
    cases h
    exact Nat.lt_succ_self _

/-- **Montaje.** Si alguna ranura valida, montar deja el invariante con lo
recuperado. -/
theorem mount_good {d d' : Disk} {i s t : Nat} {es : List Entry}
    (h : mount d = some (d', i, s, t, es)) : Good d' i s t es ∧ recover d = some (i, s, t, es) := by
  unfold mount at h
  split at h
  · simp at h
  · rename_i i0 s0 t0 es0 hr
    simp only [Option.some.injEq, Prod.mk.injEq] at h
    obtain ⟨hd', rfl, rfl, rfl, rfl⟩ := h
    obtain ⟨hi, hv⟩ := recover_slot hr
    obtain ⟨hcur, htree⟩ := view_some hv
    have hi2 := slot_lt_two hi
    -- `d'` solo difiere de `d` en la otra ranura.
    have keep : ∀ x, x ≠ 1 - i0 → d' x = d x := by
      intro x hx
      rw [← hd']
      split
      · split
        · exact upd_ne _ hx
        · rfl
      · rfl
    refine ⟨⟨hi, ?_, ?_, ?_⟩, hr⟩
    · rw [keep i0 (other_ne hi).symm]; exact hcur
    · refine treeOk_of_agree htree (keep t0 (by have := htree.1; omega)) ?_
      intro e he
      exact keep e.addr (by have := (htree.2.2 e he).1; omega)
    · intro s' t' es' hs
      rw [← hd'] at hs
      revert hs
      split
      · rename_i s'' t'' es'' hdo
        by_cases hle : s0 ≤ s''
        · simp only [hle, if_true, upd_eq]
          intro hs; cases hs
        · simp only [hle, if_false]
          intro hs
          rw [hdo] at hs
          cases hs
          omega
      · rename_i hns
        intro hs
        exact absurd hs (hns s' t' es')

/-- Montar un disco con el invariante no cambia nada visible: recupera lo
confirmado. -/
theorem mount_of_good {d : Disk} {i seq t : Nat} {es : List Entry} (hg : Good d i seq t es) :
    ∃ d', mount d = some (d', i, seq, t, es) := by
  have hv := view_of hg.cur hg.tree
  have hr : recover d = some (i, seq, t, es) := by
    refine recover_cur hg.slot hv ?_
    intro s' t' e' hv'
    exact hg.other _ _ _ (view_some hv').1
  unfold mount
  rw [hr]
  exact ⟨_, rfl⟩

/-- **Caída + montaje.** Se recupera el catálogo viejo o el nuevo, y el disco
queda listo para seguir escribiendo. -/
theorem crash_then_mount {d : Disk} {i seq t : Nat} {es : List Entry} {o n : Nat}
    {D : List Nat} {a t' : Nat} (hg : Good d i seq t es) (ha : Fresh t es a)
    (ht : Fresh t es t') (hat : a ≠ t') (o1 o2 o3 : Out) :
    ∃ d' j s u c, mount (commit d i seq es o n D a t' o1 o2 o3) = some (d', j, s, u, c) ∧
      Good d' j s u c ∧
      ((j, s, u, c) = (i, seq, t, es) ∨ (j, s, u, c) = (1 - i, seq + 1, t', put es o n D a)) := by
  have hc := crash_safe (o := o) (n := n) (D := D) hg ha ht hat o1 o2 o3
  have : ∀ r, recover (commit d i seq es o n D a t' o1 o2 o3) = some r →
      ∃ d', mount (commit d i seq es o n D a t' o1 o2 o3) = some (d', r) := by
    intro r hr
    unfold mount
    rw [hr]
    exact ⟨_, rfl⟩
  rcases hc with hr | hr
  · obtain ⟨d', hm⟩ := this _ hr
    exact ⟨d', _, _, _, _, hm, (mount_good hm).1, Or.inl rfl⟩
  · obtain ⟨d', hm⟩ := this _ hr
    exact ⟨d', _, _, _, _, hm, (mount_good hm).1, Or.inr rfl⟩

/-- Formatear: tabla vacía en el bloque 2, la ranura 1 borrada y el primer
superbloque (secuencia 1) en la ranura 0. -/
def format (d : Disk) : Disk :=
  upd (upd (upd d 2 (.table [])) 1 .junk) 0 (.super 1 2 [])

theorem format_good (d : Disk) : Good (format d) 0 1 2 [] := by
  refine ⟨Or.inl rfl, by simp [format, upd], ⟨by decide, by simp [format, upd], by simp⟩, ?_⟩
  intro s t es h
  simp [format, upd] at h

-- ------------------------------------------------------------- mutaciones

/-! ### Por qué hace falta cada regla

Cada ejemplo es un error plausible del código, y muestra un disco con el
invariante donde ese error lleva a perder el estado o a mezclarlo. -/

/-- Un objeto confirmado: el 0 del dueño 0, versión 1, datos `[7]` en el
bloque 3. -/
def e7 : Entry := { owner := 0, name := 0, version := 1, addr := 3, data := [7] }

/-- Disco con `e7` confirmado: superbloque 1 en la ranura 0, tabla en 2. -/
def d7 : Disk := fun x =>
  if x = 0 then .super 1 2 [e7] else if x = 2 then .table [e7]
  else if x = 3 then .data [7] else .junk

theorem d7_good : Good d7 0 1 2 [e7] := by
  refine ⟨Or.inl rfl, rfl, by decide, ?_⟩
  intro s t es h
  simp [d7] at h

/-- **Sin `Fresh`, se pierde todo.** Si los datos nuevos pisan los del
objeto confirmado (bloque 3) y el superbloque queda a medias, ninguna ranura
valida: ni el catálogo viejo ni el nuevo. -/
theorem overwrite_breaks :
    recover (commit d7 0 1 [e7] 0 0 [8] 3 4 .full .full .torn) = none := by decide

/-- **El superbloque va a la otra ranura.** Si fuera a la actual y quedara a
medias, tampoco se recupera nada. -/
theorem same_slot_breaks :
    recover (apply (apply (apply d7 4 (.data [8]) .full) 5
      (.table (put [e7] 0 0 [8] 4)) .full) 0 (.super 2 5 (put [e7] 0 0 [8] 4)) .torn) = none := by
  decide

/-- **La secuencia sube.** Con la misma secuencia, una escritura completa
se pierde: al montar gana la ranura 0, que tiene el catálogo viejo. -/
theorem same_seq_loses :
    recover (apply (apply (apply d7 4 (.data [8]) .full) 5
      (.table (put [e7] 0 0 [8] 4)) .full) 1 (.super 1 5 (put [e7] 0 0 [8] 4)) .full)
      = some (0, 1, 2, [e7]) := by decide

/-- Objeto que nunca se confirmó: el 4 del dueño 0 con datos `[8]` en el
bloque 6. -/
def e8 : Entry := { owner := 0, name := 4, version := 1, addr := 6, data := [8] }

/-- Disco después de una escritura de `e8` que se cayó con el superbloque
(secuencia 2) y la tabla escritos pero los datos no: la ranura 1 no valida y
se recupera la 0 (catálogo vacío). -/
def dStale : Disk := fun x =>
  if x = 0 then .super 1 2 [] else if x = 2 then .table []
  else if x = 1 then .super 2 5 [e8] else if x = 5 then .table [e8] else .junk

theorem dStale_recovers_old : recover dStale = some (0, 1, 2, []) := by decide

/-- **Hay que limpiar la otra ranura al montar.** Sin limpiar, la escritura
siguiente (el objeto 9 con los mismos datos `[8]`, que justo usa el bloque 6)
completa el árbol viejo: si se cae antes del superbloque nuevo, "resucita"
`e8`, un catálogo que nunca se confirmó (ni el viejo ni el nuevo). -/
theorem stale_resurrects :
    recover (commit dStale 0 1 [] 0 9 [8] 6 7 .full .skip .skip) = some (1, 2, 5, [e8]) := by
  decide

/-- Con la limpieza de `mount`, la ranura vieja ya no cuenta. -/
theorem mount_cleans_stale : ∃ d', mount dStale = some (d', 0, 1, 2, []) ∧ d' 1 = .junk := by
  refine ⟨_, rfl, ?_⟩
  decide

-- ------------------------------------------------------------- parte B

/-- Un handle del almacén (el extremo de canal que tiene el almacén para un
objeto abierto): de qué objeto es y si permite escribir. -/
structure Handle where
  owner : Nat
  name : Nat
  write : Bool
  deriving DecidableEq, Repr

/-- Estado del almacén en memoria: el catálogo y los handles abiertos. -/
structure Srv where
  cat : List Entry
  hnd : Nat → Option Handle

/-- Pedidos. Quién los hace no importa: lo que cuenta es por qué canal
llegan (la raíz `r` de un dueño o un handle `h`). -/
inductive Op where
  /-- `OPEN` por la raíz del dueño `r`: handle nuevo `h` al objeto `n` de
  `r`, con escritura si `w`. -/
  | openObj (r n : Nat) (w : Bool) (h : Nat)
  /-- `DUP_RO` por el handle `src`: handle nuevo `h`, de solo lectura. -/
  | dupRo (src h : Nat)
  /-- `WRITE` por el handle `h` (datos `D`, en el bloque `a`). -/
  | write (h : Nat) (D : List Nat) (a : Nat)
  /-- Se cerró el handle `h`. -/
  | close (h : Nat)
  deriving DecidableEq

/-- Asigna el handle `h`. -/
def setH (s : Srv) (h : Nat) (v : Option Handle) : Srv :=
  { s with hnd := fun x => if x = h then v else s.hnd x }

/-- Lo que hace el almacén con cada pedido. Un pedido que no cumple las
condiciones se rechaza (no es un paso). -/
inductive Exec : Srv → Op → Srv → Prop where
  | openObj (s : Srv) (r n : Nat) (w : Bool) (h : Nat) :
      s.hnd h = none → Exec s (.openObj r n w h) (setH s h (some ⟨r, n, w⟩))
  | dupRo (s : Srv) (src h : Nat) (x : Handle) :
      s.hnd src = some x → s.hnd h = none →
      Exec s (.dupRo src h) (setH s h (some { x with write := false }))
  | write (s : Srv) (h : Nat) (x : Handle) (D : List Nat) (a : Nat) :
      s.hnd h = some x → x.write = true →
      Exec s (.write h D a) { s with cat := put s.cat x.owner x.name D a }
  | close (s : Srv) (h : Nat) : Exec s (.close h) (setH s h none)

/-- Estados alcanzables desde un catálogo recién montado (sin handles), con
la lista de pedidos atendidos. -/
inductive Reach : Srv → List Op → Prop where
  | init (c : List Entry) : Reach ⟨c, fun _ => none⟩ []
  | step {s t : Srv} {tr : List Op} {op : Op} : Reach s tr → Exec s op t → Reach t (tr ++ [op])

/-- **Sin amplificación.** Todo handle escribible a un objeto de `o` lo abrió
con escritura la raíz de `o`. -/
theorem rw_handle_from_owner {s : Srv} {tr : List Op} (hr : Reach s tr) :
    ∀ h x, s.hnd h = some x → x.write = true → Op.openObj x.owner x.name true h ∈ tr := by
  induction hr with
  | init c => intro h x hx; simp at hx
  | step _ ex ih =>
    intro h x hx hw
    cases ex with
    | openObj r n w h' hfree =>
      simp only [setH] at hx
      by_cases hh : h = h'
      · subst hh
        simp at hx
        subst hx
        simp at hw
        subst hw
        simp
      · simp [hh] at hx
        exact List.mem_append_left _ (ih h x hx hw)
    | dupRo src h' y _ _ =>
      simp only [setH] at hx
      by_cases hh : h = h'
      · subst hh
        simp at hx
        subst hx
        simp at hw
      · simp [hh] at hx
        exact List.mem_append_left _ (ih h x hx hw)
    | write h' y D a _ _ =>
      exact List.mem_append_left _ (ih h x hx hw)
    | close h' =>
      simp only [setH] at hx
      by_cases hh : h = h'
      · simp [hh] at hx
      · simp [hh] at hx
        exact List.mem_append_left _ (ih h x hx hw)

theorem find_filter_ne {es : List Entry} {o n o' n' : Nat} (hk : (o', n') ≠ (o, n)) :
    find (es.filter (fun e => !isKey o n e)) o' n' = find es o' n' := by
  unfold find
  induction es with
  | nil => rfl
  | cons e es ih =>
    simp only [List.filter_cons]
    by_cases he : isKey o n e = true
    · simp only [he, Bool.not_true, Bool.false_eq_true, if_false]
      rw [ih]
      have : isKey o' n' e = false := by
        simp only [isKey, Bool.and_eq_true, beq_iff_eq] at he
        simp only [isKey, Bool.and_eq_false_iff, beq_eq_false_iff_ne]
        obtain ⟨h1, h2⟩ := he
        by_cases ho : e.owner = o'
        · right; intro hn; exact hk (by rw [← ho, ← hn, h1, h2])
        · left; exact ho
      simp [List.find?_cons, this]
    · simp only [Bool.not_eq_true] at he
      simp only [he, Bool.not_false, if_true, List.find?_cons]
      cases isKey o' n' e <;> simp [ih]

/-- Escribir un objeto no cambia ningún otro. -/
theorem find_put_ne {es : List Entry} {o n o' n' : Nat} {D a} (hk : (o', n') ≠ (o, n)) :
    find (put es o n D a) o' n' = find es o' n' := by
  have : isKey o' n' { owner := o, name := n, version := nextVersion es o n, addr := a, data := D }
      = false := by
    simp only [isKey, Bool.and_eq_false_iff, beq_eq_false_iff_ne]
    by_cases ho : o = o'
    · right; intro hn; exact hk (by rw [ho, hn])
    · left; exact ho
  simp only [put, find, List.find?_cons, this]
  exact find_filter_ne hk

/-- **Solo el dueño habilita cambios.** Si cambia el objeto `n` de `o`, fue
una escritura por un handle escribible a ese objeto, que abrió la raíz de
`o`. -/
theorem change_needs_owner {s t : Srv} {tr : List Op} {op : Op} {o n : Nat}
    (hr : Reach s tr) (ex : Exec s op t) (hch : find t.cat o n ≠ find s.cat o n) :
    ∃ h D a, op = .write h D a ∧ s.hnd h = some ⟨o, n, true⟩ ∧ Op.openObj o n true h ∈ tr := by
  cases ex with
  | openObj => exact absurd rfl hch
  | dupRo => exact absurd rfl hch
  | close => exact absurd rfl hch
  | write h x D a hx hw =>
    by_cases hk : (o, n) = (x.owner, x.name)
    · simp only [Prod.mk.injEq] at hk
      obtain ⟨rfl, rfl⟩ := hk
      refine ⟨h, D, a, rfl, ?_, rw_handle_from_owner hr h x hx hw⟩
      rw [hx]
      cases x
      simp at hw
      simp [hw]
    · exact absurd (find_put_ne hk) hch

/-- **Aislamiento entre dueños.** Si la raíz de `o` nunca abrió el objeto `n`
con escritura, el objeto sigue como en el catálogo montado, haga lo que haga
cualquier otro (otro dueño por su raíz, o quien tenga copias de solo
lectura). -/
theorem untouched_without_owner {s : Srv} {tr : List Op} {o n : Nat} {c : List Entry}
    (hr : Reach s tr) (hc : ∀ s0, Reach s0 [] → s0.cat = c)
    (hno : ∀ h, Op.openObj o n true h ∉ tr) : find s.cat o n = find c o n := by
  induction hr with
  | init c' =>
    have := hc ⟨c', fun _ => none⟩ (.init c')
    simp at this
    rw [this]
  | @step s t tr op hr' ex ih =>
    have ih' := ih (fun h hm => hno h (List.mem_append_left _ hm))
    by_cases hch : find t.cat o n = find s.cat o n
    · rw [hch, ih']
    · obtain ⟨h, _, _, _, _, hm⟩ := change_needs_owner hr' ex hch
      exact absurd (List.mem_append_left _ hm) (hno h)

end Store
end Nsos
