import Nsos.CSpace

/-!
# Sistema: cells, canales e IPC

Modelo de las syscalls de capacidades y canales de `kernel/src/syscall.rs` y
`kernel/src/cell.rs` (hito 4). Cada cell tiene su CSpace; cada extremo de
canal tiene la cola de mensajes que le llegan. Un mensaje en cola lleva
capacidades que ya no pertenecen a ninguna cell.

Simplificaciones respecto del kernel:
* Bloquear se modela en dos pasos atómicos, como en el kernel: la syscall no
  bloqueante falla con `WouldBlock`, y `waitRecv`/`waitSend` vuelven a mirar
  la condición y, si sigue sin cumplirse, registran a la cell como esperando
  (todo bajo el lock del canal en el kernel). Cada cell tiene un hilo.
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

/-- Mensaje en cola. `id` es un número único que el modelo le asigna al
enviarlo (no existe en el kernel), para poder seguirlo. -/
structure Msg where
  id : Nat
  label : Nat
  caps : List (Cap Obj)

/-- Qué fue de un mensaje. -/
inductive Fate where
  | unsent
  | queued (o : Obj)
  | delivered
  /-- Descartado porque nadie podía recibirlo (su extremo quedó inalcanzable). -/
  | dropped
  deriving DecidableEq, Repr

/-- Por qué está bloqueada una cell: esperando recibir en un extremo, o
esperando lugar en la cola de un extremo para enviar. -/
inductive Wait where
  | recv (e : Obj)
  | send (e : Obj)
  deriving DecidableEq, Repr

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
  /-- Cells esperando un mensaje en cada extremo (`recv_waiters`). -/
  recvWaiters : Obj → List Nat
  /-- Cells esperando lugar en la cola de cada extremo (`send_waiters`). -/
  sendWaiters : Obj → List Nat
  /-- En qué está bloqueada cada cell, si lo está. -/
  blocked : Nat → Option Wait
  /-- Próximo id de mensaje. -/
  nextMsg : Nat
  /-- Qué fue de cada mensaje. -/
  fate : Nat → Fate

/-- Syscalls modeladas. Los handles y máscaras son los valores crudos del ABI. -/
inductive Op where
  | dup (h mask : Nat)
  | drop (h : Nat)
  | create
  | send (h label : Nat) (hs : List Nat)
  | recv (h : Nat)
  /-- Tras un `recv` con `WouldBlock`: bloquearse si sigue sin haber nada. -/
  | waitRecv (h : Nat)
  /-- Tras un `send` con `WouldBlock`: bloquearse si la cola sigue llena. -/
  | waitSend (h : Nat)
  deriving Repr

/-- `true` para las syscalls de recepción. -/
def Op.isRecv : Op → Bool
  | .recv _ => true
  | _ => false

/-- Actualiza una función en un punto. -/
def update {β : Type} (f : Obj → β) (o : Obj) (v : β) : Obj → β :=
  fun x => if x = o then v else f x

/-- Despierta a una lista de cells. -/
def wakeAll (b : Nat → Option Wait) (ts : List Nat) : Nat → Option Wait :=
  fun t => if ts.contains t then none else b t

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

/-- Candidatos a agregar a la clausura: raíces y lo que viaja en colas de
lo ya alcanzado. -/
def frontier (s : Sys) (acc : List Obj) : List Obj :=
  s.roots ++ acc.flatMap s.queuedObjs

/-- Clausura, un objeto por vez: lo que se alcanza desde las CSpaces
siguiendo colas. -/
def reachGo (s : Sys) : Nat → List Obj → List Obj
  | 0, acc => acc
  | n + 1, acc =>
    match (s.frontier acc).find? (fun o => !acc.contains o) with
    | none => acc
    | some x => reachGo s n (x :: acc)

/-- Objetos alcanzables: los que tiene alguna cell, y los que viajan en la
cola de un extremo alcanzable. Ningún otro puede volver a usarse nunca. -/
def reachSet (s : Sys) : List Obj := s.reachGo (2 * s.nextPair + 1) []

/-- ¿Se cierra `e` en esta recolección? Solo extremos que existen. -/
def dead (s : Sys) (r : List Obj) (e : Obj) : Bool :=
  decide (e.pair < s.nextPair) && !r.contains e

