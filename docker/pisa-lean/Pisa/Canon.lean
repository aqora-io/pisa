module

public import Lean.Expr

/-!
The canonical text of expressions: what the worker hashes as a declaration's
type or value. It is injective, so equal texts are equal expressions (up to
binder names), and it doesn't depend on how terms are shared in memory, so a
module reads the same whether it was just elaborated or loaded from its
`.olean`.

Each line is one distinct subterm, after the subterms it is made of, which it
names by their line numbers from 0; a line that would repeat one above is that
one. `R <n>` ends the text of one expression, naming its root.
-/

public section

namespace Pisa.Canon
open Lean

/-- A name: `_`, then each component, `."string"` or `.number`. -/
def nameText : Name → String
  | .anonymous => "_"
  | .str p s => nameText p ++ "." ++ s.quote
  | .num p k => nameText p ++ "." ++ toString k

def levelText : Level → Except String String
  | .zero => pure "0"
  | .succ l => return s!"(+1 {← levelText l})"
  | .max a b => return s!"(max {← levelText a} {← levelText b})"
  | .imax a b => return s!"(imax {← levelText a} {← levelText b})"
  | .param n => pure s!"(u {nameText n})"
  | .mvar _ => throw "a universe metavariable"

def binderText : BinderInfo → String
  | .default => "d"
  | .implicit => "i"
  | .strictImplicit => "s"
  | .instImplicit => "c"

/-- An expression as a key that tells binder kinds apart, which `Expr`'s own
`BEq` doesn't. -/
structure Key where
  expr : Expr

instance : BEq Key := ⟨fun a b => a.expr.equal b.expr⟩
instance : Hashable Key := ⟨fun k => k.expr.hash⟩

/-- The lines written so far. `constant` writes a constant's line: how the
names of private and auxiliary constants read is the report's to say. -/
structure Text where
  lines : Array String := #[]
  index : Std.HashMap String Nat := {}
  done : Std.HashMap Key Nat := {}

abbrev TextM := StateT Text (Except String)

/-- The line number of `line`, written if it is new. -/
def line (line : String) : TextM Nat := do
  let t ← get
  if let some i := t.index[line]? then return i
  let i := t.lines.size
  set { t with lines := t.lines.push line, index := t.index.insert line i }
  return i

/-- Writes `marker` as a line of its own, between expressions. -/
def mark (marker : String) : TextM Unit :=
  modify fun t => { t with lines := t.lines.push marker }

/-- The subterms a line names, in order. `mdata` is read through. -/
def parts : Expr → Array Expr
  | .app f a => #[f, a]
  | .lam _ t b _ | .forallE _ t b _ => #[t, b]
  | .letE _ t v b _ => #[t, v, b]
  | .mdata _ e | .proj _ _ e => #[e]
  | _ => #[]

/-- Writes the lines of `e`, then `R <root>`, and returns the root's number.
`constant` gives a constant's line. The walk keeps its own stack, as deep
terms (a long list literal) are deeper than Lean's. -/
def write (constant : Name → List Level → Except String String) (e : Expr) : TextM Nat := do
  let mut stack : Array (Expr × Bool) := #[(e, false)]
  while h : stack.size > 0 do
    let (x, ready) := stack[stack.size - 1]
    stack := stack.pop
    if (← get).done.contains ⟨x⟩ then continue
    if !ready then
      stack := stack.push (x, true)
      for p in (parts x).reverse do
        stack := stack.push (p, false)
      continue
    let at_ (p : Expr) : TextM Nat := do
      match (← get).done[(⟨p⟩ : Key)]? with
      | some i => pure i
      | none => throw "a subterm was written out of order"
    let i ← match x with
      | .bvar k => line s!"B {k}"
      | .sort l => do line s!"S {← levelText l}"
      | .const n ls => do line s!"C {← constant n ls}"
      | .app f a => do line s!"A {← at_ f} {← at_ a}"
      | .lam _ t b bi => do line s!"L {binderText bi} {← at_ t} {← at_ b}"
      | .forallE _ t b bi => do line s!"P {binderText bi} {← at_ t} {← at_ b}"
      | .letE _ t v b _ => do line s!"E {← at_ t} {← at_ v} {← at_ b}"
      | .lit (.natVal n) => line s!"N {n}"
      | .lit (.strVal s) => line s!"T {s.quote}"
      | .proj s k p => do line s!"J {nameText s} {k} {← at_ p}"
      | .mdata _ p => at_ p
      | .fvar _ => throw "a free variable"
      | .mvar _ => throw "a metavariable"
    modify fun t => { t with done := t.done.insert ⟨x⟩ i }
  let root ← match (← get).done[(⟨e⟩ : Key)]? with
    | some i => pure i
    | none => throw "the root was not written"
  mark s!"R {root}"
  return root

/-- The text, from the lines written. -/
def Text.render (t : Text) : String :=
  "\n".intercalate t.lines.toList

end Pisa.Canon
