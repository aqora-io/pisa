module

public import Lean.Elab.Command
public import Lean.PrettyPrinter
public import Lean.Linter.Deprecated

/-!
What the search service indexes of a snapshot module's declarations, and the
audit's review list shows of them: for each, `{name, kind, signature,
type_pp_full, type_refs, value_refs, docstring, deprecated_to, value}`.

It skips what `search/exporter/PisaExport.lean` skips: internal, private and
auto-generated names. A definition's `value` is printed with full names and no
notation, which code can't dress up with notation of its own, and cut at
`chars` characters. Each declaration gets its own heartbeat budget, and one
that fails to print is left out.
-/

public section

namespace Pisa
open Lean Meta

/-- The export of declaration `n`, if it is one to export and prints. -/
def exportDecl (n : Name) (chars : Nat) : MetaM (Option Json) := do
  let env ← getEnv
  let some ci := env.find? n | return none
  if n.isInternalDetail || isPrivateName n then return none
  if n matches .str _ "inj" || n matches .str _ "injEq" || n matches .str _ "noConfusionType" then
    return none
  if isAuxRecursor env n || isNoConfusion env n then return none
  if ← isRec n <||> isMatcher n then return none
  Core.withCurrHeartbeats do try
      let signature ← withOptions (fun o => o |>.setBool `pp.deepTerms true
          |>.set `pp.deepTerms.threshold (10000 : Nat) |>.set `pp.maxSteps (100000 : Nat)) do
        return (← PrettyPrinter.ppSignature n).fmt.pretty 120
      let typeFull ← withOptions (fun o => o |>.setBool `pp.notation false
          |>.setBool `pp.fullNames true |>.setBool `pp.deepTerms true) do
        return (← ppExpr ci.type).pretty 120
      let valueExpr? : Option Expr := match ci with
        | .defnInfo d => some d.value
        | .opaqueInfo d => some d.value
        | _ => none
      let value? : Option String ← match valueExpr? with
        | some v => withOptions (fun o => o |>.setBool `pp.notation false
            |>.setBool `pp.fullNames true |>.set `pp.maxSteps (100000 : Nat)) do
              let text := (← ppExpr v).pretty 120
              if text.length > chars then
                return some (String.ofList (text.toList.take chars) ++ " …")
              else
                return some text
        | none => pure none
      let kind : String := match ci with
        | .axiomInfo _ => "axiom"
        | .defnInfo d =>
          if isInstanceCore env d.name then "instance"
          else if getReducibilityStatusCore env d.name == ReducibilityStatus.reducible then "abbrev"
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
      let typeRefs := ci.type.getUsedConstants.filter (· != n)
      let valueRefs := match ci.value? (allowOpaque := true) with
        | some v => v.getUsedConstantsAsSet.toArray.filter (fun r => r != n && !r.isInternalDetail)
        | none => #[]
      let doc : Option String := match ← findInternalDocString? env n with
        | some (.inl s) => some s
        | _ => none
      let deprecated := Linter.deprecatedAttr.getParam? env n
      return some (Json.mkObj [
        ("name", toJson n),
        ("kind", toJson kind),
        ("signature", toJson signature),
        ("type_pp_full", toJson typeFull),
        ("type_refs", toJson typeRefs),
        ("value_refs", toJson valueRefs),
        ("docstring", toJson doc),
        ("deprecated_to", toJson (deprecated.bind (·.newName?))),
        ("value", toJson value?)])
    catch _ => return none

/-- The exports of the declarations `names`, in order. -/
def exportDecls (names : Array Name) (chars : Nat) : MetaM (Array Json) :=
  names.filterMapM (exportDecl · chars)

/-- Logs `PISA_EXPORT <exports>` for the declarations of `m`, an imported
module, which the worker reads. -/
def exportModule (m : Name) (chars : Nat) : Elab.Command.CommandElabM Unit := do
  let env ← getEnv
  let some idx := env.getModuleIdx? m | throwError "{m} is not imported"
  let names := env.header.moduleData[idx.toNat]!.constNames
  let out ← Elab.Command.liftTermElabM (exportDecls names chars)
  logInfo (MessageData.ofFormat (.text ("PISA_EXPORT " ++ (Json.arr out).compress)))

end Pisa
