import Nsos

/-!
# Generador de trazas para los tests de Rust

Ejecuta el modelo de `CSpace` con secuencias pseudoaleatorias (deterministas)
de operaciones y escribe, para cada una, el resultado que predice el modelo.
`crates/kcore/tests/model_traces.rs` repite las mismas operaciones sobre la
`CSpace` real y exige resultados idénticos (handles, errores, derechos, len).

También genera `crates/contract/tests/data/contract_model.trace`: el
veredicto de `Contract.check` sobre contratos al azar y las tablas de los
contratos de `nsos_contract::protocols`. `crates/contract/tests/contract_model.rs`
exige que la `const fn` `check` de Rust dé lo mismo y que las tablas que
genera la macro sean estas.

Uso: `lake exe tracegen <cspace.trace> [<contract.trace>]`
-/

open Nsos

/-- Generador congruencial (determinista, igual en cualquier máquina). -/
structure Rng where
  state : UInt64

def Rng.next (g : Rng) : Nat × Rng :=
  let s := g.state * 6364136223846793005 + 1442695040888963407
  ((s >>> 33).toNat, ⟨s⟩)

abbrev GenM := StateT Rng (StateT (Array String) IO)

def rand (n : Nat) : GenM Nat := do
  let (x, g) := (← get).next
  set g
  return if n = 0 then 0 else x % n

def emit (line : String) : GenM Unit := do
  liftM (m := StateT (Array String) IO) (modify (·.push line))

def errOr {β : Type} (r : Except Err β) (f : β → String) : String :=
  match r with
  | .error e => s!"err {e.name}"
  | .ok v => s!"ok {f v}"

def capStr (c : Cap Nat) : String := s!"{c.obj}:{c.rights.bits}"

def listStr {β : Type} (f : β → String) : List β → String
  | [] => "-"
  | l => ",".intercalate (l.map f)

/-- Elige un handle: casi siempre uno ya emitido (vivo o viejo), a veces uno
fabricado. -/
def pickHandle (pool : Array Nat) : GenM Nat := do
  let k ← rand 10
  if k < 8 && pool.size > 0 then
    return pool[← rand pool.size]!
  else if k = 8 then
    -- slot plausible con generación arbitraria
    return Handle.mk (1 + (← rand 12)) (← rand 4)
  else
    return [0, 4294967295, 1048576, 1, 2, 7][← rand 6]!

def randRights : GenM Nat := do
  match ← rand 4 with
  | 0 => return 511
  | 1 => return 64 + (← rand 512) % 448   -- con DUPLICATE la mayoría de las veces
  | _ => rand 512

