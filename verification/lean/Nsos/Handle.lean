/-!
# Handles y errores

Modelo de `nsos_abi::cap::CapHandle` (32 bits: `[generación:12 | slot:20]`) y
de los códigos de `nsos_abi::Error` que usan las capacidades y el IPC.
-/

namespace Nsos

/-- Bits de slot. -/
def SLOT_BITS : Nat := 20
/-- Slots por CSpace (`MAX_SLOTS`). -/
def MAX_SLOTS : Nat := 2 ^ 20
/-- Generaciones distintas (`1 << GEN_BITS`). -/
def GEN_MOD : Nat := 2 ^ 12

namespace Handle

/-- Slot de un handle crudo. -/
def slot (h : Nat) : Nat := h % MAX_SLOTS
/-- Generación de un handle crudo (los handles son `u32`, así que `< 4096`). -/
def gen (h : Nat) : Nat := h / MAX_SLOTS
/-- `CapHandle::new(slot, gen)` sin el chequeo de rango. -/
def mk (slot gen : Nat) : Nat := (gen % GEN_MOD) * MAX_SLOTS + slot
/-- `CapHandle::next_generation`. -/
def nextGen (g : Nat) : Nat := (g + 1) % GEN_MOD

theorem slot_mk {s g : Nat} (hs : s < MAX_SLOTS) : slot (mk s g) = s := by
  simp only [slot, mk, MAX_SLOTS, GEN_MOD] at *
  omega

theorem gen_mk {s g : Nat} (hs : s < MAX_SLOTS) : gen (mk s g) = g % GEN_MOD := by
  simp only [gen, mk, MAX_SLOTS, GEN_MOD] at *
  omega

end Handle

/-- Errores del ABI que aparecen en el modelo. -/
inductive Err where
  | badHandle | accessDenied | invalidArgs | wouldBlock | peerClosed | noMemory | badMessage
  deriving DecidableEq, Repr

/-- Nombre como lo imprime `{:?}` en Rust. -/
def Err.name : Err → String
  | .badHandle => "BadHandle" | .accessDenied => "AccessDenied"
  | .invalidArgs => "InvalidArgs" | .wouldBlock => "WouldBlock"
  | .peerClosed => "PeerClosed" | .noMemory => "NoMemory" | .badMessage => "BadMessage"

end Nsos
