/-
Exports every user-facing declaration of an environment as one JSON line, for
pisa-search to index. Run from the Lake project that pins the libraries:

  lake env lean --run PisaExport.lean --env mathlib-physlib Mathlib Physlib > decls.ndjson

Lines are `{"record":"decl",...}` per declaration, sorted by name, then
`{"record":"module",...}` per module with a module docstring. Internal,
auto-generated and private declarations are skipped, as Loogle and doc-gen4 do,
and so are tooling libraries unless `--all` is given.

It imports only `Lean`, so it runs as a script without compiling Mathlib.
-/
import Lean
open Lean Core Meta

/-- Libraries that are tooling rather than mathematics or physics. -/
def toolingLibraries : List Name :=
  [`Lean, `Lake, `Aesop, `Qq, `ProofWidgets, `ImportGraph, `LeanSearchClient, `Plausible, `Cli]

/-- Mathlib's `Name.isBlackListed`, from Lean's completion server, plus private names. -/
def isBlackListed (env : Environment) (n : Name) : MetaM Bool := do
  if n == ``sorryAx || isPrivateName n || n.isInternal then return true
  if n matches .str _ "inj" then return true
  if n matches .str _ "noConfusionType" then return true
  pure <| n.isInternalDetail || isAuxRecursor env n || isNoConfusion env n
    <||> isRec n <||> isMatcher n

/-- Each constant's module. Built once: `getModuleIdxFor?` costs tens of
microseconds per call, which adds up over a million constants. -/
def moduleMap (env : Environment) : Std.HashMap Name Name := Id.run do
  let header := env.header
  let mut map : Std.HashMap Name Name := {}
  for h : i in [0:header.moduleData.size] do
    let mod := header.moduleNames[i]!
    for n in header.moduleData[i].constNames do
      map := map.insert n mod
  return map

def kindOf (env : Environment) (ci : ConstantInfo) : String :=
  match ci with
  | .axiomInfo _ => "axiom"
  | .defnInfo d =>
    if isInstanceCore env d.name then "instance"
    else if getReducibilityStatusCore env d.name == .reducible then "abbrev"
    else "def"
  | .thmInfo _ => "theorem"
  | .opaqueInfo _ => "opaque"
  | .quotInfo _ => "quot"
  | .inductInfo i =>
    if isClass env i.name then "class"
    else if isStructure env i.name then "structure"
    else "inductive"
  | .ctorInfo _ => "constructor"
  | .recInfo _ => "recursor"

/-- Libraries whose builds reject `sorry` (warnings are errors), so their
declarations are sorry-free without walking their proofs. -/
def sorryFreeLibraries : List Name :=
  [`Init, `Std, `Lean, `Lake, `Batteries, `Mathlib, `Aesop, `Qq, `ProofWidgets, `ImportGraph,
   `LeanSearchClient, `Plausible, `Cli]

/-- Whether each constant depends, transitively, on `sorryAx`. Walks every
constant outside `sorryFreeLibraries`, auxiliary ones included, since a `sorry`
often sits in a `_proof_n` or `match_n` helper. Iterative, so long dependency
chains can't overflow the stack. -/
def sorryClosure (env : Environment) (modules : Std.HashMap Name Name) :
    Std.HashMap Name Bool := Id.run do
  let mut memo : Std.HashMap Name Bool := {}
  for (n, _) in env.constants.toList do
    if sorryFreeLibraries.contains (modules.getD n .anonymous).getRoot then
      memo := memo.insert n false
  -- `sorryAx` itself lives in `Init`.
  memo := memo.insert ``sorryAx true
  for (root, _) in env.constants.toList do
    if memo.contains root then continue
    -- Post-order DFS: a frame is (name, expanded?). `entered` breaks the
    -- cycles of unsafe recursive definitions, which refer to themselves.
    let mut stack : Array (Name × Bool) := #[(root, false)]
    let mut entered : Std.HashSet Name := {}
    while h : stack.size > 0 do
      let (n, expanded) := stack[stack.size - 1]
      stack := stack.pop
      if memo.contains n then continue
      let refs := match env.find? n with
        | some ci => ci.getUsedConstantsAsSet.toList.filter (· != n)
        | none => []
      if expanded then
        let uses := n == ``sorryAx || refs.any fun r => memo.getD r false
        memo := memo.insert n uses
      else if !entered.contains n then
        entered := entered.insert n
        stack := stack.push (n, true)
        for r in refs do
          unless memo.contains r || entered.contains r do stack := stack.push (r, false)
  return memo