def trace (limit len : Nat) : GenM Unit := do
  emit s!"T {limit}"
  let mut cs : CSpace Nat := CSpace.new limit
  let mut pool : Array Nat := #[]
  let mut obj := 1
  for _ in [0:len] do
    let k ← rand 100
    let line ←
      if k < 25 then do
        let r ← randRights
        let res := cs.insert ⟨obj, Rights.ofBits r⟩
        let l := s!"insert {obj} {r} => {errOr res (toString ·.2)}"
        obj := obj + 1
        if let .ok (cs', h) := res then cs := cs'; pool := pool.push h
        pure l
      else if k < 37 then do
        let h ← pickHandle pool
        pure s!"get {h} => {errOr (cs.get h) capStr}"
      else if k < 47 then do
        let h ← pickHandle pool
        let need ← rand 512
        pure s!"getw {h} {need} => {errOr (cs.getWith h (Rights.ofBits need)) capStr}"
      else if k < 60 then do
        let h ← pickHandle pool
        let res := cs.remove h
        if let .ok (cs', _) := res then cs := cs'
        pure s!"remove {h} => {errOr res (capStr ·.2)}"
      else if k < 75 then do
        let h ← pickHandle pool
        let mask ← rand 512
        let res := cs.dup h (Rights.ofBits mask)
        if let .ok (cs', h') := res then cs := cs'; pool := pool.push h'
        pure s!"dup {h} {mask} => {errOr res (toString ·.2)}"
      else if k < 85 then do
        let n ← rand 4
        let mut hs : List Nat := []
        for _ in [0:n] do hs := hs ++ [← pickHandle pool]
        let need := [32, 0, 1, 33][← rand 4]!
        let res := cs.removeAll hs (Rights.ofBits need)
        if let .ok (cs', _) := res then cs := cs'
        pure s!"removeall {need} {listStr toString hs} => {errOr res (listStr capStr ·.2)}"
      else if k < 95 then do
        let n ← rand 4
        let mut caps : List (Cap Nat) := []
        for _ in [0:n] do
          caps := caps ++ [⟨obj, Rights.ofBits (← randRights)⟩]
          obj := obj + 1
        let res := cs.insertAll caps
        if let .ok (cs', hs) := res then cs := cs'; pool := pool ++ hs.toArray
        pure s!"insertall {listStr capStr caps} => {errOr res (listStr toString ·.2)}"
      else if k < 98 then do
        let (cs', out) := cs.clear
        cs := cs'
        pure s!"clear => ok {listStr capStr out}"
      else do
        -- reuso intenso de un mismo slot
        let n ← rand 40
        let mut last := 0
        let mut ok := true
        for _ in [0:n] do
          match cs.insert ⟨obj, Rights.ofBits 3⟩ with
          | .ok (cs', h) =>
            match cs'.remove h with
            | .ok (cs'', _) => cs := cs''; last := h; pool := pool.push h
            | .error _ => ok := false
          | .error _ => ok := false
        pure s!"cycle {n} {obj} 3 => {if ok then s!"ok {last}" else "err NoMemory"}"
    emit s!"{line} len={cs.live}"

/-- Traza fija: después de 4096 reusos, el slot se retira y el handle original
sigue sin valer (la generación tiene 12 bits y no da la vuelta). -/
def wrapTrace : GenM Unit := do
  emit "T 4"
  emit "insert 1 3 => ok 1 len=1"
  let mut cs : CSpace Nat := CSpace.new 4
  if let .ok (cs', _) := cs.insert ⟨1, Rights.ofBits 3⟩ then cs := cs'
  if let .ok (cs', _) := cs.remove 1 then cs := cs'
  emit "remove 1 => ok 1:3 len=0"
  let mut last := 0
  for _ in [0:5000] do
    if let .ok (cs', h) := cs.insert ⟨2, Rights.ofBits 3⟩ then
      if let .ok (cs'', _) := cs'.remove h then cs := cs''; last := h
  emit s!"cycle 5000 2 3 => ok {last} len={cs.live}"
  let res := cs.insert ⟨3, Rights.ofBits 1⟩
  if let .ok (cs', _) := res then cs := cs'
  emit s!"insert 3 1 => {errOr res (toString ·.2)} len={cs.live}"
  emit s!"get 1 => {errOr (cs.get 1) capStr} len={cs.live}"

/-! ## Contratos -/

def dirStr : Contract.Dir → String
  | .ab => "ab"
  | .ba => "ba"

def transStr (c : Contract.Contract) : String :=
  " ".intercalate (c.trans.map fun t => s!"{t.src},{dirStr t.dir},{t.label},{t.dst}")

def checkStr (c : Contract.Contract) : String :=
  match Contract.check c with
  | some k => s!"some {k}"
  | none => "none"

/-- Un contrato al azar. Casi siempre respeta la dirección de cada estado y
no repite etiquetas, para que salgan tanto aceptados como rechazados. -/
def randContract : GenM Contract.Contract := do
  let ns := 1 + (← rand 5)
  let nt ← rand 7
  let mut dirs : Array Contract.Dir := #[]
  for _ in [0:ns] do
    dirs := dirs.push (if (← rand 2) = 0 then .ab else .ba)
  let mut ts : List Contract.Trans := []
  for _ in [0:nt] do
    let src ← rand ns
    let dir ← if (← rand 10) < 8 then pure dirs[src]! else
      pure (if (← rand 2) = 0 then .ab else .ba)
    let label ← rand 3
    let dst ← rand ns
    ts := ts ++ [⟨src, dir, label, dst⟩]
  return ⟨← rand ns, ts⟩

def contractTraces : GenM Unit := do
  emit "# Generado por `lake exe tracegen` (verification/lean). No editar a mano."
  emit "# C <inicio> <origen,dir,etiqueta,destino>... => some <cota> | none   (Contract.check)"
  emit "# P <nombre> <inicio> <transiciones>...   (tablas de nsos_contract::protocols)"
  for c in [Contract.mixed, Contract.nondet, Contract.flood, Contract.echoOld,
            Contract.echo, Contract.note] do
    emit s!"C {c.start} {transStr c} => {checkStr c}"
  for _ in [0:400] do
    let c ← randContract
    emit s!"C {c.start} {transStr c} => {checkStr c}"
  emit s!"P echo {Contract.echo.start} {transStr Contract.echo}"
  emit s!"P note {Contract.note.start} {transStr Contract.note}"

def writeLines (path : String) (gen : GenM Unit) : IO Unit := do
  let (((), _), lines) ← (gen.run ⟨20261119⟩).run #[]
  IO.FS.writeFile path ("\n".intercalate lines.toList ++ "\n")

def main (args : List String) : IO UInt32 := do
  let path := args.headD "/dev/stdout"
  let gen : GenM Unit := do
    emit "# Generado por `lake exe tracegen` (verification/lean). No editar a mano."
    emit "# Formato: <op> <args> => ok <resultado> | err <Error>  len=<capacidades vivas>"
    for i in [0:120] do
      let limit := [2, 3, 5, 8, 16, 1024][i % 6]!
      trace limit (20 + i % 40)
    wrapTrace
  writeLines path gen
  if let some p := args[1]? then writeLines p contractTraces
  return 0
