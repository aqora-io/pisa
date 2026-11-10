module

public import Lean.Elab.Command
public import Lean.Meta.Sorry
public import Lean.Util.CollectAxioms
public import Pisa.Canon

/-!
How the worker describes a declaration, for its axiom report (`run_cmd
_root_.Pisa.reportNew` after code, and `pisa build` for a snapshot module) and
for the kernel verifier (`pisa verify`), which call the same `describe`.

A description is `{name, kind, axioms, assumes, type, value, type_refs,
value_refs, generated, parent}`:
- `axioms`, all the axioms it depends on, and `assumes`, those declared as
  `axiom` in this code or a snapshot;
- `type`, and for a definition `value`, their canonical text (see
  `Pisa.Canon`), which the worker hashes;
- `type_refs` and `value_refs`, the constants of this code and of snapshots
  they use (an inductive's "value" is its constructors and recursor);
- `generated`, for what Lean makes on demand, such as equation lemmas, which a
  replacement needn't have;
- `parent`, the inductive type of the same code that it is one of Lean's
  declarations for, such as a constructor or a projection (see `parentOf`).

Auxiliaries Lean makes, such as the `f._unary` holding the body of a
well-founded definition or the matcher `f.match_1`, aren't described: what
uses one reads its value in its place (see `auxiliary`), and its refs go
through to what that uses. The private names
of the module described are written without the module, which differs
between a module and its replacement; another module's are written whole,
so that one can't stand in for the other.
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

/-- Whether `n`, its private prefix aside, is a name Lean makes: one of its
components starts with `_`. -/
def leanMade (n : Name) : Bool := (privateToUserName n).isInternal

/-- The name of an auxiliary Lean makes for a declaration: an internal one
(`f._unary`, `f._proof_1`, a private one's too), a matcher (`f.match_1`, and
`t.match_1_1` in a theorem's body) or what Lean makes from one (its equations
`f.match_1.eq_1`, `f.match_1.splitter`), or `grind`'s (`f.grind_1`). By name
alone, so that the verifier, which has no more than the constants, finds the
same. -/
def auxName (n : Name) : Bool :=
  let numbered (s : String) := !s.isEmpty && (s.splitOn "_").all fun k => !k.isEmpty && k.isNat
  let matcher : Name → Bool
    | .str _ s => "match_".isPrefixOf s && numbered (s.drop 6).toString
    | _ => false
  let user := privateToUserName n
  leanMade n || user.components.any matcher || match user with
    | .str _ s => "grind_".isPrefixOf s && (s.drop 6).toString.isNat
    | _ => false

/-- Whether `type` is that of syntax Lean makes a definition for: the parser
of a `notation`, `infix`, `syntax` or `macro` (`termD`, `«term_+_»`), or a
syntax category. By type, as code may take such a definition's name. -/
def syntaxType (type : Expr) : Bool :=
  type.isConstOf ``Lean.ParserDescr || type.isConstOf ``Lean.TrailingParserDescr ||
    type.isConstOf ``Lean.Parser.Category

/-- An auxiliary: a definition or theorem with an auxiliary's name, or a
definition of syntax. It isn't described; what uses one reads, in its place,
its value, or for a theorem the proposition it proves (see `canon`). Others
with such a name, such as `native_decide`'s axiom, are described. -/
def auxiliary (n : Name) : Option ConstantInfo → Bool
  | some (.defnInfo d) => auxName n || syntaxType d.type
  | some (.thmInfo _) => auxName n
  | _ => false

/-- The module that declared the private name `n`. -/
def privateModule? (n : Name) : Option Name :=
  (privatePrefix? n).map fun p => p.getPrefix.replacePrefix privateHeader .anonymous

/-- A constant's name in a description of module `mod`. A private name of
`mod` is written `own` and the name as written, which a replacement shares;
any other name is written whole. -/
def naming (mod : Name) (n : Name) : String :=
  match privateToUserName? n with
  | some u => if privateModule? n == some mod then "own " ++ nameText u else nameText n
  | none => nameText n

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
- a definition's equations (`f.eq_1`, `f.eq_def`, `f.eq_unfold`), private
  in module-system code when its body isn't exposed, and case split
  (`f.fun_cases`), and a recursive one's induction principles (`f.induct`,
  `f.mutual_induct`; Lean lets code declare these for one that doesn't
  recurse);
- a constant's congruence lemmas (`f.congr_simp`, `f.hcongr_1`), and
  `ctorIdx.hinj`;
