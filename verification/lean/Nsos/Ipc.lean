import Nsos.Security

/-!
# IPC completo: colas, bloqueo y recolección

Teoremas:

* `Sys.step_msgInv`: **ningún mensaje se pierde ni se duplica**. Cada mensaje
  enviado está en exactamente una posición de una sola cola, o se entregó, o
  se descartó porque su extremo quedó inalcanzable (y entonces nadie podía
  recibirlo). `Sys.step_fate_final`: entregado y descartado son finales, así
  que un mensaje se entrega a lo sumo una vez.
* `Sys.gc_keeps_reachable`: **el recolector nunca libera un canal vivo**: un
  extremo alcanzable conserva su cola y no se cierra.
* `Sys.step_wakeInv`: **sin despertares perdidos**. Toda cell bloqueada está
  registrada para que la despierten, y su condición de espera sigue sin
  cumplirse (no duerme mientras podría avanzar).
-/

namespace Nsos
namespace Sys

/-! ## Lemas generales -/

theorem wakeAll_some {b : Nat → Option Wait} {ts : List Nat} {t : Nat} {w : Wait}
    (h : wakeAll b ts t = some w) : b t = some w ∧ t ∉ ts := by
  unfold wakeAll at h
  split at h
  · cases h
  · rename_i hn
    exact ⟨h, by simpa using hn⟩

theorem update_apply {β : Type} (f : Obj → β) (o x : Obj) (v : β) :
    update f o v x = if x = o then v else f x := rfl

theorem mem_endpoints {s : Sys} {e : Obj} (h : e.pair < s.nextPair) : e ∈ s.endpoints := by
  cases e with
  | endpoint p b =>
    simp only [Obj.pair] at h
    simp only [endpoints, List.mem_flatMap, List.mem_range]
    exact ⟨p, h, by cases b <;> simp⟩

theorem peer_peer (e : Obj) : e.peer.peer = e := by
  cases e; simp [Obj.peer]

theorem peer_pair (e : Obj) : e.peer.pair = e.pair := by
  cases e; simp [Obj.peer, Obj.pair]

/-! ## Conservación de mensajes -/

/-- Cuántas veces está el mensaje `i` en la cola de `o`. -/
def qcount (s : Sys) (o : Obj) (i : Nat) : Nat := ((s.queue o).map (·.id)).count i

/-- Invariante de mensajes. -/
def MsgInv (s : Sys) : Prop :=
  (∀ o i, s.qcount o i = if s.fate i = .queued o then 1 else 0) ∧
  (∀ i, s.nextMsg ≤ i → s.fate i = .unsent)

