module

public import Lean.Replay
public import Pisa.Olean
public import Pisa.Report

/-!
`pisa verify M1 … Mk`: re-checks snapshot modules through the Lean kernel, in
a clean process, and reports the axioms of their declarations. The modules
come imports first.

For each module it reads its own constants straight from its `.olean` files,
not from an elaborated environment. It imports, once, everything the modules
import besides each other, at the private level, so a theorem is a theorem.
Then it replays each module's constants in turn into that environment, the
kernel checking every one (`Lean.Kernel.Environment.replay`), so each later
module is checked against the earlier ones. A declaration added during a build
with the kernel check skipped is rejected here. Axioms are collected by
walking the re-checked constants, not from extension state a build could have
forged, and each module is described as soon as it is replayed, before the
modules after it exist. No initializer of the imports runs.

It prints `PISA_IMPORTED {}` once the imports are loaded, then for each module
`PISA_VERIFY {"module", "ok": true, "declarations": [...]}`, each declaration
as `Pisa.describe` gives it, or `{"module", "ok": false, "reason"}` when the
kernel rejects it, which ends the run. A failure that is no verdict on the
code, reading a module's files, importing what they import or describing a
module, prints `PISA_FAILED {"reason"}` and ends the run with exit code 1.
-/

public section

namespace Pisa.Verify
open Lean Core

/-- Fails the run on `module` with `reason`. -/
def reject (module : Name) (reason : String) : IO UInt32 := do
  emit "PISA_VERIFY" (Json.mkObj [("module", toJson module), ("ok", false),
    ("reason", Json.str reason)])
  return 0

/-- Ends the run without a verdict: `reason` is no verdict on the code. -/
def failed (reason : String) : IO UInt32 := do
  emit "PISA_FAILED" (Json.mkObj [("reason", Json.str reason)])
  return 1

def run (modules : Array Name) : IO UInt32 := do
  -- The most private part of each has every constant of the module.
  let mut own : Array (Name × ModuleData) := #[]
  for m in modules do
    try
      let parts ← readParts (← findOLean m)
      own := own.push (m, parts.back!)
    catch e =>
      return ← failed s!"reading `{m}`: {e}"
  let chain := modules.foldl (·.insert ·) ({} : NameSet)
  let mut outside : Array Import := #[]
  let mut seen : NameSet := {}
  for (_, data) in own do
    for i in data.imports do
      if !chain.contains i.module && !seen.contains i.module then
        seen := seen.insert i.module
        outside := outside.push i
  let base ← try importModules outside {} (trustLevel := 1024)
    catch e => return ← failed s!"importing what the modules import: {e}"
  -- A module imported from outside would come in unchecked.
  if let some m := modules.find? (base.header.moduleNames.contains ·) then
    return ← reject m "it is also imported by a module outside the re-check"
  emit "PISA_IMPORTED" (Json.mkObj [])
  let mut kenv := base.toKernelEnv
  -- Which module of the chain declared each name replayed so far.
  let mut declaredBy : Std.HashMap Name Name := {}
  for (m, data) in own do
    let mut constants : Std.HashMap Name ConstantInfo := {}
    for n in data.constNames, ci in data.constants do
      constants := constants.insert n ci
    try
      kenv ← kenv.replay constants
    catch e =>
      return ← reject m s!"kernel re-check: {e}"
    -- The replay keeps a theorem whose name and type are there already, from
    -- an import or an earlier module, and skips this module's: its proof
    -- would be neither checked nor described.
    for n in data.constNames, ci in data.constants do
      let some found := kenv.find? n | continue
      if found.value? (allowOpaque := true) != ci.value? (allowOpaque := true) then
        let other := match declaredBy[n]? with
          | some o => s!"`{o}`"
          | none => match base.getModuleIdxFor? n with
            | some i => s!"`{base.header.moduleNames[i.toNat]!}`"
            | none => "an import"
        return ← reject m
          s!"`{n}` is also declared by {other}, with another proof: no module can import both"
    for n in data.constNames do
      declaredBy := declaredBy.insert n m
    let ctx : Core.Context := { fileName := "", fileMap := default, maxHeartbeats := 0 }
    let env := Environment.ofKernelEnv kenv
    let described ←
      (describeAll m data.constNames (fromKernel := true) declaredBy).toIO ctx { env }
        |>.toBaseIO
    let (decls, _) ← match described with
      | .ok d => pure d
      | .error e => return ← failed s!"describing `{m}`: {e}"
    emit "PISA_VERIFY" (Json.mkObj [("module", toJson m), ("ok", true),
      ("declarations", Json.arr decls)])
  return 0

end Pisa.Verify
