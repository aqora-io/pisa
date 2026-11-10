module

public import Lean.Elab.Frontend
public import Lean.Language.Lean
public import Pisa.Export
public import Pisa.Olean
public import Pisa.Report

/-!
`pisa build -R <root> -o <olean> [-D <option>=<value>]… --value-chars <n> <file>`:
compiles a snapshot module, as `lean -R <root> -o <olean> --json <file>` does,
and then, in the same process, says what the module declares, so that nothing
has to import it again.

It elaborates the file with `Lean.Elab.runFrontend`, as Lean's shell does,
which prints Lean's messages as JSON lines and, without errors, writes the
module's files. Then it prints:
- `PISA_IMPORTS [names]`, the modules the written module imports;
- for code outside the module system, `PISA_DECLS [descriptions]`, each
  declaration of the module as `Pisa.describe` gives it, from the same
  constants the verifier re-checks. Module-system code exports its theorems
  as axioms to the modules it imports, so only the verifier, which loads them
  whole, can describe it;
- `PISA_EXPORT [exports]`, as `Pisa.exportDecls` gives them, for what the
  module exports.

It exits with 1 when Lean reported errors, as `lean` does.
-/

public section

namespace Pisa.Build
open Lean System

/-- The option overrides Lean was built with, which its shell applies. -/
@[extern "lean_internal_get_option_overrides"]
opaque optionOverrides (_ : Unit) : Options

/-- The trust level up to which declarations may be added unchecked. -/
@[extern "lean_internal_get_believer_trust_level"]
opaque believerTrustLevel (_ : Unit) : UInt32

/-- Sets option `arg`, `<name>=<value>`, as the shell's `-D` does. -/
def setConfigOption (opts : Options) (arg : String) : IO Options := do
  match arg.splitOn "=" with
  | name :: val@(_ :: _) =>
    let name := name.toName
    let val := "=".intercalate val
    if let some decl := (← getOptionDecls).find? name then
      Language.Lean.setOption opts decl name val
    else
      -- Imports may register more options, which the elaborator checks.
      return opts.set name val
  | _ => throw <| .userError "invalid -D parameter, argument must contain '='"

structure Args where
  root : Option FilePath := none
  olean : Option FilePath := none
  defs : Array String := #[]
  valueChars : Nat := 4000
  file : Option FilePath := none

def parse : List String → Args → Except String Args
  | [], a => .ok a
  | "-R" :: r :: rest, a => parse rest { a with root := some r }
  | "-o" :: o :: rest, a => parse rest { a with olean := some o }
  | "-D" :: d :: rest, a => parse rest { a with defs := a.defs.push d }
  | "--value-chars" :: n :: rest, a => match n.toNat? with
    | some n => parse rest { a with valueChars := n }
    | none => .error s!"--value-chars takes a number, not {n}"
  | f :: rest, a => match a.file with
    | some _ => .error "expected one file"
    | none => parse rest { a with file := some f }

/-- The file's text as Lean's shell reads it: a first line `#lang lean4` is
blanked, and any other language refused. -/
def contents (file : FilePath) : IO String := do
  let text ← String.fromUTF8Lossy <$> IO.FS.readBinFile file
  match text.splitOn "\n" with
  | first :: rest =>
    if first.startsWith "#lang" then
      let lang := (first.drop 5).toString.trimAscii.toString
      if lang != "lean4" then
        throw <| .userError s!"unknown language '{lang}'"
      return "\n" ++ "\n".intercalate rest
    return text
  | [] => return text

def run (args : List String) : IO UInt32 := do
  let a ← match parse args {} with
    | .ok a => pure a
    | .error e => IO.eprintln e; return 2
  let (some root, some olean, some file) := (a.root, a.olean, a.file)
    | IO.eprintln "usage: pisa build -R <root> -o <olean> [-D <option>=<value>]… <file>"; return 2
  let mut opts : Options := {}
  for d in a.defs do
    opts ← setConfigOption opts d
  opts := opts.mergeBy (fun _ _ v => v) (optionOverrides ())
  let input ← contents file
  let mainModule ← moduleNameOfFileName file (some root)
  let some env ← Elab.runFrontend input opts file.toString mainModule
      (trustLevel := believerTrustLevel () + 1) (oleanFileName? := some olean)
      (jsonOutput := true)
    | return 1
  let parts ← readParts olean
  let isModule := parts[0]!.isModule
  emit "PISA_IMPORTS" (toJson (parts[0]!.imports.map (·.module)))
  let own := parts.back!.constNames
  if !isModule then
    -- What the verifier replays: it skips unsafe and partial constants.
    let kenv := env.toKernelEnv
    let names := own.filter fun n => match kenv.find? n with
      | some ci => !ci.isUnsafe && !ci.isPartial
      | none => false
    let ctx : Core.Context := { fileName := "", fileMap := default, maxHeartbeats := 0 }
    let (decls, _) ← (describeAll mainModule names).toIO ctx { env := .ofKernelEnv kenv }
    emit "PISA_DECLS" (Json.arr decls)
  -- What importers see: a module-system module's exported part.
  let (names, seen) := if isModule then (parts[0]!.constNames, env.setExporting true)
    else (own, env)
  let ctx : Core.Context := {
    fileName := file.toString, fileMap := default, options := opts
    maxHeartbeats := Core.getMaxHeartbeats opts }
  let (exports, _, _) ← (exportDecls names a.valueChars).toIO ctx { env := seen }
  emit "PISA_EXPORT" (Json.arr exports)
  return 0

end Pisa.Build