- a private internal name. -/
def generated (env : Environment) (n : Name) : Bool :=
  ((privateToUserName? n).map (·.isInternal)).getD false ||
  match n with
  | .str p s =>
    let defn := env.find? p
    let numbered (pre : String) := pre.isPrefixOf s && (s.drop pre.length).toString.isNat
    (["eq_def", "eq_unfold"].contains s || numbered "eq_")
      && (defn <|> env.find? (privateToUserName p)) matches some (.defnInfo _) ||
    ["fun_cases", "fun_cases_unfolding"].contains s && defn matches some (.defnInfo _) ||
    ["induct", "induct_unfolding", "mutual_induct", "mutual_induct_unfolding"].contains s
      && (match defn with | some (.defnInfo d) => recurses d | _ => false) ||
    (s == "congr_simp" || numbered "hcongr_") && defn.isSome ||
    s == "hinj" && p matches .str _ "ctorIdx" && defn.isSome
  | _ => false

/-- The marker for a `sorry`: `sorry α s` (the anonymous constant, which no
declaration can be). -/
def sorryMarker : Expr := .const .anonymous []

/-- The marker for a proof: `proof T` stands for any proof of `T`, as the
kernel takes any two proofs of a proposition to be equal. The constant `_.0`,
which no declaration can be. -/
def proofMarker : Expr := .const (.num .anonymous 0) []

/-- Whether `type`, its binders aside, is a proposition: its head is a
constant whose own type ends in `Prop`, such as `Eq`, `LE.le` or `Not`, so
that a term of this type is a proof. Read from the expression as it is,
without unfolding anything; it is fully applied, as a type must be. -/
def isProp (env : Environment) (type : Expr) : Bool := Id.run do
  let .const i _ := type.getForallBody.getAppFn | return false
  let some ci := env.find? i | return false
  return ci.type.getForallBody matches .sort .zero

/-- Whether an application of `c` is a proof: `c` is a theorem, or its type
is a proposition (`Eq.refl`, `rfl`, `And.intro`). -/
def proofHead (env : Environment) (c : Name) : Bool :=
  match env.find? c with
  | some (.thmInfo _) => true
  | some ci => isProp env ci.type
  | none => false

/-- `e` with every β- and ζ-redex reduced. -/
partial def betaZeta (e : Expr) : Expr :=
  e.replace fun
    | .letE _ _ v b _ => some (betaZeta (b.instantiate1 v))
    | x@(.app ..) => if x.getAppFn.isLambda then some (betaZeta x.headBeta) else none
    | _ => none

/-- What a proof of `type` applied to `args` proves, as far as `type` takes
arguments without unfolding anything, β- and ζ-reduced, and the arguments left
over. -/
def proves (type : Expr) (args : Array Expr) : Expr × Array Expr := Id.run do
  let mut type := type
  let mut i := 0
  while h : i < args.size do
    let .forallE _ _ body _ := type | break
    type := body.instantiate1 args[i]
    i := i + 1
  return (betaZeta type, args.extract i args.size)

/-- The canonical form of `e`:
- each proof, an application of a theorem or of another constant whose type is
  a proposition, as `proof T`, `T` the proposition it proves, so that a lemma
  and Lean's auxiliary `_proof_1` for the same proposition read the same;
- each `sorry` as one marker, `sorry α s` for `sorryAx α s`, or `proof α` if
  `α` is a proposition, as `sorry` there is a proof like any other; a `sorry`
  that Lean labels with where it was written, `sorryAx (Unit → α) s label`
  (or `Name → α`), reads as one of `α`, so the same `sorry` reads the same in
  another module or place, and the universe level is the type's;
- each auxiliary definition in `forms` as its form there, at the levels it is
  used at.
Any further arguments stay, read the same way. -/
partial def canon (env : Environment) (forms : Std.HashMap Name Expr) (e : Expr) : Expr :=
  e.replace fun x => do
    let .const c ls := x.getAppFn | none
    let args := x.getAppArgs
    let proof (type : Expr) (args : Array Expr) :=
      let (type, rest) := proves type args
      mkAppN (mkApp proofMarker (canon env forms type)) (rest.map (canon env forms))
    if c == ``sorryAx && args.size ≥ 2 then
      let (type, rest) :=
        match (Meta.isLabeledSorry? x).isSome, args[0]! with
        | true, .forallE _ _ body _ => (body.lowerLooseBVars 1 1, args.extract 3 args.size)
        | _, type => (type, args.extract 2 args.size)
      if ls.head!.normalize == .zero || isProp env type then
        return proof type rest
      return mkAppN (mkApp2 sorryMarker (canon env forms type) args[1]!)
        (rest.map (canon env forms))
    if proofHead env c then
      let ci ← env.find? c
      return proof (ci.instantiateTypeLevelParams ls) args
    let form ← forms[c]?
    let ci ← env.find? c
    return mkAppN (form.instantiateLevelParams ci.levelParams ls) (args.map (canon env forms))

