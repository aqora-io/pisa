module

public import Lean.Elab.Command
public import Lean.Util.CollectAxioms
public import Pisa.Canon

/-!
How the worker describes a declaration, for its axiom report (`run_cmd
_root_.Pisa.reportNew` after code, `_root_.Pisa.reportModule` for an imported
snapshot) and for the kernel verifier (`docker/verifier/PisaVerify.lean`),
which call the same `describe`.

A description is `{name, kind, axioms, assumes, type, value, type_refs,
value_refs, generated}`:
- `axioms`, all the axioms it depends on, and `assumes`, those declared as
  `axiom` in this code or a snapshot;
- `type`, and for a definition `value`, their canonical text (see
  `Pisa.Canon`), which the worker hashes;
- `type_refs` and `value_refs`, the constants of this code and of snapshots
  they use (an inductive's "value" is its constructors and recursor);
- `generated`, for what Lean makes on demand, such as equation lemmas, which a
  replacement needn't have.

Internal auxiliaries, such as the `f._unary` holding the body of a
well-founded definition, aren't described: what uses one has their text
follow its own, and its refs go through to what they use. Private names are
written without their module, which differs between a module and its
replacement.
-/

public section

namespace Pisa
open Lean Canon

/-- Constants of this code or of a snapshot: Lean's and Mathlib's are fixed by
the version. -/
def tracked (env : Environment) (m : Name) : Bool :=
  match env.getModuleIdxFor? m with
  | some i => (`PisaSnap).isPrefixOf (env.header.moduleNames[i.toNat]!)
  | none => true

/-- Auxiliaries the description leaves out; what uses one carries what it is. -/
def hidden (m : Name) : Bool := m.isInternal && !isPrivateName m

/-- A private name without its module. -/
def norm (m : Name) : Name :=
  match privateToUserName? m with
  | some u => `_private ++ u
  | none => m

/-- A constant's line: its name, private ones without their module, and its
universe levels. -/
def constant (n : Name) (ls : List Level) : Except String String := do
  let ls ← ls.mapM levelText
  return s!"{nameText (norm n)} [{" ".intercalate ls}]"

/-- Whether definition `d` recurses, by its shape: structurally, through a
`brecOn`, or by well-founded recursion, through `WellFounded.fix` or the
auxiliary `_unary` or `_mutual` that holds it. -/
def recurses (d : DefinitionVal) : Bool :=
  d.value.getUsedConstants.any fun c =>
    c == ``WellFounded.fix || c matches .str _ "brecOn" ||
    match privateToUserName c with
    | .str _ s => s == "_unary" || s == "_mutual"
    | _ => false

/-- What Lean made on demand, which a replacement needn't have, by name and
shape alone, so that the verifier, which has no more than the constants,
finds the same; only names Lean reserves, which code can't declare itself:
- a definition's equations (`f.eq_1`, `f.eq_def`, `f.eq_unfold`) and case
  split (`f.fun_cases`), and a recursive one's induction principles
  (`f.induct`, `f.mutual_induct`; Lean lets code declare these for one that
  doesn't recurse);
- a constant's congruence lemmas (`f.congr_simp`, `f.hcongr_1`), and
  `ctorIdx.hinj`;
- a private internal name. -/
def generated (env : Environment) (n : Name) : Bool :=
  ((privateToUserName? n).map (·.isInternal)).getD false ||
  match n with
  | .str p s =>
    let defn := env.find? p
    let numbered (pre : String) := pre.isPrefixOf s && (s.drop pre.length).toString.isNat
    (["eq_def", "eq_unfold", "fun_cases", "fun_cases_unfolding"].contains s || numbered "eq_")
      && defn matches some (.defnInfo _) ||
    ["induct", "induct_unfolding", "mutual_induct", "mutual_induct_unfolding"].contains s
      && (match defn with | some (.defnInfo d) => recurses d | _ => false) ||
    (s == "congr_simp" || numbered "hcongr_") && defn.isSome ||
    s == "hinj" && p matches .str _ "ctorIdx" && defn.isSome
  | _ => false

/-- The tracked constants `e` uses, seeing through hidden ones, and the hidden
ones it reaches, by name. -/
def reach (env : Environment) (self : Name) (e : Expr) : Array Name × Array Name := Id.run do
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
  return (refs.toArray.qsort Name.lt, reached.qsort (fun a b => a.toString < b.toString))

/-- The text of `e`, then of each hidden constant it reaches, and its refs. -/
def text (env : Environment) (self : Name) (e : Expr) : Except String (String × Array Name) := do
  let (refs, reached) := reach env self e
  let ((), t) ← (do
      discard <| write constant e
      for m in reached do
        let some ci := env.find? m | continue
        mark s!"H {nameText (norm m)}"
        discard <| write constant ci.type
        if let .defnInfo d := ci then
          discard <| write constant d.value
    : TextM Unit).run {}
  return (t.render, refs)

def kindOf : ConstantInfo → String
  | .thmInfo _ => "theorem"
  | .defnInfo _ => "def"
  | .axiomInfo _ => "axiom"
  | .opaqueInfo _ => "opaque"
  | .quotInfo _ => "quot"
  | .inductInfo _ => "inductive"
  | .ctorInfo _ => "constructor"
  | .recInfo _ => "recursor"

/-- Whether `n` is described: auxiliaries aren't, unless private. -/
def described (n : Name) : Bool := !hidden n

/-- The description of the declaration `n`, whose constant is `ci`. -/
def describe (n : Name) (ci : ConstantInfo) : CoreM Json := do
  let env ← getEnv
  let axioms ← collectAxioms n
  -- Declared as `axiom` here or in a snapshot. Lean's own, and the ones it
  -- makes, such as `native_decide`'s (internal names), aren't assumptions.
  let assumes := axioms.filter fun a => tracked env a && !hidden a &&
    (match env.find? a with | some (.axiomInfo _) => true | _ => false)
  let ofExcept {α} (r : Except String α) : CoreM α := match r with
    | .ok a => pure a
    | .error e => throwError "describing {n}: {e}"
  let (type, typeRefs) ← ofExcept (text env n ci.type)
  let (value, valueRefs) : Json × Array Name ← match ci with
    | .defnInfo d => do
      let (v, refs) ← ofExcept (text env n d.value)
      pure (Json.str v, refs)
    | .inductInfo i => pure (Json.null, (i.ctors ++ [mkRecName i.name]).toArray)
    | _ => pure (Json.null, #[])
  return Json.mkObj [
    ("name", toJson n), ("kind", toJson (kindOf ci)),
    ("axioms", toJson axioms), ("assumes", toJson assumes),
    ("type", Json.str type), ("value", value),
    ("type_refs", toJson typeRefs), ("value_refs", toJson valueRefs),
    ("generated", toJson (generated env n))]

/-- The descriptions of the declarations `names` that are described. -/
def describeAll (names : Array Name) : CoreM (Array Json) := do
  let env ← getEnv
  let mut out := #[]
  for n in names do
    if !described n then continue
    if let some ci := env.find? n then
      out := out.push (← describe n ci)
  return out

/-- Logs `PISA_DECLS <descriptions>`, which the worker reads. -/
def log (names : Array Name) : Elab.Command.CommandElabM Unit := do
  let decls ← Elab.Command.liftCoreM (describeAll names)
  logInfo (MessageData.ofFormat (.text ("PISA_DECLS " ++ (Json.arr decls).compress)))

/-- The report on the declarations added after the imports. -/
def reportNew : Elab.Command.CommandElabM Unit := do
  log ((← getEnv).constants.map₂.toList.map (·.1)).toArray

/-- The report on the declarations of `m`, an imported module. -/
def reportModule (m : Name) : Elab.Command.CommandElabM Unit := do
  let env ← getEnv
  let some idx := env.getModuleIdx? m | throwError "{m} is not imported"
  log env.header.moduleData[idx.toNat]!.constNames

end Pisa
