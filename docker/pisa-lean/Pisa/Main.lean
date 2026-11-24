module

public import Pisa.Build
public import Pisa.Verify

/-!
The `pisa` executable, which the worker runs in a slot's sandbox:
- `pisa build …` compiles a snapshot module and describes it (`Pisa.Build`);
- `pisa verify M1 … Mk` re-checks snapshot modules through the kernel
  (`Pisa.Verify`).
-/

public section

open Lean

unsafe def main (args : List String) : IO UInt32 := do
  initSearchPath (← findSysroot)
  match args with
  | "build" :: rest =>
    enableInitializersExecution
    let code ← Pisa.Build.run rest
    -- As Lean's shell does, exit without freeing the environment.
    IO.Process.exit code.toUInt8
  | "verify" :: rest =>
    let code ← Pisa.Verify.run (rest.map String.toName).toArray
    IO.Process.exit code.toUInt8
  | _ =>
    IO.eprintln "usage: pisa build … | pisa verify MODULE…"
    return 2