/-- Names written as literals in an expression, e.g. the dependency lists of
Physlib's `informal_lemma`s. -/
partial def nameLiterals (e : Expr) : Array Name :=
  let rec lit? : Expr → Option Name
    | .const ``Name.anonymous _ => some .anonymous
    | .app (.app (.const ``Name.mkStr _) p) (.lit (.strVal s)) => (lit? p).map (·.str s)
    | e =>
      let args := e.getAppArgs
      if e.getAppFn.isConstOf ``Name.mkStr1 || e.getAppFn.isConstOf ``Name.mkStr2 ||
          e.getAppFn.isConstOf ``Name.mkStr3 || e.getAppFn.isConstOf ``Name.mkStr4 ||
          e.getAppFn.isConstOf ``Name.mkStr5 || e.getAppFn.isConstOf ``Name.mkStr6 ||
          e.getAppFn.isConstOf ``Name.mkStr7 || e.getAppFn.isConstOf ``Name.mkStr8 then
        args.foldlM (init := Name.anonymous) fun acc a => match a with
          | .lit (.strVal s) => some (acc.str s)
          | _ => none
      else none
  let rec go (e : Expr) (acc : Array Name) : Array Name :=
    match lit? e with
    | some n => if n.isAnonymous then acc else acc.push n
    | none => match e with
      | .app f a => go a (go f acc)
      | .lam _ t b _ | .forallE _ t b _ => go b (go t acc)
      | .letE _ t v b _ => go b (go v (go t acc))
      | .mdata _ b | .proj _ _ b => go b acc
      | _ => acc
  go e #[]

def isInformal (ci : ConstantInfo) : Bool :=
  match ci.type.getAppFn with
  | .const (.str _ s) _ => s == "InformalDefinition" || s == "InformalLemma"
  | _ => false

def fmt (f : FormatWithInfos) : String := f.fmt.pretty 120

def names (ns : Array Name) : Json := toJson (ns.map toString)

/-- The record for one declaration, or `none` if Lean fails on it. -/
def exportDecl (env : Environment) (envLabel : String) (modules : Std.HashMap Name Name)
    (kept : Std.HashSet Name)
    (sorried : Std.HashMap Name Bool) (n : Name) (ci : ConstantInfo) : MetaM (Option Json) := do
  try
    let signature ← withOptions (fun o => o |>.setBool `pp.deepTerms true
        |>.set `pp.deepTerms.threshold (10000 : Nat) |>.set `pp.maxSteps (100000 : Nat)) do
      return fmt (← PrettyPrinter.ppSignature n)
    let typeFull ← withOptions (fun o => o |>.setBool `pp.notation false
        |>.setBool `pp.fullNames true |>.setBool `pp.deepTerms true) do
      return (← ppExpr ci.type).pretty 120
    let typeRefs := ci.type.getUsedConstants.filter (· != n)
    let valueRefs := match ci.value? (allowOpaque := true) with
      | some v => v.getUsedConstantsAsSet.toArray.filter (fun r => r != n && kept.contains r)
      | none => #[]
    let module := modules.getD n .anonymous
    let doc ← (findDocString? env n : IO _)
    let ranges ← findDeclarationRanges? n
    let deprecated := Linter.deprecatedAttr.getParam? env n
    let conclKeys ← if ci matches .thmInfo _ then
        try
          let (_, _, concl) ← forallMetaTelescopeReducing ci.type
          let keys ← DiscrTree.mkPath concl
          pure (keys.map fun k => (format k).pretty)
        catch _ => pure #[]
      else pure #[]
    let informal := isInformal ci
    let informalDeps := if informal then
        (ci.value?.map nameLiterals).getD #[] |>.filter (· != n)
      else #[]
    let attrs := #[
        if isInstanceCore env n then some "instance" else none,
        if (simpExtension.getState env).isLemma (.decl n) then some "simp" else none,
        if deprecated.isSome then some "deprecated" else none].filterMap id
    return some <| Json.mkObj [
      ("record", "decl"),
      ("env", envLabel),
      ("name", toString n),
      ("kind", kindOf env ci),
      ("module", toString module),
      ("library", toString module.getRoot),
      ("signature", signature),
      ("type_pp_full", typeFull),
      ("type_refs", names typeRefs),
      ("value_refs", names valueRefs),
      ("docstring", toJson doc),
      ("attrs", toJson attrs),
      ("range", match ranges with
        | some r => Json.mkObj [("start", r.range.pos.line), ("end", r.range.endPos.line)]
        | none => Json.null),
      ("deprecated_to", toJson ((deprecated.bind (·.newName?)).map toString)),
      ("uses_sorry", toJson (sorried.getD n false)),
      ("informal", toJson informal),
      ("informal_deps", names informalDeps),
      ("concl_keys", toJson conclKeys)]
  catch e =>
    IO.eprintln s!"skipping {n}: {← e.toMessageData.toString}"
    return none

