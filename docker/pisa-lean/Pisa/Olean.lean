module

public import Lean.Environment
public import Lean.Util.Path

/-!
Reading a module's `.olean` files as written: its export part and, for a
module-system module, its server and private parts. Later parts share objects
with earlier ones, so they are read together, as Lean's own import does.
-/

public section

namespace Pisa
open Lean System

unsafe def readPartsUnsafe (olean : FilePath) : IO (Array ModuleData) := do
  let main ← CompactedRegion.read (α := ModuleData) olean #[]
  if !main.1.isModule then
    return #[main.1]
  let server ← CompactedRegion.read (α := ModuleData) (OLeanLevel.server.adjustFileName olean)
    #[main.2]
  let priv ← CompactedRegion.read (α := ModuleData) (OLeanLevel.private.adjustFileName olean)
    #[main.2, server.2]
  return #[main.1, server.1, priv.1]

/-- The parts of the module whose `.olean` is `olean`: the export part, then
for a module-system module its server and private parts. The last has every
constant of the module. The regions stay mapped until the process exits. -/
@[implemented_by readPartsUnsafe]
opaque readParts (olean : FilePath) : IO (Array ModuleData)

/-- Prints `<marker> <json>` on a line of its own, and flushes it, for the
worker that reads the output as it comes. -/
def emit (marker : String) (j : Json) : IO Unit := do
  let out ← IO.getStdout
  out.putStrLn s!"{marker} {j.compress}"
  out.flush

end Pisa