/-- The tracked auxiliary definitions that the canonical form of `e` reads:
those it uses, and those that what it reads in their place uses (their
values, and the types of the proofs in them). -/
def auxiliariesReached (env : Environment) (e : Expr) : Array Name := Id.run do
  let mut seen : NameSet := {}
  let mut out := #[]
  let mut todo := e.getUsedConstants.toList
  repeat
    match todo with
    | [] => break
    | m :: rest =>
      todo := rest
      if seen.contains m then continue
      seen := seen.insert m
      let some ci := env.find? m | continue
      if tracked env m && auxiliary m ci && ci matches .defnInfo _ then
        out := out.push m
        todo := ci.value!.getUsedConstants.toList ++ todo
      if proofHead env m then
        todo := ci.type.getUsedConstants.toList ++ todo
  return out

/-- The canonical form of auxiliary definition `m`, into `forms`, after those
of the auxiliary definitions its value uses. Lean's constants don't refer to
themselves, but one that did would be left as it is, `visiting` holding those
on the way. -/
partial def auxiliaryForm (env : Environment) (visiting : NameSet) (m : Name) :
    StateM (Std.HashMap Name Expr) Unit := do
  if (← get).contains m || visiting.contains m then return
  let some (.defnInfo d) := env.find? m | return
  for a in auxiliariesReached env d.value do
    if a != m then auxiliaryForm env (visiting.insert m) a
  let forms ← get
  modify (·.insert m (canon env forms d.value))

/-- The canonical form of `e` (see `canon`), its auxiliary definitions read
as their values. -/
def canonical (env : Environment) (e : Expr) : Expr :=
  let forms := (auxiliariesReached env e).foldl (init := {}) fun forms m =>
    ((auxiliaryForm env {} m).run forms).2
  canon env forms e

/-- The text of `e`, part of declaration `self` of module `mod`, whose
universe parameters are `params`, and the constants of this code and of
snapshots its canonical form uses, `self` aside. -/
def text (env : Environment) (mod self : Name) (params : List Name) (e : Expr) :
    Except String (String × Array Name) := do
  let c := canonical env e
  let markers := [sorryMarker.constName!, proofMarker.constName!]
  let refs := c.getUsedConstants.filter fun m => m != self && tracked env m && !markers.contains m
  let ((), t) ← (discard <| write (naming mod) params c : TextM Unit).run {}
  return (t.render, refs.qsort Name.lt)

def kindOf : ConstantInfo → String
  | .thmInfo _ => "theorem"
  | .defnInfo _ => "def"
  | .axiomInfo _ => "axiom"
  | .opaqueInfo _ => "opaque"
  | .quotInfo _ => "quot"
  | .inductInfo _ => "inductive"
  | .ctorInfo _ => "constructor"
  | .recInfo _ => "recursor"


/-- The names Lean gives what it makes from an inductive type, after the
type's name, and from a constructor, after the constructor's. -/
def typeDerived : List String :=
  ["casesOn", "recOn", "below", "brecOn", "binductionOn", "ibelow", "noConfusion",
    "noConfusionType", "ctorIdx", "toCtorIdx", "ctorElim", "ctorElimType"]
def constructorDerived : List String :=
  ["injEq", "inj", "sizeOf_spec", "hinj", "noConfusion", "elim"]

/-- The inductive type of `own`, the declarations described with `n`, that
`n` is one of Lean's declarations for, by name and by shape, as the verifier
finds the same, and so that what code declares isn't taken for it:
- a constructor or the recursor;
- named after the type (`casesOn`, `noConfusion`, `below`, …), a definition
  built from the type's recursor, `casesOn`, `noConfusionType` or `ctorIdx`,
  which Lean 4.36 builds an enumeration's `noConfusion` from; its `ctorIdx`;
  and its `ctorElimType`, which Lean builds from the constructors alone;
- named after a constructor (`injEq`, `inj`, `sizeOf_spec`, …), a lemma
  about the constructor;
- a projection: a definition `fun … self => self.i` named after the
  structure's `i`th field. -/