/-- Recolección tras cada syscall: cierra los extremos inalcanzables,
descarta sus colas (sus mensajes quedan `dropped`) y despierta a quien
esperaba en ellos o en su peer. Es lo que hace `ipc_gc` + `Drop for Endpoint`
en el kernel (al estilo del recolector de descriptores en vuelo de Linux). -/
def gc (s : Sys) : Sys :=
  let r := s.reachSet
  let d := s.dead r
  let woken := s.endpoints.flatMap fun e =>
    if d e then s.recvWaiters e ++ s.sendWaiters e ++ s.recvWaiters e.peer ++
      s.sendWaiters e.peer else []
  { s with closed := fun e => s.closed e || d e
           queue := fun e => if d e then [] else s.queue e
           recvWaiters := fun e => if d e || d e.peer then [] else s.recvWaiters e
           sendWaiters := fun e => if d e || d e.peer then [] else s.sendWaiters e
           blocked := wakeAll s.blocked woken
           fate := fun i => match s.fate i with
             | .queued o => if d o then .dropped else .queued o
             | f => f }

/-- CSpace de la cell `a`. -/
def cellAt (s : Sys) (a : Nat) : Except Err (CSpace Obj) :=
  match s.cells[a]? with
  | some cs => .ok cs
  | none => .error .badHandle

/-- Reemplaza la CSpace de la cell `a`. -/
def setCell (s : Sys) (a : Nat) (cs : CSpace Obj) : Sys := { s with cells := s.cells.set a cs }

/-- Efecto de una syscall de una cell no bloqueada. -/
def exec' (s : Sys) (a : Nat) : Op → Except Err (Sys × List Nat)
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
        .ok ({ s.setCell a cs' with nextPair := s.nextPair + 1 }, hs)
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
                    queue := update s.queue dst (s.queue dst ++ [⟨s.nextMsg, label, moved⟩])
                    nextMsg := s.nextMsg + 1
                    fate := fun i => if i = s.nextMsg then .queued dst else s.fate i
                    recvWaiters := update s.recvWaiters dst []
                    blocked := wakeAll s.blocked (s.recvWaiters dst) }, [])
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
          | .ok (cs', hs) =>
            .ok ({ s.setCell a cs' with
                    queue := update s.queue c.obj rest
                    fate := fun i => if i = m.id then .delivered else s.fate i
                    sendWaiters := update s.sendWaiters c.obj []
                    blocked := wakeAll s.blocked (s.sendWaiters c.obj) }, m.label :: hs)
        | [] => if s.closed c.obj.peer then .error .peerClosed else .error .wouldBlock
  | .waitRecv h =>
    match s.cellAt a with
    | .error e => .error e
    | .ok cs =>
      match cs.getWith h (Rights.ofList [.recv]) with
      | .error e => .error e
      | .ok c =>
        -- Volver a mirar bajo el lock: si ya hay algo (o el peer cerró), no
        -- bloquear; la cell reintenta el `recv`.
        if !(s.queue c.obj).isEmpty || s.closed c.obj.peer then .error .invalidArgs
        else .ok ({ s with recvWaiters := update s.recvWaiters c.obj (a :: s.recvWaiters c.obj)
                           blocked := fun t => if t = a then some (.recv c.obj) else s.blocked t },
                  [])
  | .waitSend h =>
    match s.cellAt a with
    | .error e => .error e
    | .ok cs =>
      match cs.getWith h (Rights.ofList [.send]) with
      | .error e => .error e
      | .ok c =>
        let dst := c.obj.peer
        if s.closed dst || (s.queue dst).length < QUEUE_LIMIT then .error .invalidArgs
        else .ok ({ s with sendWaiters := update s.sendWaiters dst (a :: s.sendWaiters dst)
                           blocked := fun t => if t = a then some (.send dst) else s.blocked t },
                  [])

/-- Efecto de una syscall de la cell `a` (sin la recolección posterior). Una
cell bloqueada no ejecuta nada. -/
def exec (s : Sys) (a : Nat) (op : Op) : Except Err (Sys × List Nat) :=
  if (s.blocked a).isSome then .error .wouldBlock else s.exec' a op

/-- Una syscall completa: efecto + cierre en cascada. Si falla, el estado no
cambia (el kernel garantiza atomicidad: ver `remove_all`/`insert_all`). -/
def step (s : Sys) (a : Nat) (op : Op) : Except Err (Sys × List Nat) :=
  match s.exec a op with
  | .error e => .error e
  | .ok (s', r) => .ok (s'.gc, r)

end Sys
end Nsos
