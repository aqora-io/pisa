module

public import Lean.Elab.Command
public import Lean.PrettyPrinter
public import Lean.Linter.Deprecated

/-!
What the search service indexes of a snapshot module's declarations, and the
audit's review list shows of them: for each, `{name, kind, signature,
type_pp_full, type_explicit, type_refs, value_refs, docstring, deprecated_to,
value, private}`. `type_explicit` is the type with every argument shown,
instances too, cut at `chars` characters like `value`: where a replacement's
type prints like the original's but differs, a refusal shows it.

It skips what `search/exporter/PisaExport.lean` skips, internal and
auto-generated names, but not private ones, which the review list shows and
search leaves out (`private: true`). A definition's `value` is printed with
full names and no notation, which code can't dress up with notation of its
own, each `sorry` in it as `sorry`, and cut at `chars` characters. Each
declaration gets its own heartbeat budget, and one that fails to print is kept
with its kind and `error`.
-/

public section

namespace Pisa
open Lean Meta

/-- What kind of declaration `ci` is, as the export names it. -/
def exportKind (env : Environment) (ci : ConstantInfo) : String :=
  match ci with
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

/-- The export of `ci`, a declaration of kind `kind`, as it prints. -/
def printed (ci : ConstantInfo) (kind : String) (chars : Nat) : MetaM Json := do
  let env ← getEnv
  let n := ci.name
  let signature ← withOptions (fun o => o |>.setBool `pp.deepTerms true
      |>.set `pp.deepTerms.threshold (10000 : Nat) |>.set `pp.maxSteps (100000 : Nat)) do
    return (← PrettyPrinter.ppSignature n).fmt.pretty 120
  let typeFull ← withOptions (fun o => o |>.setBool `pp.notation false
      |>.setBool `pp.fullNames true |>.setBool `pp.deepTerms true) do
    return (← ppExpr ci.type).pretty 120
  -- Every argument shown, instances too: where two types that print alike
  -- differ, as when an instance is found by another path. Binders have their
  -- types, which an argument doesn't always show, as for `fun (x : N) => x`
  -- where `N` is an `abbrev` of the type `Eq` is given.
  let typeExplicit ← withOptions (fun o => o |>.setBool `pp.explicit true
      |>.setBool `pp.notation false |>.setBool `pp.fullNames true
      |>.setBool `pp.universes true |>.setBool `pp.deepTerms true
      |>.setBool `pp.funBinderTypes true |>.setBool `pp.letVarTypes true
      |>.set `pp.maxSteps (100000 : Nat)) do
    let text := (← ppExpr ci.type).pretty 120
    if text.length > chars then
      return String.ofList (text.toList.take chars) ++ " …"
    else
      return text
  let valueExpr? : Option Expr := match ci with
    | .defnInfo d => some d.value
    | .opaqueInfo d => some d.value
    | _ => none
  -- Without notation a `sorry` prints as `sorryAx` with its type and
  -- label, so each is marked with a constant that prints as `sorry`.
  let mark := `Pisa.Export.sorryMark
  let unsorried := valueExpr?.map fun v =>
    v.replace fun e => if e.isSorry then some (mkConst mark) else none
  let value? : Option String ← match unsorried with
    | some v => withOptions (fun o => o |>.setBool `pp.notation false
        |>.setBool `pp.fullNames true |>.set `pp.maxSteps (100000 : Nat)) do
          let text := ((← ppExpr v).pretty 120).replace (toString mark) "sorry"
          if text.length > chars then
            return some (String.ofList (text.toList.take chars) ++ " …")
          else
            return some text
    | none => pure none
  let typeRefs := ci.type.getUsedConstants.filter (· != n)
  let valueRefs := match ci.value? (allowOpaque := true) with
    | some v => v.getUsedConstantsAsSet.toArray.filter (fun r => r != n && !r.isInternalDetail)
    | none => #[]
  let doc : Option String := match ← findInternalDocString? env n with
    | some (.inl s) => some s
    | _ => none
  let deprecated := Linter.deprecatedAttr.getParam? env n
  return Json.mkObj [
    ("name", toJson n),
    ("kind", toJson kind),
    ("signature", toJson signature),
    ("type_pp_full", toJson typeFull),
    ("type_explicit", toJson typeExplicit),
    ("type_refs", toJson typeRefs),
    ("value_refs", toJson valueRefs),
    ("docstring", toJson doc),
    ("deprecated_to", toJson (deprecated.bind (·.newName?))),
    ("value", toJson value?),
    ("private", toJson (isPrivateName n))]

/-- The export of declaration `n`, if it is one to export. One that fails to
print, even for want of heartbeats, is kept with its kind and `error`, so
that the review list can say why it has no value. -/
def exportDecl (n : Name) (chars : Nat) : MetaM (Option Json) := do
  let env ← getEnv
  let some ci := env.find? n | return none
  if (privateToUserName n).isInternalDetail then return none
  if n matches .str _ "inj" || n matches .str _ "injEq" || n matches .str _ "noConfusionType" then
    return none
  if isAuxRecursor env n || isNoConfusion env n then return none
  if ← isRec n <||> isMatcher n then return none
  let kind := exportKind env ci
  Core.withCurrHeartbeats do
    tryCatchRuntimeEx (some <$> printed ci kind chars) fun e => do
      return some (Json.mkObj [
        ("name", toJson n),
        ("kind", toJson kind),
        ("error", toJson (← e.toMessageData.toString)),
        ("private", toJson (isPrivateName n))])

/-- The exports of the declarations `names`, in order. -/
def exportDecls (names : Array Name) (chars : Nat) : MetaM (Array Json) :=
  names.filterMapM (exportDecl · chars)

end Pisa
