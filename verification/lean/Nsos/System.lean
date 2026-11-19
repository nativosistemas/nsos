import Nsos.CSpace

/-!
# Sistema: cells, canales e IPC

Modelo de las syscalls de capacidades y canales de `kernel/src/syscall.rs` y
`kernel/src/cell.rs` (hito 4). Cada cell tiene su CSpace; cada extremo de
canal tiene la cola de mensajes que le llegan. Un mensaje en cola lleva
capacidades que ya no pertenecen a ninguna cell.

Simplificaciones respecto del kernel:
* Los envíos y recepciones son no bloqueantes: bloquear es reintentar más
  tarde, y no cambia el estado.
* Los datos del mensaje se abstraen en una etiqueta (`label`).
* El cierre de extremos es por alcanzabilidad: tras cada syscall se cierra
  todo extremo que ninguna cell pueda alcanzar (directamente o siguiendo
  colas), y se descarta su cola. Es lo que logran juntos el conteo de
  referencias (`Arc`) y el recolector `ipc_gc` del kernel.
-/

namespace Nsos

/-- Objetos del kernel. Por ahora solo extremos de canal: `endpoint par lado`. -/
inductive Obj where
  | endpoint (pair : Nat) (side : Bool)
  deriving DecidableEq, Repr

namespace Obj
/-- Par (canal) al que pertenece el extremo. -/
def pair : Obj → Nat | .endpoint p _ => p
/-- El otro extremo del canal. -/
def peer : Obj → Obj | .endpoint p s => .endpoint p (!s)
end Obj

/-- Mensaje en cola. -/
structure Msg where
  label : Nat
  caps : List (Cap Obj)

/-- `QUEUE_LIMIT` de `kernel/src/cell.rs`. -/
def QUEUE_LIMIT : Nat := 64
/-- `MAX_CAPS` de `nsos_abi::msg`. -/
def MAX_CAPS : Nat := 4
/-- `CSPACE_LIMIT` de `kernel/src/cell.rs`. -/
def CSPACE_LIMIT : Nat := 1024

/-- Conjunto de derechos a partir de una lista. -/
def Rights.ofList (l : List Right) : Rights := ⟨fun x => l.contains x⟩

/-- `ENDPOINT_RIGHTS`: derechos de un extremo recién creado. -/
def ENDPOINT_RIGHTS : Rights := Rights.ofList [.send, .recv, .grant, .duplicate, .wait]

/-- Estado global. -/
structure Sys where
  /-- CSpace de cada cell (el índice es el id de la cell). -/
  cells : List (CSpace Obj)
  /-- Mensajes que le llegan a cada extremo. -/
  queue : Obj → List Msg
  /-- Extremos cerrados. -/
  closed : Obj → Bool
  /-- Próximo par libre: los objetos con `pair ≥ nextPair` todavía no existen. -/
  nextPair : Nat

/-- Syscalls modeladas. Los handles y máscaras son los valores crudos del ABI. -/
inductive Op where
  | dup (h mask : Nat)
  | drop (h : Nat)
  | create
  | send (h label : Nat) (hs : List Nat)
  | recv (h : Nat)
  deriving Repr

/-- `true` para las syscalls de recepción. -/
def Op.isRecv : Op → Bool
  | .recv _ => true
  | _ => false

/-- Actualiza una función en un punto. -/
def update {β : Type} (f : Obj → β) (o : Obj) (v : β) : Obj → β :=
  fun x => if x = o then v else f x

theorem mem_update {f : Obj → List Msg} {o : Obj} {v : List Msg} {x : Obj} {m : Msg}
    (h : m ∈ update f o v x) : m ∈ v ∨ m ∈ f x := by
  unfold update at h
  split at h
  · exact .inl h
  · exact .inr h

/-- Sin duplicados (versión booleana). -/
def nodupB : List Nat → Bool
  | [] => true
  | x :: xs => !xs.contains x && nodupB xs

/-- `Message::validate` en lo que respecta a las capacidades: como mucho 4,
ningún handle nulo y sin repetidos. -/
def validCaps (hs : List Nat) : Bool :=
  hs.length ≤ MAX_CAPS && hs.all (fun h => Handle.slot h != 0) && nodupB hs

namespace Sys

/-- Extremos que existen. -/
def endpoints (s : Sys) : List Obj :=
  (List.range s.nextPair).flatMap fun p => [.endpoint p false, .endpoint p true]

/-- Objetos que viajan en la cola del extremo `o`. Quien tenga `o` los puede
recibir. -/
def queuedObjs (s : Sys) (o : Obj) : List Obj :=
  (s.queue o).flatMap fun m => m.caps.map (·.obj)

