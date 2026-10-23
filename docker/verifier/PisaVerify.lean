/-
Re-verifies one snapshot module through the Lean kernel, in a clean process, and
reports the axioms of its declarations. Run from the Lake project that pins the
libraries:

  lean --run PisaVerify.lean PisaSnap.s<id>

It reads the module's own constants straight from its `.olean` (without trusting
an elaborated environment), imports only the module's dependencies, and replays
the constants into that dependency-only environment with the kernel checking
every one (`Lean.Kernel.Environment.replay`). A declaration added during the
build with the kernel check skipped is rejected here. Axioms are then collected
by walking the re-checked constants, not from any precomputed extension state a
build could have forged.

It prints one line, `PISA_VERIFY <json>`:
  {"ok": true, "declarations": [{"name","kind","axioms","type_hash","value_hash"?}]}
or `{"ok": false, "reason": "..."}` when the kernel rejects the module. It imports
only `Lean`, so it runs as a script without compiling Mathlib.
-/
import Lean
open Lean Core

/-- The `.olean` files of a module: its export part, plus the server and private
parts for module-system modules. Mirrors the toolchain's private
`readModuleDataPartsOfMod`. -/
unsafe def modDataParts (mod : Name) : IO (Array ModuleData) := do
  let mFile ← findOLean mod
  unless (← mFile.pathExists) do
    throw <| IO.userError s!"object file '{mFile}' of module {mod} does not exist"
  let main ← CompactedRegion.read (α := ModuleData) mFile #[]
  unless main.1.isModule do
    return #[main.1]
  let server ← CompactedRegion.read (α := ModuleData) (OLeanLevel.server.adjustFileName mFile) #[]
  let priv ← CompactedRegion.read (α := ModuleData) (OLeanLevel.private.adjustFileName mFile) #[]
  return #[main.1, server.1, priv.1]

def kindOf : ConstantInfo → String
  | .thmInfo _ => "theorem"
  | .defnInfo _ => "def"
  | .axiomInfo _ => "axiom"
  | .opaqueInfo _ => "opaque"
  | .quotInfo _ => "quot"
  | .inductInfo _ => "inductive"
  | .ctorInfo _ => "constructor"
  | .recInfo _ => "recursor"

def declJson (n : Name) (ci : ConstantInfo) : CoreM Json := do
  let axioms ← collectAxioms n
  let valueHash := match ci with
    | .defnInfo d => Json.str (toString (hash d.value))
    | _ => Json.null
  return Json.mkObj [
    ("name", toJson n), ("kind", toJson (kindOf ci)),
    ("axioms", toJson axioms), ("type_hash", Json.str (toString (hash ci.type))),
    ("value_hash", valueHash)]

unsafe def main (args : List String) : IO UInt32 := do
  let some target := args.head?.map String.toName
    | IO.eprintln "usage: PisaVerify.lean MODULE"; return 1
  initSearchPath (← findSysroot)
  let out ← IO.getStdout
  let emit (j : Json) : IO Unit := out.putStrLn s!"PISA_VERIFY {j.compress}"
  let parts ← modDataParts target
  let data := parts.back!  -- the most private part has every own constant
  -- The module's own constants, read from its `.olean`, not from elaboration.
  let mut newConstants : Std.HashMap Name ConstantInfo := {}
  for n in data.constNames, ci in data.constants do
    newConstants := newConstants.insert n ci
  -- A trusted environment of just the module's dependencies.
  let base ← importModules data.imports {} (trustLevel := 1024)
  -- Re-check every own constant through the kernel; throws if any is rejected.
  let kenv ← try
    base.toKernelEnv.replay newConstants
  catch e =>
    emit (Json.mkObj [("ok", false), ("reason", Json.str s!"kernel re-check: {e}")])
    return 0
  let env := Environment.ofKernelEnv kenv
  let ctx : Core.Context := { fileName := "", fileMap := default, maxHeartbeats := 0 }
  let (decls, _) ← (CoreM.toIO · ctx { env }) do
    let mut decls : Array Json := #[]
    for n in data.constNames do
      if n.isInternal && !isPrivateName n then continue
      if let some ci := env.find? n then
        decls := decls.push (← declJson n ci)
    return decls
  emit (Json.mkObj [("ok", true), ("declarations", Json.arr decls)])
  return 0
