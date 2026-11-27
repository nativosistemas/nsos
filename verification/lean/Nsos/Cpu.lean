import Nsos.Quota

/-!
# Reservas de CPU por supervisor

Modelo de la parte nueva de las cuotas de CPU (punto 4 de
`docs/amenazas.md`). Se escribe antes que el código del scheduler.

Cada cell tiene una **reserva**: unidades de tiempo por período que el
scheduler le garantiza. Las reservas se reparten como la cuota de memoria:
`cell_spawn` le da a la hija parte de la reserva del supervisor, y al morir
vuelve. Esa parte (que la suma de las reservas no cambia) es una instancia de
`Nsos/Quota.lean`, con `avail` como la reserva.

Lo que se modela acá es un período del scheduler. Al empezar, cada cell
tiene su reserva entera para gastar (`left`). En cada unidad de tiempo,
algunas cells quieren correr (`ready`) y el scheduler elige a quién darle la
CPU, con una sola regla: **si alguna cell que quiere correr todavía tiene
reserva, la CPU es para una de ellas**, y se le descuenta. Si no, puede
correr cualquiera (o nadie) sin descontar nada: así el scheduler no deja la
CPU ociosa.

## Qué se demuestra

* `tick_le`, `run_le`: lo que le queda a cada cell solo baja.
* `tick_sum`: cuando alguna cell con reserva quiere correr, lo que queda en
  total baja exactamente en uno.
* `reserved`: si las reservas suman como mucho la longitud del período, una
  cell que quiere correr en todo el período lo termina con su reserva
  gastada entera: corrió al menos su reserva, hagan lo que hagan las demás
  (cuántas sean, cuánto quieran correr) y elija lo que elija el scheduler
  entre las que tienen reserva.

## Abstracciones

* El tiempo es discreto: cada unidad se le cobra a quien corrió. El kernel
  mide el tiempo de verdad (en nanosegundos, en cada cambio de contexto),
  así que una cell no puede esquivar el cobro cediendo la CPU justo antes
  del tick.
* El período empieza con las reservas enteras (el kernel las renueva al
  cambiar de período).
* Los hilos del kernel no se modelan: el kernel se guarda una parte del
  período para ellos.

## Mutaciones que rompen la demostración

Cada una hace fallar `tick_sum`, en que se apoya `reserved`: que el
scheduler pueda elegir a una cell sin reserva aunque haya otra con reserva
lista, no descontarle la unidad a quien corrió con reserva, y dejar la CPU
sin dar a nadie con reserva aunque haya alguno listo.
-/

namespace Nsos.CpuModel

open Nsos.QuotaModel (upd sumOn sumOn_upd le_sumOn)

/-- ¿Alguna cell de `ts` quiere correr y tiene reserva? -/
def someReady (ts : List Nat) (left : Nat → Nat) (ready : Nat → Bool) : Bool :=
  ts.any fun y => ready y && decide (0 < left y)

/-- Una unidad de tiempo. `ready` dice quién quiere correr y `c` a quién
le da la CPU el scheduler (con reserva) o `none` (nadie con reserva). -/
def tick (ts : List Nat) (left : Nat → Nat) (ready : Nat → Bool) (c : Option Nat) :
    Option (Nat → Nat) :=
  if someReady ts left ready then
    match c with
    | some x =>
      if x ∈ ts ∧ ready x = true ∧ 0 < left x then some (upd left x (left x - 1)) else none
    | none => none
  else some left

theorem someReady_of {ts : List Nat} {left : Nat → Nat} {ready : Nat → Bool} {x : Nat}
    (hx : x ∈ ts) (hr : ready x = true) (hl : 0 < left x) : someReady ts left ready = true := by
  simp only [someReady, List.any_eq_true]
  exact ⟨x, hx, by simp [hr, hl]⟩

theorem tick_le {ts : List Nat} {left l' : Nat → Nat} {ready : Nat → Bool} {c : Option Nat}
    (h : tick ts left ready c = some l') (y : Nat) : l' y ≤ left y := by
  simp only [tick] at h
  split at h
  · cases c with
    | none => cases h
    | some x =>
      simp only at h
      split at h
      · cases h
        by_cases hy : y = x
        · subst hy; simp [upd]
        · simp [upd, hy]
      · cases h
  · cases h; exact Nat.le_refl _

theorem tick_sum {ts : List Nat} {left l' : Nat → Nat} {ready : Nat → Bool} {c : Option Nat}
    (hnd : ts.Nodup) (h : tick ts left ready c = some l') (hs : someReady ts left ready = true) :
    sumOn ts l' + 1 = sumOn ts left := by
  simp only [tick, hs, ite_true] at h
  cases c with
  | none => cases h
  | some x =>
    simp only at h
    split at h
    · rename_i hc
      cases h
      have := sumOn_upd (f := left) (v := left x - 1) hnd hc.1
      have hp := hc.2.2
      omega
    · cases h

/-- Un período: una lista de unidades de tiempo, cada una con quién quiere
correr y a quién se le dio la CPU. -/
inductive Run (ts : List Nat) : (Nat → Nat) → List ((Nat → Bool) × Option Nat) → (Nat → Nat) → Prop
  | nil (l : Nat → Nat) : Run ts l [] l
  | cons {l l1 l2 : Nat → Nat} {r : Nat → Bool} {c : Option Nat} {es} :
      tick ts l r c = some l1 → Run ts l1 es l2 → Run ts l ((r, c) :: es) l2

theorem run_le {ts : List Nat} {l l' : Nat → Nat} {es} (h : Run ts l es l') (y : Nat) :
    l' y ≤ l y := by
  induction h with
  | nil => exact Nat.le_refl _
  | cons ht _ ih => exact Nat.le_trans ih (tick_le ht y)

/-- Si `x` quiere correr en todas las unidades y al final le queda reserva,
entonces en todas había alguien con reserva listo y el total bajó en uno por
unidad. -/
theorem run_sum {ts : List Nat} {l l' : Nat → Nat} {es} {x : Nat} (hnd : ts.Nodup)
    (hx : x ∈ ts) (h : Run ts l es l') (hready : ∀ e ∈ es, e.1 x = true) (hleft : 0 < l' x) :
    sumOn ts l' + es.length = sumOn ts l := by
  induction h with
  | nil => simp
  | cons ht hr ih =>
    rename_i l0 l1 l2 r c es'
    have h1 := ih (fun e he => hready e (List.mem_cons_of_mem _ he)) hleft
    have hl1 : 0 < l1 x := Nat.lt_of_lt_of_le hleft (run_le hr x)
    have hl0 : 0 < l0 x := Nat.lt_of_lt_of_le hl1 (tick_le ht x)
    have hrx : r x = true := hready (r, c) (List.mem_cons_self ..)
    have h2 := tick_sum hnd ht (someReady_of hx hrx hl0)
    simp only [List.length_cons]
    omega

/-- **La reserva se cumple**: si las reservas suman como mucho la longitud
del período y `x` quiere correr en todo el período, termina con su reserva
gastada entera (corrió al menos su reserva). -/
theorem reserved {ts : List Nat} {b l' : Nat → Nat} {es} {x : Nat} (hnd : ts.Nodup)
    (hx : x ∈ ts) (h : Run ts b es l') (hsum : sumOn ts b ≤ es.length)
    (hready : ∀ e ∈ es, e.1 x = true) : l' x = 0 := by
  by_cases hz : l' x = 0
  · exact hz
  · have hpos : 0 < l' x := Nat.pos_of_ne_zero hz
    have hs := run_sum hnd hx h hready hpos
    have hle := le_sumOn (f := l') hx
    omega

end Nsos.CpuModel