/-- Objetos que alguna cell tiene en su CSpace. -/
def roots (s : Sys) : List Obj :=
  s.cells.flatMap fun cs => cs.caps.map (·.obj)

/-- Clausura: lo que se alcanza desde `acc` siguiendo colas. -/
def reachGo (s : Sys) : Nat → List Obj → List Obj
  | 0, acc => acc
  | n + 1, acc =>
    let new := (acc.flatMap s.queuedObjs).filter (fun o => !acc.contains o)
    if new.isEmpty then acc else reachGo s n (acc ++ new.eraseDups)

/-- Objetos alcanzables: los que tiene alguna cell, y los que viajan en la
cola de un extremo alcanzable. Ningún otro puede volver a usarse nunca. -/
def reachSet (s : Sys) : List Obj := s.reachGo (2 * s.nextPair + 1) s.roots

/-- Recolección tras cada syscall: cierra los extremos inalcanzables y
descarta sus colas. Es lo que hace `ipc_gc` en el kernel (al estilo del
recolector de descriptores en vuelo de Linux): sin esto, un ciclo de extremos
que viajan en colas ajenas quedaría vivo para siempre. -/
def gc (s : Sys) : Sys :=
  let r := s.reachSet
  { s with closed := fun e => s.closed e || !r.contains e
           queue := fun e => if r.contains e then s.queue e else [] }

/-- CSpace de la cell `a`. -/
def cellAt (s : Sys) (a : Nat) : Except Err (CSpace Obj) :=
  match s.cells[a]? with
  | some cs => .ok cs
  | none => .error .badHandle

/-- Reemplaza la CSpace de la cell `a`. -/
def setCell (s : Sys) (a : Nat) (cs : CSpace Obj) : Sys := { s with cells := s.cells.set a cs }

/-- Efecto de una syscall de la cell `a` (sin la recolección posterior). -/
def exec (s : Sys) (a : Nat) : Op → Except Err (Sys × List Nat)
  | .dup h mask =>
    if !Rights.validBits mask then .error .invalidArgs else
    match s.cellAt a with
    | .error e => .error e
    | .ok cs =>
      match cs.dup h (Rights.ofBits mask) with
      | .error e => .error e
      | .ok (cs', h') => .ok (s.setCell a cs', [h'])
  | .drop h =>
    match s.cellAt a with
    | .error e => .error e
    | .ok cs =>
      match cs.remove h with
      | .error e => .error e
      | .ok (cs', _) => .ok (s.setCell a cs', [])
  | .create =>
    match s.cellAt a with
    | .error e => .error e
    | .ok cs =>
      let e0 := Obj.endpoint s.nextPair false
      let e1 := Obj.endpoint s.nextPair true
      match cs.insertAll [⟨e0, ENDPOINT_RIGHTS⟩, ⟨e1, ENDPOINT_RIGHTS⟩] with
      | .error e => .error e
      | .ok (cs', hs) =>
        .ok ({ s.setCell a cs' with
                nextPair := s.nextPair + 1
                queue := update (update s.queue e0 []) e1 []
                closed := update (update s.closed e0 false) e1 false }, hs)
  | .send h label hs =>
    if !validCaps hs then .error .badMessage else
    match s.cellAt a with
    | .error e => .error e
    | .ok cs =>
      match cs.getWith h (Rights.ofList [.send]) with
      | .error e => .error e
      | .ok c =>
        let dst := c.obj.peer
        if s.closed dst then .error .peerClosed
        else if (s.queue dst).length ≥ QUEUE_LIMIT then .error .wouldBlock
        else
          match cs.removeAll hs (Rights.ofList [.grant]) with
          | .error e => .error e
          | .ok (cs', moved) =>
            .ok ({ s.setCell a cs' with
                    queue := update s.queue dst (s.queue dst ++ [⟨label, moved⟩]) }, [])
  | .recv h =>
    match s.cellAt a with
    | .error e => .error e
    | .ok cs =>
      match cs.getWith h (Rights.ofList [.recv]) with
      | .error e => .error e
      | .ok c =>
        match s.queue c.obj with
        | m :: rest =>
          match cs.insertAll m.caps with
          | .error e => .error e
          | .ok (cs', hs) => .ok ({ s.setCell a cs' with queue := update s.queue c.obj rest },
                                  m.label :: hs)
        | [] => if s.closed c.obj.peer then .error .peerClosed else .error .wouldBlock

/-- Una syscall completa: efecto + cierre en cascada. Si falla, el estado no
cambia (el kernel garantiza atomicidad: ver `remove_all`/`insert_all`). -/
def step (s : Sys) (a : Nat) (op : Op) : Except Err (Sys × List Nat) :=
  match s.exec a op with
  | .error e => .error e
  | .ok (s', r) => .ok (s'.gc, r)

end Sys
end Nsos
