import Nsos.Security

/-!
# Escenarios concretos

Se evalúan al compilar (`#guard`). Sirven como documentación ejecutable del
modelo y dejan registrados dos límites conocidos del diseño actual.
-/

namespace Nsos
namespace Examples

/-- Cells vacías, sin canales. -/
def empty (n : Nat) : Sys :=
  ⟨List.replicate n (CSpace.new CSPACE_LIMIT), fun _ => [], fun _ => false, 0⟩

/-- Ejecuta una lista de syscalls; devuelve el estado y el resultado de cada una. -/
def runAll (s : Sys) : List (Nat × Op) → Sys × List (Except Err (List Nat))
  | [] => (s, [])
  | (a, op) :: rest =>
    match s.step a op with
    | .ok (s', r) => let (s'', rs) := runAll s' rest; (s'', .ok r :: rs)
    | .error e => let (s'', rs) := runAll s rest; (s'', .error e :: rs)

def isOk {β : Type} : Except Err β → Bool | .ok _ => true | .error _ => false
def okIs (v : List Nat) : Except Err (List Nat) → Bool | .ok r => r == v | .error _ => false
def errIs {β : Type} (e : Err) : Except Err β → Bool | .error e' => e == e' | .ok _ => false

/-- Handles de un CSpace nuevo: el primero es 1, el segundo 2, ... -/
abbrev h1 : Nat := 1
abbrev h2 : Nat := 2
abbrev h3 : Nat := 3

/-! ## Pasaje de una capacidad entre dos cells

La cell 0 crea un canal (handles 1 y 2), y crea otro (3 y 4). Como la cell 1
todavía no tiene nada, simulamos el arranque: el kernel le dio el extremo 2
del primer canal, que es lo que hace `init` con `cell_spawn`. -/

def boot : Sys :=
  let s := (runAll (empty 2) [(0, .create)]).1
  -- mover a mano el extremo `endpoint 0 true` de la cell 0 a la cell 1
  match s.cells[0]?, s.cells[1]? with
  | some c0, some c1 =>
    match c0.remove h2, c1.insert ⟨.endpoint 0 true, ENDPOINT_RIGHTS⟩ with
    | .ok (c0', _), .ok (c1', _) => { s with cells := [c0', c1'] }
    | _, _ => s
  | _, _ => s

def passing : Sys × List (Except Err (List Nat)) :=
  runAll boot [
    (0, .create),                 -- [0] la cell 0 crea el canal 1: handles 2 y 3
    (1, .recv h1),                -- [1] nada todavía: WouldBlock
    (0, .send h1 7 [h3]),         -- [2] envía el extremo `endpoint 1 true`
    (0, .send h1 8 [h3]),         -- [3] ya no lo tiene: BadHandle
    (1, .recv h1),                -- [4] la cell 1 lo recibe como handle 2
    (1, .dup h2 1),               -- [5] copia con solo SEND (bit 0): handle 3
    (1, .send h3 9 [h3]),         -- [6] esa copia no tiene GRANT: AccessDenied
    (1, .dup h3 511)              -- [7] ni DUPLICATE: no puede recuperar derechos
  ]

#guard errIs .wouldBlock (passing.2.getD 1 (.error .noMemory))
#guard isOk (passing.2.getD 2 (.error .noMemory))
#guard errIs .badHandle (passing.2.getD 3 (.error .noMemory))
#guard okIs [7, 2] (passing.2.getD 4 (.error .noMemory))
#guard okIs [3] (passing.2.getD 5 (.error .noMemory))
#guard errIs .accessDenied (passing.2.getD 6 (.error .noMemory))
#guard errIs .accessDenied (passing.2.getD 7 (.error .noMemory))

/-! ## Extremos que viajan en ciclos se liberan

Si una cell manda un extremo por su propio canal hacia la cola de ese mismo
extremo y suelta el resto de sus handles, el extremo queda referenciado solo
por su cola. Con conteo de referencias puro (`Arc`) nunca se liberaría. La
recolección por alcanzabilidad lo cierra y vacía su cola. -/

def selfLoop : Sys × List (Except Err (List Nat)) :=
  runAll (empty 1) [
    (0, .create),              -- handles 1 (lado 0) y 2 (lado 1)
    (0, .send h1 0 [h2]),      -- el lado 1 viaja en la cola del lado 1
    (0, .send h1 1 [])         -- el lado 1 ya es inalcanzable: PeerClosed
  ]

#guard isOk (selfLoop.2.getD 1 (.error .noMemory))
#guard errIs .peerClosed (selfLoop.2.getD 2 (.ok []))
#guard selfLoop.1.closed (.endpoint 0 true)
#guard (selfLoop.1.queue (.endpoint 0 true)).isEmpty

/-- Ciclo de dos canales: el lado 1 de cada uno viaja en la cola del lado 1
del otro. Ninguna comprobación local lo detecta; la recolección sí. -/
def twoCycle : Sys × List (Except Err (List Nat)) :=
  runAll (empty 1) [
    (0, .create),              -- canal A: handles 1 y 2
    (0, .create),              -- canal B: handles 3 y 4
    (0, .send h1 0 [4]),       -- B.1 viaja hacia A.1
    (0, .send h3 0 [h2]),      -- A.1 viaja hacia B.1: ciclo, ya inalcanzables
    (0, .drop h1),
    (0, .drop h3)
  ]

#guard twoCycle.2.all isOk
#guard (twoCycle.1.cells.map (·.caps.length)) == [0]
#guard [Obj.endpoint 0 false, .endpoint 0 true, .endpoint 1 false, .endpoint 1 true].all
  twoCycle.1.closed
#guard (twoCycle.1.queue (.endpoint 0 true)).isEmpty && (twoCycle.1.queue (.endpoint 1 true)).isEmpty

/-! ## Los handles viejos no reviven

La generación tiene 12 bits. Un slot que agota sus 4096 generaciones se
retira, en lugar de volver a la generación 0. Ver
`CSpace.stale_handle_never_revives` para la demostración general. -/

def noWrap : Bool := Id.run do
  let mut cs : CSpace Nat := CSpace.new 4
  if let .ok (cs', _) := cs.insert ⟨1, Rights.ofBits 3⟩ then cs := cs'
  if let .ok (cs', _) := cs.remove h1 then cs := cs'
  for _ in [0:5000] do
    if let .ok (cs', h) := cs.insert ⟨2, Rights.ofBits 3⟩ then
      if let .ok (cs'', _) := cs'.remove h then cs := cs''
  if let .ok (cs', _) := cs.insert ⟨3, Rights.ofBits 1⟩ then cs := cs'
  return match cs.get h1 with
    | .ok _ => false
    | .error _ => true

#guard noWrap

end Examples
end Nsos
