/-
Lists `to_additive` pairs (e.g. `mul_comm` ↔ `add_comm`) as
`{"record":"additive","name":...,"sibling":...}` lines, so search can show a
pair as one hit. Separate from PisaExport.lean because it needs Mathlib's
translation data; run it after the main export:

  lake env lean --run ToAdditive.lean Mathlib Physlib >> decls.ndjson
-/
import Lean
import Mathlib.Tactic.Translate.ToAdditive
open Lean Mathlib.Tactic

unsafe def main (args : List String) : IO UInt32 := do
  if args.isEmpty then
    IO.eprintln "usage: ToAdditive.lean MODULE..."
    return 1
  initSearchPath (← findSysroot)
  enableInitializersExecution
  let env ← importModules (args.toArray.map ({ module := ·.toName })) {} (trustLevel := 1024)
    (loadExts := true)
  let out ← IO.getStdout
  let mut count := 0
  for (n, _) in env.constants.toList do
    if n.isInternal || isPrivateName n then continue
    let some sibling := Translate.findTranslationName? env ToAdditive.data n | continue
    unless env.contains sibling do continue
    out.putStrLn (Json.mkObj [("record", "additive"), ("name", toString n),
      ("sibling", toString sibling)]).compress
    count := count + 1
  IO.eprintln s!"{count} to_additive pairs"
  return 0