unsafe def main (args : List String) : IO UInt32 := do
  let rec parse : List String → String × Bool × List Name → String × Bool × List Name
    | "--env" :: label :: rest, (_, all, mods) => parse rest (label, all, mods)
    | "--all" :: rest, (label, _, mods) => parse rest (label, true, mods)
    | m :: rest, (label, all, mods) => parse rest (label, all, mods ++ [m.toName])
    | [], acc => acc
  let (envLabel, all, mods) := parse args ("default", false, [])
  if mods.isEmpty then
    IO.eprintln "usage: PisaExport.lean [--env LABEL] [--all] MODULE..."
    return 1
  initSearchPath (← findSysroot)
  enableInitializersExecution
  let env ← importModules (mods.toArray.map ({ module := · })) {} (trustLevel := 1024)
    (loadExts := true)
  let ctx : Core.Context := { fileName := "", fileMap := default, maxHeartbeats := 0 }
  let out ← IO.getStdout
  let start ← IO.monoMsNow
  let modules := moduleMap env
  let (count, _) ← (CoreM.toIO · ctx { env }) do
    let mut kept : Array (Name × ConstantInfo) := #[]
    for (n, ci) in env.constants.toList do
      let lib := (modules.getD n .anonymous).getRoot
      if !all && toolingLibraries.contains lib then continue
      if ← MetaM.run' (isBlackListed env n) then continue
      kept := kept.push (n, ci)
    kept := kept.qsort (fun a b => a.1.toString < b.1.toString)
    let keptSet := Std.HashSet.ofArray (kept.map (·.1))
    IO.eprintln s!"{kept.size} declarations to export; computing sorry closure"
    (← IO.getStderr).flush
    let sorried := sorryClosure env modules
    IO.eprintln s!"{(sorried.toList.filter (·.2)).length} constants depend on sorry"
    (← IO.getStderr).flush
    let mut count := 0
    -- Fresh Meta state per chunk keeps caches from growing across the run.
    for i in [0:kept.size:1000] do
      let chunk := kept.extract i (i + 1000)
      MetaM.run' do
        for (n, ci) in chunk do
          if let some json ← exportDecl env envLabel modules keptSet sorried n ci then
            out.putStrLn json.compress
      count := count + chunk.size
      if count % 20000 < 1000 then
        IO.eprintln s!"{count}/{kept.size}"
        (← IO.getStderr).flush
    for mod in env.header.moduleNames do
      let docs := getModuleDoc? env mod |>.getD #[]
      if !all && toolingLibraries.contains mod.getRoot then continue
      unless docs.isEmpty do
        let text := "\n\n".intercalate (docs.map (·.doc)).toList
        out.putStrLn (Json.mkObj [("record", "module"), ("env", envLabel),
          ("name", toString mod), ("doc", text)]).compress
    return count
  IO.eprintln s!"exported {count} declarations in {(← IO.monoMsNow) - start} ms"
  return 0