def parentOf (env : Environment) (own : NameSet) (n : Name) (ci : ConstantInfo) :
    Option Name := do
  let inductive_ (i : Name) : Option InductiveVal := do
    guard (own.contains i)
    let some (.inductInfo v) := env.find? i | none
    return v
  let projection : Option Name := do
    let .defnInfo d := ci | none
    let .str _ field := n | none
    let mut body := d.value
    while body.isLambda do
      body := body.bindingBody!
    let .proj s i (.bvar 0) := body | none
    let v ← inductive_ s
    let [c] := v.ctors | none
    let some (.ctorInfo cv) := env.find? c | none
    let mut type := cv.type
    for _ in [0:cv.numParams + i] do
      let .forallE _ _ b _ := type | none
      type := b
    let .forallE name _ _ _ := type | none
    guard (name == .mkSimple field)
    return s
  match ci with
  | .ctorInfo c => return (← inductive_ c.induct).name
  | .recInfo _ => return (← inductive_ n.getPrefix).name
  | _ =>
    let .str p s := n | projection
    let afterType : Option Name := do
      guard (typeDerived.contains s)
      let v ← inductive_ p
      let .defnInfo d := ci | none
      let used := d.value.getUsedConstants
      -- Lean makes `ctorIdx` for every inductive type, so code can't name
      -- one so; for a single constructor it is a constant.
      guard (s == "ctorIdx" ||
        [mkRecName p, p ++ `casesOn, p ++ `noConfusionType, p ++ `ctorIdx].any used.contains ||
        (s == "ctorElimType" && v.ctors.any used.contains))
      return v.name
    let afterConstructor : Option Name := do
      guard (constructorDerived.contains s && own.contains p)
      let some (.ctorInfo c) := env.find? p | none
      guard (ci.type.getUsedConstants.contains p)
      return (← inductive_ c.induct).name
    afterType <|> afterConstructor <|> projection

/-- The description of the declaration `n` of module `mod`, whose constant is
`ci`, among `own`, the declarations described with it. -/
def describe (mod n : Name) (ci : ConstantInfo) (own : NameSet := {}) : CoreM Json := do
  let env ← getEnv
  let axioms ← collectAxioms n
  -- Declared as `axiom` here or in a snapshot. Lean's own, and the ones it
  -- makes, such as `native_decide`'s (internal names), aren't assumptions.
  let assumes := axioms.filter fun a => tracked env a && !leanMade a &&
    (match env.find? a with | some (.axiomInfo _) => true | _ => false)
  let ofExcept {α} (r : Except String α) : CoreM α := match r with
    | .ok a => pure a
    | .error e => throwError "describing {n}: {e}"
  let (type, typeRefs) ← ofExcept (text env mod n ci.levelParams ci.type)
  let (value, valueRefs) : Json × Array Name ← match ci with
    | .defnInfo d => do
      let (v, refs) ← ofExcept (text env mod n ci.levelParams d.value)
      pure (Json.str v, refs)
    | .inductInfo i => pure (Json.null, (i.ctors ++ [mkRecName i.name]).toArray)
    | _ => pure (Json.null, #[])
  return Json.mkObj [
    ("name", toJson n), ("kind", toJson (kindOf ci)),
    ("axioms", toJson axioms), ("assumes", toJson assumes),
    ("type", Json.str type), ("value", value),
    ("type_refs", toJson typeRefs), ("value_refs", toJson valueRefs),
    ("generated", toJson (generated env n)),
    ("parent", toJson (parentOf env own n ci))]

/-- The descriptions of the declarations `names` of module `mod` that are
described. -/
def describeAll (mod : Name) (names : Array Name) : CoreM (Array Json) := do
  let env ← getEnv
  let own := names.foldl (·.insert ·) ({} : NameSet)
  let mut out := #[]
  for n in names do
    let some ci := env.find? n | continue
    if auxiliary n ci then continue
    out := out.push (← describe mod n ci own)
  return out

/-- Logs `PISA_DECLS <descriptions>`, which the worker reads. -/
def log (mod : Name) (names : Array Name) : Elab.Command.CommandElabM Unit := do
  let decls ← Elab.Command.liftCoreM (describeAll mod names)
  logInfo (MessageData.ofFormat (.text ("PISA_DECLS " ++ (Json.arr decls).compress)))

/-- The report on the declarations added after the imports. -/
def reportNew : Elab.Command.CommandElabM Unit := do
  let env ← getEnv
  log env.mainModule (env.constants.map₂.toList.map (·.1)).toArray

end Pisa
