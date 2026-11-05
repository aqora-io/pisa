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
  {"ok": true, "declarations": [{"name","kind","axioms","assumes","type_hash","value_hash"?,
                                 "type_refs","value_refs","generated"}]}
or `{"ok": false, "reason": "..."}` when the kernel rejects the module. It imports
only `Lean`, so it runs as a script without compiling Mathlib.

The declarations are as the worker's axiom report (`policy.rs`) gives them, and
must stay so: the same refs and hashes, computed the same way.
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

/-- What the report follows: constants of this module or of a snapshot. Lean's
and Mathlib's are fixed by the version. -/
def tracked (env : Environment) (m : Name) : Bool :=
  match env.getModuleIdxFor? m with
  | some i => (`PisaSnap).isPrefixOf (env.header.moduleNames[i.toNat]!)
  | none => true

/-- Auxiliaries the report leaves out; what uses one carries what it is. -/
def hidden (m : Name) : Bool := m.isInternal && !isPrivateName m

/-- A private name without its module, which differs between a module and its
replacement. -/
def norm (m : Name) : Name :=
  match privateToUserName? m with
  | some u => `_private ++ u
  | none => m

def hashOf (e : Expr) : UInt64 :=
  hash (e.replace fun x => match x with
    | .const m ls => if isPrivateName m then some (.const (norm m) ls) else none
    | _ => none)

/-- The tracked constants `e` uses, seeing through hidden ones, and its hash
with the hidden ones it reaches folded in. -/
def deep (env : Environment) (self : Name) (e : Expr) : UInt64 × List Name := Id.run do
  let mut seen : NameSet := {}
  let mut refs : NameSet := {}
  let mut reached : Array Name := #[]
  let mut todo : List Name := e.getUsedConstants.toList
  repeat
    match todo with
    | [] => break
    | m :: rest =>
      todo := rest
      if seen.contains m || !tracked env m then continue
      seen := seen.insert m
      if hidden m then
        reached := reached.push m
        if let some ci := env.find? m then
          todo := ci.type.getUsedConstants.toList ++ todo
          if let .defnInfo d := ci then
            todo := d.value.getUsedConstants.toList ++ todo
      else if m != self then
        refs := refs.insert m
  let mut h := hashOf e
  for m in reached.qsort (fun a b => a.toString < b.toString) do
    if let some ci := env.find? m then
      h := mixHash h (mixHash (hash (norm m)) (hashOf ci.type))
      if let .defnInfo d := ci then
        h := mixHash h (hashOf d.value)
  return (h, refs.toList)

def declJson (n : Name) (ci : ConstantInfo) : CoreM Json := do
  let env ← getEnv
  let axioms ← collectAxioms n
  -- Declared as `axiom` here or in a snapshot. Lean's own, and the ones it
  -- makes, such as `native_decide`'s (internal names), aren't assumptions.
  let assumes := axioms.filter fun a => tracked env a && !hidden a &&
    (match env.find? a with | some (.axiomInfo _) => true | _ => false)
  let (typeHash, typeRefs) := deep env n ci.type
  let (valueHash, valueRefs) : Json × List Name := match ci with
    | .defnInfo d => let (h, refs) := deep env n d.value; (Json.str (toString h), refs)
    | .inductInfo i => (Json.null, i.ctors ++ [mkRecName i.name])
    | _ => (Json.null, [])
  let generated := isReservedName env n || ((privateToUserName? n).map (·.isInternal)).getD false
  return Json.mkObj [
    ("name", toJson n), ("kind", toJson (kindOf ci)),
    ("axioms", toJson axioms), ("assumes", toJson assumes),
    ("type_hash", Json.str (toString typeHash)),
    ("value_hash", valueHash), ("type_refs", toJson typeRefs),
    ("value_refs", toJson valueRefs), ("generated", toJson generated)]

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