/-- Consecuencia: un mensaje está a lo sumo una vez en una cola, y en una sola
cola. -/
theorem msgInv_unique {s : Sys} (h : s.MsgInv) {o o' : Obj} {i : Nat}
    (h1 : 0 < s.qcount o i) (h2 : 0 < s.qcount o' i) : o = o' ∧ s.qcount o i = 1 := by
  have a := h.1 o i
  have b := h.1 o' i
  split at a
  · split at b
    · rename_i ha hb
      rw [ha] at hb
      cases hb
      exact ⟨rfl, a⟩
    · omega
  · omega

theorem msgInv_of_eq {s s' : Sys} (hq : s'.queue = s.queue) (hf : s'.fate = s.fate)
    (hn : s'.nextMsg = s.nextMsg) (h : s.MsgInv) : s'.MsgInv := by
  unfold MsgInv qcount; rw [hq, hf, hn]; exact h

theorem msgInv_send {s : Sys} (h : s.MsgInv) (dst : Obj) (l : Nat) (moved : List (Cap Obj))
    (cells : List (CSpace Obj)) (rw : Obj → List Nat) (b : Nat → Option Wait) :
    MsgInv { s with cells := cells
                    queue := update s.queue dst (s.queue dst ++ [⟨s.nextMsg, l, moved⟩])
                    nextMsg := s.nextMsg + 1
                    fate := fun i => if i = s.nextMsg then .queued dst else s.fate i
                    recvWaiters := rw
                    blocked := b } := by
  obtain ⟨h1, h2⟩ := h
  have hu := h2 s.nextMsg (Nat.le_refl _)
  refine ⟨?_, ?_⟩
  · intro o i
    have e := h1 o i
    unfold qcount at e ⊢
    simp only [update_apply]
    by_cases ho : o = dst
    · subst ho
      simp only [ite_true, List.map_append, List.count_append, List.map_cons, List.map_nil,
        List.count_cons, List.count_nil]
      by_cases hi : i = s.nextMsg
      · subst hi; simp [hu] at e ⊢; omega
      · have : (s.nextMsg == i) = false := by simp; omega
        simp [hi, this, e]
    · simp only [ho, ite_false]
      by_cases hi : i = s.nextMsg
      · subst hi; simp [hu] at e; simp [e, Ne.symm ho]
      · simp [hi, e]
  · intro i hi
    simp only at hi
    have : i ≠ s.nextMsg := by omega
    simp [this, h2 i (by omega)]

theorem msgInv_recv {s : Sys} (h : s.MsgInv) {o : Obj} {m : Msg} {rest : List Msg}
    (hq : s.queue o = m :: rest) (cells : List (CSpace Obj)) (sw : Obj → List Nat)
    (b : Nat → Option Wait) :
    MsgInv { s with cells := cells
                    queue := update s.queue o rest
                    fate := fun i => if i = m.id then .delivered else s.fate i
                    sendWaiters := sw
                    blocked := b } := by
  obtain ⟨h1, h2⟩ := h
  have hm := h1 o m.id
  unfold qcount at hm
  rw [hq] at hm
  simp only [List.map_cons, List.count_cons, beq_self_eq_true, ite_true] at hm
  have hf : s.fate m.id = .queued o := by
    by_cases hf : s.fate m.id = .queued o
    · exact hf
    · simp [hf] at hm
  have hr : (rest.map (·.id)).count m.id = 0 := by simp [hf] at hm; omega
  refine ⟨?_, ?_⟩
  · intro o' i
    have e := h1 o' i
    unfold qcount at e ⊢
    simp only [update_apply]
    by_cases ho : o' = o
    · subst ho
      rw [hq] at e
      simp only [ite_true]
      by_cases hi : i = m.id
      · subst hi; simp [hr]
      · have : (m.id == i) = false := by simp; omega
        simp only [List.map_cons, List.count_cons, this] at e
        simpa [hi] using e
    · simp only [ho, ite_false]
      by_cases hi : i = m.id
      · subst hi; simp [hf] at e; simp [e, Ne.symm ho]
      · simp [hi, e]
  · intro i hi
    have hne : i ≠ m.id := by
      intro he; subst he; rw [h2 _ hi] at hf; cases hf
    simp [hne, h2 i hi]

theorem exec'_msgInv {s s' : Sys} {a : Nat} {op : Op} {r : List Nat} (h : s.MsgInv)
    (he : s.exec' a op = .ok (s', r)) : s'.MsgInv := by
  cases op with
  | dup =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; exact msgInv_of_eq rfl rfl rfl h
  | drop =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    cases he; exact msgInv_of_eq rfl rfl rfl h
  | create _ =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    cases he; exact msgInv_of_eq rfl rfl rfl h
  | send =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; exact msgInv_send h _ _ _ _ _ _
  | recv =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he
    · rename_i m rest hq
      split at he; · cases he
      cases he; exact msgInv_recv h hq _ _ _
    · split at he <;> cases he
  | waitRecv =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; exact msgInv_of_eq rfl rfl rfl h
  | waitSend =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; exact msgInv_of_eq rfl rfl rfl h

theorem gc_fate_queued {s : Sys} {i : Nat} {o : Obj} :
    s.gc.fate i = .queued o ↔ s.fate i = .queued o ∧ s.dead s.reachSet o = false := by
  simp only [gc]
  split
  · rename_i o' hf
    rw [hf]
    split <;> (simp_all; try (intro hh; subst hh; assumption))
  · rename_i hf
    constructor
    · intro h'; exact absurd h' (hf o)
    · intro ⟨h', _⟩; exact absurd h' (hf o)

theorem gc_msgInv {s : Sys} (h : s.MsgInv) : s.gc.MsgInv := by
  obtain ⟨h1, h2⟩ := h
  refine ⟨?_, ?_⟩
  · intro o i
    have e := h1 o i
    by_cases hd : s.dead s.reachSet o = true
    · have : ¬ s.gc.fate i = .queued o := by rw [gc_fate_queued]; simp [hd]
      simp only [this, ite_false]
      simp [qcount, gc, hd]
    · have hd' : s.dead s.reachSet o = false := by simpa using hd
      have : s.gc.fate i = .queued o ↔ s.fate i = .queued o := by rw [gc_fate_queued]; simp [hd']
      simp only [this]
      rw [← e]
      simp [qcount, gc, hd']
  · intro i hi
    simp only [gc] at hi ⊢
    rw [h2 i hi]

/-- **Ningún mensaje se pierde ni se duplica**: el invariante se mantiene en
cada syscall, exitosa o no. -/
theorem step_msgInv {s : Sys} {a : Nat} {op : Op} (h : s.MsgInv) :
    ∀ {s' r}, s.step a op = .ok (s', r) → s'.MsgInv := by
  intro s' r hs
  unfold step at hs
  split at hs
  · cases hs
  rename_i s₁ r₁ he
  cases hs
  unfold exec at he
  split at he
  · cases he
  exact gc_msgInv (exec'_msgInv h he)

/-- El primer mensaje de una cola está registrado en esa cola. -/
theorem fate_head {s : Sys} (h : s.MsgInv) {o : Obj} {m : Msg} {rest : List Msg}
    (hq : s.queue o = m :: rest) : s.fate m.id = .queued o := by
  have hm := h.1 o m.id
  unfold qcount at hm
  rw [hq] at hm
  by_cases hf : s.fate m.id = .queued o
  · exact hf
  · simp [hf] at hm

/-- Un mensaje entregado o descartado queda así. -/
def _root_.Nsos.Fate.final : Fate → Bool
  | .delivered | .dropped => true
  | _ => false

theorem exec'_fate_final {s s' : Sys} {a : Nat} {op : Op} {r : List Nat} (h : s.MsgInv)
    (he : s.exec' a op = .ok (s', r)) {i : Nat} (hf : (s.fate i).final) :
    s'.fate i = s.fate i := by
  have hnu : ∀ j, s.fate j = .unsent → i ≠ j := by
    intro j hj hij; subst hij; rw [hj] at hf; cases hf
  have hnq : ∀ j o, s.fate j = .queued o → i ≠ j := by
    intro j o hj hij; subst hij; rw [hj] at hf; cases hf
  cases op with
  | dup =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; rfl
  | drop =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    cases he; rfl
  | create _ =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    cases he; rfl
  | send =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he
    simp [hnu _ (h.2 _ (Nat.le_refl _))]
  | recv =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he
    · rename_i m rest hq
      split at he; · cases he
      cases he
      simp [hnq _ _ (fate_head h hq)]
    · split at he <;> cases he
  | waitRecv =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; rfl
  | waitSend =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; rfl

/-- **Entregado y descartado son finales**: un mensaje se entrega a lo sumo
una vez, y uno descartado no reaparece. -/
theorem step_fate_final {s s' : Sys} {a : Nat} {op : Op} {r : List Nat} (h : s.MsgInv)
    (hs : s.step a op = .ok (s', r)) {i : Nat} (hf : (s.fate i).final) :
    s'.fate i = s.fate i := by
  unfold step at hs
  split at hs
  · cases hs
  rename_i s₁ r₁ he
  cases hs
  unfold exec at he
  split at he
  · cases he
  have e := exec'_fate_final h he hf
  rw [← e] at hf ⊢
  simp only [gc]
  split
  · rename_i o' hq; rw [hq] at hf; cases hf
  · rfl

/-! ## El recolector no libera canales vivos -/

/-- Buena formación: toda capacidad apunta a un objeto que existe. -/
def WF (s : Sys) : Prop := ∀ c, s.Exists c → c.obj.pair < s.nextPair

theorem step_wf {s s' : Sys} {a : Nat} {op : Op} {r : List Nat} (h : s.WF)
    (hs : s.step a op = .ok (s', r)) : s'.WF := by
  obtain ⟨⟨cs, cs', h1, h2, h3⟩, h4, h5⟩ := step_spec hs
  intro c hc
  rcases hc with ⟨b, csb, hb, hcb⟩ | hq
  · by_cases hab : b = a
    · subst hab
      rw [h2, List.getElem?_set_self', h1] at hb
      cases hb
      rcases h3 c hcb with ⟨c₀, h0, ho, _⟩ | ⟨_, hlt⟩ | ⟨_, hq⟩
      · rw [← ho]; exact Nat.lt_of_lt_of_le (h c₀ (.inl ⟨b, cs, h1, h0⟩)) h4
      · exact hlt
      · exact Nat.lt_of_lt_of_le (h c (.inr hq)) h4
    · rw [h2, List.getElem?_set_ne (Ne.symm hab)] at hb
      exact Nat.lt_of_lt_of_le (h c (.inl ⟨b, csb, hb, hcb⟩)) h4
  · exact Nat.lt_of_lt_of_le (h c (h5 c hq)) h4

theorem reach_exists {s : Sys} {x : Obj} (h : s.Reach x) : ∃ c, s.Exists c ∧ c.obj = x := by
  cases h with
  | held hcs hc =>
    obtain ⟨a, ha⟩ := List.mem_iff_getElem?.1 hcs
    exact ⟨_, .inl ⟨a, _, ha, hc⟩, rfl⟩
  | queued _ hm hc => exact ⟨_, .inr ⟨_, _, hm, hc⟩, rfl⟩

theorem reach_pair {s : Sys} (hw : s.WF) {x : Obj} (h : s.Reach x) : x.pair < s.nextPair := by
  obtain ⟨c, hc, rfl⟩ := reach_exists h
  exact hw c hc

theorem endpoints_length (s : Sys) : s.endpoints.length = 2 * s.nextPair := by
  unfold endpoints
  generalize s.nextPair = n
  induction n with
  | zero => rfl
  | succ n ih => simp [List.range_succ, List.flatMap_append, ih]; omega

/-- Si la frontera ya está en `acc`, todo lo alcanzable está en `acc`. -/
theorem closed_reach {s : Sys} {acc : List Obj} (hc : ∀ y, y ∈ s.frontier acc → y ∈ acc)
    {x : Obj} (h : s.Reach x) : x ∈ acc := by
  induction h with
  | held hcs hc' =>
    apply hc
    simp only [frontier, roots, queuedObjs, List.mem_append, List.mem_flatMap, List.mem_map]
    exact .inl ⟨_, hcs, _, hc', rfl⟩
  | queued _ hm hc' ih =>
    apply hc
    simp only [frontier, roots, queuedObjs, List.mem_append, List.mem_flatMap, List.mem_map]
    exact .inr ⟨_, ih, _, hm, _, hc', rfl⟩

theorem reachGo_complete (s : Sys) (hw : s.WF) : ∀ (n : Nat) (acc : List Obj),
    acc.Nodup → (∀ y, y ∈ acc → s.Reach y) → 2 * s.nextPair < acc.length + n →
    ∀ x, s.Reach x → x ∈ s.reachGo n acc := by
  intro n
  induction n with
  | zero =>
    intro acc hn hr hlen x _
    exfalso
    have := hn.length_le_of_subset (fun y hy => mem_endpoints (reach_pair hw (hr y hy)))
    rw [endpoints_length] at this
    omega
  | succ n ih =>
    intro acc hn hr hlen x hx
    simp only [reachGo]
    split
    · rename_i hf
      apply closed_reach _ hx
      intro y hy
      have := List.find?_eq_none.1 hf y hy
      simpa using this
    · rename_i y hf
      have hyn : y ∉ acc := by simpa using List.find?_some hf
      have hyr : s.Reach y := frontier_reach s hr (List.mem_of_find?_eq_some hf)
      apply ih (y :: acc) (List.nodup_cons.2 ⟨hyn, hn⟩) _ (by simp; omega) x hx
      intro z hz
      simp only [List.mem_cons] at hz
      rcases hz with rfl | hz
      · exact hyr
      · exact hr z hz

/-- `reachSet` calcula exactamente lo alcanzable. -/
theorem mem_reachSet {s : Sys} (hw : s.WF) {x : Obj} : x ∈ s.reachSet ↔ s.Reach x :=
  ⟨reachSet_sound s, fun h => reachGo_complete s hw _ [] List.nodup_nil (by simp)
    (by simp) x h⟩

/-- **El recolector nunca libera un canal vivo**: un extremo que alguna cell
todavía puede alcanzar (directamente o recibiendo mensajes) no se cierra ni
pierde su cola. -/
theorem gc_keeps_reachable {s : Sys} (hw : s.WF) {e : Obj} (h : s.Reach e) :
    s.gc.closed e = s.closed e ∧ s.gc.queue e = s.queue e := by
  have hm := (mem_reachSet hw).2 h
  simp [gc, dead, hm]

/-! ## Sin despertares perdidos -/

/-- Toda cell bloqueada está registrada en el extremo que la va a despertar, y
su condición de espera sigue sin cumplirse: si está esperando recibir, la
cola está vacía y el peer abierto; si está esperando enviar, la cola de
destino está llena y abierta. Equivale a: en cuanto la condición se cumple, la
cell ya no está bloqueada. -/
def WakeInv (s : Sys) : Prop :=
  (∀ t e, s.blocked t = some (.recv e) →
    t ∈ s.recvWaiters e ∧ s.queue e = [] ∧ s.closed e.peer = false) ∧
  (∀ t e, s.blocked t = some (.send e) →
    t ∈ s.sendWaiters e ∧ QUEUE_LIMIT ≤ (s.queue e).length ∧ s.closed e = false)

theorem wakeInv_send {s : Sys} (h : s.WakeInv) (dst : Obj) (msg : Msg)
    (cells : List (CSpace Obj)) (n : Nat) (f : Nat → Fate) :
    WakeInv { s with cells := cells
                     queue := update s.queue dst (s.queue dst ++ [msg])
                     nextMsg := n
                     fate := f
                     recvWaiters := update s.recvWaiters dst []
                     blocked := wakeAll s.blocked (s.recvWaiters dst) } := by
  obtain ⟨h1, h2⟩ := h
  refine ⟨?_, ?_⟩
  · intro t e hb
    obtain ⟨hb, hn⟩ := wakeAll_some hb
    obtain ⟨r1, r2, r3⟩ := h1 t e hb
    have hne : e ≠ dst := by rintro rfl; exact hn r1
    simp only [update_apply, hne, ite_false]
    exact ⟨r1, r2, r3⟩
  · intro t e hb
    obtain ⟨hb, _⟩ := wakeAll_some hb
    obtain ⟨r1, r2, r3⟩ := h2 t e hb
    refine ⟨r1, ?_, r3⟩
    simp only [update_apply]
    split
    · rename_i he; subst he; simp only [List.length_append]; omega
    · exact r2

theorem wakeInv_recv {s : Sys} (h : s.WakeInv) {o : Obj} {m : Msg} {rest : List Msg}
    (hq : s.queue o = m :: rest) (cells : List (CSpace Obj)) (f : Nat → Fate) :
    WakeInv { s with cells := cells
                     queue := update s.queue o rest
                     fate := f
                     sendWaiters := update s.sendWaiters o []
                     blocked := wakeAll s.blocked (s.sendWaiters o) } := by
  obtain ⟨h1, h2⟩ := h
  refine ⟨?_, ?_⟩
  · intro t e hb
    obtain ⟨hb, _⟩ := wakeAll_some hb
    obtain ⟨r1, r2, r3⟩ := h1 t e hb
    have hne : e ≠ o := by rintro rfl; rw [hq] at r2; cases r2
    simp only [update_apply, hne, ite_false]
    exact ⟨r1, r2, r3⟩
  · intro t e hb
    obtain ⟨hb, hn⟩ := wakeAll_some hb
    obtain ⟨r1, r2, r3⟩ := h2 t e hb
    have hne : e ≠ o := by rintro rfl; exact hn r1
    simp only [update_apply, hne, ite_false]
    exact ⟨r1, r2, r3⟩

theorem wakeInv_waitRecv {s : Sys} (h : s.WakeInv) {o : Obj} {a : Nat}
    (hq : s.queue o = []) (hc : s.closed o.peer = false) :
    WakeInv { s with recvWaiters := update s.recvWaiters o (a :: s.recvWaiters o)
                     blocked := fun t => if t = a then some (.recv o) else s.blocked t } := by
  obtain ⟨h1, h2⟩ := h
  refine ⟨?_, ?_⟩
  · intro t e hb
    simp only at hb
    split at hb
    · cases hb; rename_i ht; subst ht
      simp [update_apply, hq, hc]
    · obtain ⟨r1, r2, r3⟩ := h1 t e hb
      refine ⟨?_, r2, r3⟩
      simp only [update_apply]
      split
      · rename_i he; subst he; exact List.mem_cons_of_mem _ r1
      · exact r1
  · intro t e hb
    simp only at hb
    split at hb
    · cases hb
    · exact h2 t e hb

theorem wakeInv_waitSend {s : Sys} (h : s.WakeInv) {o : Obj} {a : Nat}
    (hq : QUEUE_LIMIT ≤ (s.queue o).length) (hc : s.closed o = false) :
    WakeInv { s with sendWaiters := update s.sendWaiters o (a :: s.sendWaiters o)
                     blocked := fun t => if t = a then some (.send o) else s.blocked t } := by
  obtain ⟨h1, h2⟩ := h
  refine ⟨?_, ?_⟩
  · intro t e hb
    simp only at hb
    split at hb
    · cases hb
    · exact h1 t e hb
  · intro t e hb
    simp only at hb
    split at hb
    · cases hb; rename_i ht; subst ht
      simp [update_apply, hq, hc]
    · obtain ⟨r1, r2, r3⟩ := h2 t e hb
      refine ⟨?_, r2, r3⟩
      simp only [update_apply]
      split
      · rename_i he; subst he; exact List.mem_cons_of_mem _ r1
      · exact r1

theorem exec'_wakeInv {s s' : Sys} {a : Nat} {op : Op} {r : List Nat} (h : s.WakeInv)
    (he : s.exec' a op = .ok (s', r)) : s'.WakeInv := by
  cases op with
  | dup =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; exact h
  | drop =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    cases he; exact h
  | create _ =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    cases he; exact h
  | send =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    cases he; exact wakeInv_send h _ _ _ _ _
  | recv =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he
    · rename_i m rest hq
      split at he; · cases he
      cases he; exact wakeInv_recv h hq _ _
    · split at he <;> cases he
  | waitRecv =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    rename_i hcond
    simp only [Bool.or_eq_true, Bool.not_eq_true', List.isEmpty_eq_false_iff, not_or,
      Bool.not_eq_true] at hcond
    cases he
    exact wakeInv_waitRecv h (by simpa using hcond.1) hcond.2
  | waitSend =>
    simp only [exec'] at he
    split at he; · cases he
    split at he; · cases he
    split at he; · cases he
    rename_i hcond
    simp only [Bool.or_eq_true, decide_eq_true_eq, not_or, Bool.not_eq_true,
      Nat.not_lt] at hcond
    cases he
    exact wakeInv_waitSend h hcond.2 hcond.1

theorem mem_woken {s : Sys} {e : Obj} {t : Nat} (hd : s.dead s.reachSet e = true)
    (ht : t ∈ s.recvWaiters e ∨ t ∈ s.sendWaiters e ∨ t ∈ s.recvWaiters e.peer ∨
      t ∈ s.sendWaiters e.peer) :
    t ∈ s.endpoints.flatMap fun e => if s.dead s.reachSet e then
      s.recvWaiters e ++ s.sendWaiters e ++ s.recvWaiters e.peer ++ s.sendWaiters e.peer
      else [] := by
  have hp : e.pair < s.nextPair := by
    simp only [dead, Bool.and_eq_true, decide_eq_true_eq] at hd; exact hd.1
  simp only [List.mem_flatMap]
  refine ⟨e, mem_endpoints hp, ?_⟩
  simp only [hd, ite_true, List.mem_append]
  rcases ht with h | h | h | h <;> simp [h]

/-- Una cell que no se despierta en la recolección esperaba en un extremo que
sigue vivo, igual que su peer. -/
theorem gc_not_woken {s : Sys} {e : Obj} {t : Nat}
    (hn : ¬ t ∈ s.endpoints.flatMap fun e => if s.dead s.reachSet e then
      s.recvWaiters e ++ s.sendWaiters e ++ s.recvWaiters e.peer ++ s.sendWaiters e.peer
      else [])
    (ht : t ∈ s.recvWaiters e ∨ t ∈ s.sendWaiters e) :
    s.dead s.reachSet e = false ∧ s.dead s.reachSet e.peer = false := by
  constructor
  · cases hd : s.dead s.reachSet e
    · rfl
    · exact absurd (mem_woken hd (by rcases ht with h | h <;> simp [h])) hn
  · cases hd : s.dead s.reachSet e.peer
    · rfl
    · exact absurd (mem_woken hd (by rw [peer_peer]; rcases ht with h | h <;> simp [h])) hn

theorem gc_wakeInv {s : Sys} (h : s.WakeInv) : s.gc.WakeInv := by
  obtain ⟨h1, h2⟩ := h
  refine ⟨?_, ?_⟩
  · intro t e hb
    simp only [gc] at hb
    obtain ⟨hb, hn⟩ := wakeAll_some hb
    obtain ⟨r1, r2, r3⟩ := h1 t e hb
    obtain ⟨d1, d2⟩ := gc_not_woken hn (.inl r1)
    simp [gc, d1, d2, r1, r2, r3]
  · intro t e hb
    simp only [gc] at hb
    obtain ⟨hb, hn⟩ := wakeAll_some hb
    obtain ⟨r1, r2, r3⟩ := h2 t e hb
    obtain ⟨d1, d2⟩ := gc_not_woken hn (.inr r1)
    simp [gc, d1, d2, r1, r2, r3]

/-- **Sin despertares perdidos**: el invariante se mantiene en cada syscall. -/
theorem step_wakeInv {s : Sys} {a : Nat} {op : Op} (h : s.WakeInv) :
    ∀ {s' r}, s.step a op = .ok (s', r) → s'.WakeInv := by
  intro s' r hs
  unfold step at hs
  split at hs
  · cases hs
  rename_i s₁ r₁ he
  cases hs
  unfold exec at he
  split at he
  · cases he
  exact gc_wakeInv (exec'_wakeInv h he)

/-! ## Ejecuciones -/

/-- Los tres invariantes juntos. -/
def Inv (s : Sys) : Prop := s.MsgInv ∧ s.WakeInv ∧ s.WF

/-- Un sistema recién arrancado (colas vacías, nadie bloqueado, ningún mensaje
enviado, capacidades solo a objetos que existen) cumple los invariantes. -/
theorem inv_init {s : Sys} (hq : ∀ o, s.queue o = []) (hb : ∀ t, s.blocked t = none)
    (hf : ∀ i, s.fate i = .unsent)
    (hc : ∀ (a : Nat) (cs : CSpace Obj) (c : Cap Obj), s.cells[a]? = some cs → c ∈ cs.caps →
      c.obj.pair < s.nextPair) :
    s.Inv := by
  refine ⟨⟨?_, fun i _ => hf i⟩, ⟨?_, ?_⟩, ?_⟩
  · intro o i; simp [qcount, hq, hf]
  · intro t e h; rw [hb] at h; cases h
  · intro t e h; rw [hb] at h; cases h
  · intro c hc'
    rcases hc' with ⟨a, cs, h1, h2⟩ | ⟨o, m, hm', _⟩
    · exact hc a cs c h1 h2
    · rw [hq] at hm'; cases hm'

theorem step_inv {s s' : Sys} {a : Nat} {op : Op} {r : List Nat} (h : s.Inv)
    (hs : s.step a op = .ok (s', r)) : s'.Inv :=
  ⟨step_msgInv h.1 hs, step_wakeInv h.2.1 hs, step_wf h.2.2 hs⟩

/-- Los invariantes valen en toda ejecución. -/
theorem run_inv {s s' : Sys} {tr : List (Nat × Op)} (h : Run s tr s') (hi : s.Inv) :
    s'.Inv := by
  induction h with
  | nil => exact hi
  | ok hs _ ih => exact ih (step_inv hi hs)
  | err _ _ ih => exact ih hi

/-- **`channel_call`** (hito 13) no agrega pasos al modelo: es un `send` en el
extremo `h` seguido de un `recv` en el mismo extremo, con esperas
(`waitRecv`) y pasos de otras cells en el medio. Por eso conserva los tres
invariantes: ningún mensaje se pierde ni se duplica, el recolector no libera
nada vivo y nadie duerme con un mensaje esperándolo. -/
theorem call_inv {s s' : Sys} {a h l : Nat} {hs : List Nat} {mid : List (Nat × Op)}
    (hr : Run s ((a, .send h l hs) :: mid ++ [(a, .recv h)]) s') (hi : s.Inv) : s'.Inv :=
  run_inv hr hi

end Sys
end Nsos
