//! Outcome of the verification policy that workers apply to checked code.
//! [`DescribedDeclaration`]s are parsed from the worker's Lean report (the
//! `Pisa` library's `describe`), so their field names must match the JSON it
//! prints, and a [`Declaration`] keeps the same names.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Verification {
    /// True when the code elaborated without errors and passed every policy check.
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declarations: Vec<Declaration>,
    /// What keeps the code from replacing what it replaces, or from keeping
    /// a session's statements, each also one of `reasons`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<DeclarationChange>,
}

/// A new declaration and the axioms it depends on. Unless a request asks for
/// declarations in full, a response summarises each, leaving out `assumes`,
/// `type_hash`, `type_refs` and `value_refs`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
pub struct Declaration {
    pub name: String,
    pub kind: String,
    pub axioms: Vec<String>,
    /// Its axioms that are assumptions: declared as `axiom` in this code or
    /// in a snapshot it builds on.
    #[serde(default)]
    pub assumes: Vec<String>,
    /// What its axioms make it, by [`ProofStatus`].
    pub status: ProofStatus,
    /// The SHA-256 of its type's canonical text, with private names taken
    /// without their module, and with the internal auxiliaries it uses
    /// following it.
    #[serde(default)]
    pub type_hash: String,
    /// For definitions, the same for the value, so a proof can't change what
    /// a statement means by redefining something it mentions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_hash: Option<String>,
    /// The constants of this code and of the snapshots it builds on that its
    /// type uses, through internal auxiliaries.
    #[serde(default)]
    pub type_refs: Vec<String>,
    /// The same for a definition's value, or an inductive's constructors and
    /// recursor.
    #[serde(default)]
    pub value_refs: Vec<String>,
    /// Made by Lean on demand, such as an equation lemma: code with the same
    /// statements needn't have it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
    /// The inductive type it is made from, when it is one of the
    /// declarations Lean makes for one: a constructor, the recursor,
    /// `casesOn`, `noConfusion`, `injEq`, `sizeOf_spec`, a projection and
    /// the like. A change to it is a change to that type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// For a theorem of `¬ X` (or `X → False`), the SHA-256 of `X`'s
    /// canonical text with every name written whole: the `type_hash` of a
    /// declaration that states `X` and mentions no private name, from any
    /// module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negates: Option<String>,
    /// The axioms it gets other than through `inherits`: from its own proof,
    /// the definitions it uses, Lean and Mathlib. Absent in records made
    /// before it was recorded, and in a check's report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_axioms: Option<Vec<String>>,
    /// The theorems of other snapshot modules through which it gets axioms
    /// beyond Lean's standard three. A theorem's proof can be swapped for
    /// another proof of the same statement, so an audit reads an inherited
    /// `sorry` through the run that proves the theorem.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherits: Vec<Inherited>,
    /// What its own axioms make it, as `status` is from all of them: a
    /// finished proof on a sorried theorem of another snapshot is `sorry`
    /// and its own status `proved`. Present when it `inherits` something;
    /// otherwise its status is its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_status: Option<ProofStatus>,
}

/// A theorem of another snapshot module that a declaration gets axioms
/// through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Inherited {
    pub name: String,
    /// Its module, `PisaSnap.s…`.
    pub module: String,
}

impl Serialize for Declaration {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        // Only a summary has no type hash. In full, every field is written,
        // empty or not, as stored records were before summaries: workers of
        // earlier releases read them.
        let full = !self.type_hash.is_empty();
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("name", &self.name)?;
        m.serialize_entry("kind", &self.kind)?;
        m.serialize_entry("axioms", &self.axioms)?;
        if full {
            m.serialize_entry("assumes", &self.assumes)?;
        }
        m.serialize_entry("status", &self.status)?;
        if full {
            m.serialize_entry("type_hash", &self.type_hash)?;
        }
        if let Some(value_hash) = &self.value_hash {
            m.serialize_entry("value_hash", value_hash)?;
        }
        if full {
            m.serialize_entry("type_refs", &self.type_refs)?;
            m.serialize_entry("value_refs", &self.value_refs)?;
        }
        if self.generated {
            m.serialize_entry("generated", &true)?;
        }
        if let Some(parent) = &self.parent {
            m.serialize_entry("parent", parent)?;
        }
        if full && let Some(negates) = &self.negates {
            m.serialize_entry("negates", negates)?;
        }
        if full && let Some(own_axioms) = &self.own_axioms {
            m.serialize_entry("own_axioms", own_axioms)?;
        }
        if !self.inherits.is_empty() {
            m.serialize_entry("inherits", &self.inherits)?;
        }
        if let Some(own_status) = &self.own_status {
            m.serialize_entry("own_status", own_status)?;
        }
        m.end()
    }
}

impl Declaration {
    /// The declaration as a response gives it unless asked for in full: its
    /// name, kind and status, the axioms it uses outside `allowed`, and its
    /// own status and what it inherits through.
    pub fn summarised(self, allowed: &[String]) -> Declaration {
        Declaration {
            name: self.name,
            kind: self.kind,
            axioms: self
                .axioms
                .into_iter()
                .filter(|a| !allowed.contains(a))
                .collect(),
            assumes: Vec::new(),
            status: self.status,
            type_hash: String::new(),
            value_hash: None,
            type_refs: Vec::new(),
            value_refs: Vec::new(),
            generated: false,
            parent: None,
            negates: None,
            own_axioms: None,
            inherits: self.inherits,
            own_status: self.own_status,
        }
    }
}

/// A declaration as Lean's report prints it (`Pisa.describe`): a
/// [`ReportedDeclaration`] with the canonical text of its type and, for a
/// definition, of its value in place of their hashes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DescribedDeclaration {
    pub name: String,
    pub kind: String,
    pub axioms: Vec<String>,
    pub assumes: Vec<String>,
    #[serde(rename = "type")]
    pub type_text: String,
    #[serde(rename = "value")]
    pub value_text: Option<String>,
    pub type_refs: Vec<String>,
    pub value_refs: Vec<String>,
    #[serde(default)]
    pub generated: bool,
    #[serde(default)]
    pub parent: Option<String>,
    /// For a theorem of `¬ X`, the canonical text of `X`, every name whole.
    #[serde(default)]
    pub negates: Option<String>,
    #[serde(default)]
    pub own_axioms: Option<Vec<String>>,
    #[serde(default)]
    pub inherits: Vec<Inherited>,
}

/// The SHA-256 of `text`, as hex: what a declaration's hashes are.
fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

impl From<DescribedDeclaration> for ReportedDeclaration {
    fn from(d: DescribedDeclaration) -> Self {
        ReportedDeclaration {
            type_hash: digest(&d.type_text),
            value_hash: d.value_text.as_deref().map(digest),
            negates: d.negates.as_deref().map(digest),
            own_axioms: d.own_axioms,
            inherits: d.inherits,
            name: d.name,
            kind: d.kind,
            axioms: d.axioms,
            assumes: d.assumes,
            type_refs: d.type_refs,
            value_refs: d.value_refs,
            generated: d.generated,
            parent: d.parent,
        }
    }
}

/// A [`Declaration`] as Lean's report gives it, its texts hashed (see
/// [`DescribedDeclaration`]): the same fields, without `status`, which the
/// worker's policy works out from the axioms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportedDeclaration {
    pub name: String,
    pub kind: String,
    pub axioms: Vec<String>,
    pub assumes: Vec<String>,
    pub type_hash: String,
    pub value_hash: Option<String>,
    pub type_refs: Vec<String>,
    pub value_refs: Vec<String>,
    pub generated: bool,
    pub parent: Option<String>,
    pub negates: Option<String>,
    pub own_axioms: Option<Vec<String>>,
    pub inherits: Vec<Inherited>,
}

impl ReportedDeclaration {
    /// The declaration, with what its axioms make it.
    pub fn with_status(self, status: ProofStatus) -> Declaration {
        Declaration {
            name: self.name,
            kind: self.kind,
            axioms: self.axioms,
            assumes: self.assumes,
            status,
            type_hash: self.type_hash,
            value_hash: self.value_hash,
            type_refs: self.type_refs,
            value_refs: self.value_refs,
            generated: self.generated,
            parent: self.parent,
            negates: self.negates,
            own_axioms: self.own_axioms,
            inherits: self.inherits,
            own_status: None,
        }
    }
}

/// The axioms every proof may use by default (`PISA_ALLOWED_AXIOMS`).
pub const DEFAULT_AXIOMS: &str = "propext,Classical.choice,Quot.sound";

/// What a declaration's axioms make it, besides the allowed ones (by default
/// [`DEFAULT_AXIOMS`]), worst last.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProofStatus {
    /// None.
    Proved,
    /// Assumptions only: axioms declared in this code or in a snapshot.
    Conditional,
    /// `sorryAx`, with or without assumptions.
    Sorry,
    /// Any other, such as the one `native_decide` declares, or
    /// `Lean.trustCompiler`.
    Rejected,
}

impl ProofStatus {
    /// What `axioms` make a declaration whose assumptions are `assumes`:
    /// besides the `allowed` ones, none is `proved`; only assumptions
    /// `conditional`; `sorryAx`, with or without assumptions, `sorry`;
    /// anything else `rejected`.
    pub fn of(axioms: &[String], assumes: &[String], allowed: &[String]) -> ProofStatus {
        let each = axioms.iter().filter(|a| !allowed.contains(a)).map(|a| {
            if assumes.contains(a) {
                ProofStatus::Conditional
            } else if a == "sorryAx" {
                ProofStatus::Sorry
            } else {
                ProofStatus::Rejected
            }
        });
        each.max().unwrap_or(ProofStatus::Proved)
    }
}

/// `name` as it is across modules: a private name without the module it was
/// declared in, which differs between a module and its replacement, e.g.
/// `_private.PisaSnap.s1a2b.0.helper` as `_private.helper`.
pub fn normalised(name: &str) -> Cow<'_, str> {
    let Some(rest) = name.strip_prefix("_private.") else {
        return Cow::Borrowed(name);
    };
    // The module's components, then a number, then the name as written.
    let mut parts = rest.split('.');
    let mut user = None;
    let mut offset = 0;
    for part in parts.by_ref() {
        offset += part.len() + 1;
        if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
            user = rest.get(offset..);
            break;
        }
    }
    match user {
        Some(user) if !user.is_empty() => Cow::Owned(format!("_private.{user}")),
        _ => Cow::Borrowed(name),
    }
}

/// How a declaration of the original code changed, or what else keeps new
/// code from replacing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Missing,
    /// What it is, e.g. an `axiom` where it was a `theorem`.
    Kind,
    Type,
    /// A definition's value; its type is the same.
    Value,
    /// An inductive type's constructors, or what Lean makes from them (see
    /// [`Declaration::parent`]), listed in
    /// [`includes`](DeclarationChange::includes); its own type is the same.
    Derived,
    /// An assumption the new code declares, or a snapshot it newly builds on
    /// does, that the original didn't.
    AddedAssumption,
    /// An assumption the original declared, with another statement.
    Assumption,
    /// A constant of a snapshot the new code no longer builds on, which more
    /// than one declaration of the code and what it builds on matches.
    Ambiguous,
}

impl Change {
    /// How a reason says it of a declaration, e.g. `has a different type`.
    pub fn says(self) -> &'static str {
        match self {
            Change::Missing => "is missing",
            Change::Kind => "is a different kind of declaration",
            Change::Type => "has a different type",
            Change::Value => "has a different value",
            Change::Derived => "has different constructors or what Lean makes from them",
            Change::AddedAssumption => "is an assumption the code adds",
            Change::Assumption => "is an assumption with a different statement",
            Change::Ambiguous => {
                "matches more than one declaration of the code and what it builds on"
            }
        }
    }
}

/// A declaration of the original code that new code doesn't keep as it
/// was, or an assumption it adds. Displays as a reason, e.g.
/// `statement: t has a different type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeclarationChange {
    /// The declaration, as the original code, or the snapshot `module`,
    /// names it; an added assumption as the new code does.
    pub name: String,
    /// The snapshot module it is from, when that is a snapshot the new code
    /// no longer builds on, rather than the original code itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// For a missing declaration, the snapshot module the new code builds on
    /// that declares an identical one, with the same kind, type and value.
    /// An import can't stand in for the spec's own declarations: keep them
    /// in the module and import only helpers (README, *Reusing a proof*).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<String>,
    pub change: Change,
    /// For a changed type or assumption, its signature before, and `now`
    /// after, from the search exports of the modules, where they have them.
    /// The exports are what the builds printed; the comparison is of the
    /// declarations' hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
    /// When `was` and `now` print alike, as when an instance is found by
    /// another path, the type before around where it first differs, printed
    /// with every argument shown, and `now_explicit` the type after. Only
    /// where both builds' exports have that text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was_explicit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now_explicit: Option<String>,
    /// For a change to what Lean makes from an inductive type, the
    /// declarations that changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
}

impl DeclarationChange {
    pub fn new(name: impl Into<String>, change: Change) -> Self {
        DeclarationChange {
            name: name.into(),
            module: None,
            imported_from: None,
            change,
            was: None,
            now: None,
            was_explicit: None,
            now_explicit: None,
            includes: Vec::new(),
        }
    }
}

/// Characters of context [`explicit_difference`] keeps before the first
/// difference, and after the last.
const DIFFERENCE_CONTEXT: usize = 40;

/// Where texts `a` and `b` differ, if they do: each cut to its differing
/// part with [`DIFFERENCE_CONTEXT`] characters around it, at most four times
/// that in all, and `…` where it was cut.
pub fn explicit_difference(a: &str, b: &str) -> Option<(String, String)> {
    if a == b {
        return None;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let shorter = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(shorter)
        .take_while(|(x, y)| x == y)
        .count();
    let window = |text: &[char]| {
        let start = prefix.saturating_sub(DIFFERENCE_CONTEXT);
        let end = (text.len() - suffix + DIFFERENCE_CONTEXT)
            .min(text.len())
            .min(start + 4 * DIFFERENCE_CONTEXT);
        let mut out = String::new();
        if start > 0 {
            out.push('…');
        }
        out.extend(&text[start..end]);
        if end < text.len() {
            out.push('…');
        }
        out
    };
    Some((window(&a), window(&b)))
}

impl std::fmt::Display for DeclarationChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = &self.name;
        match (self.change, &self.module) {
            (Change::AddedAssumption, _) => {
                write!(f, "statement: the code adds the assumption {name}")
            }
            (Change::Assumption, _) => {
                write!(
                    f,
                    "statement: the assumption {name} has a different statement"
                )
            }
            (change, Some(module)) => {
                write!(f, "statement: {name}, from {module}, {}", change.says())
            }
            (change, None) => write!(f, "statement: {name} {}", change.says()),
        }?;
        if let Some(from) = &self.imported_from {
            write!(
                f,
                "; `{from}` declares an identical {name} \u{2014} keep the spec's declarations in this \
                 module and import only helpers (README, *Reusing a proof*)"
            )?;
        }
        if !self.includes.is_empty() {
            write!(f, " ({})", self.includes.join(", "))?;
        }
        if let (Some(was), Some(now)) = (&self.was_explicit, &self.now_explicit) {
            write!(
                f,
                "; it prints alike, but with every argument shown it was `{was}` and is now `{now}`"
            )?;
        }
        Ok(())
    }
}

/// `changes`, those of declarations Lean made from an inductive type
/// (`parent` gives it, by the change's name and module) folded into one for
/// that type, which lists them in `includes`: a [`Change::Derived`], unless
/// the type itself changed.
pub fn grouped(
    changes: Vec<DeclarationChange>,
    parent: impl Fn(&DeclarationChange) -> Option<String>,
) -> Vec<DeclarationChange> {
    let mut out: Vec<DeclarationChange> = Vec::new();
    let mut folded: Vec<(DeclarationChange, String)> = Vec::new();
    for c in changes {
        match parent(&c) {
            Some(p) => folded.push((c, p)),
            None => out.push(c),
        }
    }
    for (c, p) in folded {
        let at = out.iter().position(|o| o.name == p && o.module == c.module);
        let group = match at {
            Some(i) => &mut out[i],
            None => {
                out.push(DeclarationChange {
                    module: c.module.clone(),
                    ..DeclarationChange::new(p, Change::Derived)
                });
                out.last_mut().unwrap()
            }
        };
        group.includes.push(c.name);
    }
    out
}

/// The parent of the declaration named `name` among `decls`, if it has one
/// (see [`Declaration::parent`]).
pub fn parent_in(decls: &[Declaration], name: &str) -> Option<String> {
    decls.iter().find(|d| d.name == name)?.parent.clone()
}

/// Every declaration of the original code must still exist as the same kind
/// of declaration, so a theorem can't become an axiom, with the same type,
/// and definitions with the same value. New declarations may be added, and
/// what Lean generated needn't stay. Names are matched across modules (see
/// [`normalised`]).
pub fn statement_changes(original: &[Declaration], now: &[Declaration]) -> Vec<DeclarationChange> {
    original
        .iter()
        .filter(|d| !d.generated)
        .filter_map(|d| {
            let name = normalised(&d.name);
            let change = match now.iter().find(|n| normalised(&n.name) == name) {
                None => Change::Missing,
                Some(n) if n.kind != d.kind => Change::Kind,
                Some(n) if n.type_hash != d.type_hash => Change::Type,
                Some(n) if n.value_hash != d.value_hash => Change::Value,
                Some(_) => return None,
            };
            Some(DeclarationChange::new(d.name.clone(), change))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str, ty: &str, value: Option<&str>) -> Declaration {
        let reported = ReportedDeclaration {
            name: name.into(),
            kind: if value.is_some() { "def" } else { "theorem" }.into(),
            type_hash: ty.into(),
            value_hash: value.map(Into::into),
            ..Default::default()
        };
        reported.with_status(ProofStatus::Proved)
    }

    #[test]
    fn a_summary_keeps_the_name_kind_status_and_the_axioms_not_allowed() {
        let mut d = decl("t", "T", Some("V"));
        d.axioms = vec!["propext".into(), "sorryAx".into(), "oops".into()];
        d.assumes = vec!["oops".into()];
        d.type_refs = vec!["f".into()];
        d.value_refs = vec!["g".into()];
        d.generated = true;
        d.parent = Some("S".into());
        let summary = d.summarised(&["propext".to_string()]);
        assert_eq!(
            serde_json::to_value(&summary).unwrap(),
            serde_json::json!({"name": "t", "kind": "def", "axioms": ["sorryAx", "oops"],
                               "status": "proved"})
        );
        // It reads back, with what it left out empty.
        let back: Declaration =
            serde_json::from_value(serde_json::to_value(&summary).unwrap()).unwrap();
        assert_eq!(back, summary);
    }

    #[test]
    fn what_a_theorem_negates_is_hashed_and_written_in_full_only() {
        let described: DescribedDeclaration = serde_json::from_value(serde_json::json!({
            "name": "r", "kind": "theorem", "axioms": [], "assumes": [], "type": "¬X",
            "value": null, "type_refs": [], "value_refs": [], "negates": "X"}))
        .unwrap();
        let reported = ReportedDeclaration::from(described);
        assert_eq!(reported.negates.as_deref(), Some(digest("X").as_str()));
        let d = reported.with_status(ProofStatus::Proved);
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["negates"], serde_json::json!(digest("X")));
        let summary = serde_json::to_value(d.summarised(&[])).unwrap();
        assert!(summary.get("negates").is_none(), "{summary}");
        // Absent, it isn't written.
        let json = serde_json::to_value(decl("t", "T", None)).unwrap();
        assert!(json.get("negates").is_none(), "{json}");
    }

    #[test]
    fn a_summary_keeps_the_own_status_and_what_it_inherits() {
        let described: DescribedDeclaration = serde_json::from_value(serde_json::json!({
            "name": "t", "kind": "theorem", "axioms": ["sorryAx"], "assumes": [],
            "type": "T", "value": null, "type_refs": [], "value_refs": [],
            "own_axioms": [], "inherits": [{"name": "l", "module": "PisaSnap.sx"}]}))
        .unwrap();
        let mut d = ReportedDeclaration::from(described).with_status(ProofStatus::Sorry);
        d.own_status = Some(ProofStatus::Proved);
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["own_axioms"], serde_json::json!([]));
        let inherits = serde_json::json!([{"name": "l", "module": "PisaSnap.sx"}]);
        assert_eq!(json["inherits"], inherits);
        assert_eq!(json["own_status"], "proved");
        assert_eq!(serde_json::from_value::<Declaration>(json).unwrap(), d);
        // A summary keeps what tells its own status from what it inherits.
        let summary = serde_json::to_value(d.summarised(&[])).unwrap();
        assert!(summary.get("own_axioms").is_none(), "{summary}");
        assert_eq!(
            (&summary["own_status"], &summary["inherits"]),
            (&serde_json::json!("proved"), &inherits)
        );
        // A record from before has neither.
        let json = serde_json::to_value(decl("t", "T", None)).unwrap();
        assert!(json.get("own_axioms").is_none() && json.get("inherits").is_none());
        assert!(json.get("own_status").is_none());
    }

    #[test]
    fn a_status_is_the_worst_of_what_the_axioms_make_it() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let allowed = names(&["propext"]);
        let of = |axioms: &[&str], assumes: &[&str]| {
            ProofStatus::of(&names(axioms), &names(assumes), &allowed)
        };
        assert_eq!(of(&["propext"], &[]), ProofStatus::Proved);
        assert_eq!(of(&["h"], &["h"]), ProofStatus::Conditional);
        assert_eq!(of(&["h", "sorryAx"], &["h"]), ProofStatus::Sorry);
        assert_eq!(
            of(&["sorryAx", "Lean.ofReduceBool"], &[]),
            ProofStatus::Rejected
        );
    }

    #[test]
    fn a_full_declaration_writes_every_field_even_empty() {
        // As stored records are read by workers that came before summaries.
        let d = decl("t", "T", None);
        let json = serde_json::to_value(&d).unwrap();
        for key in ["assumes", "type_hash", "type_refs", "value_refs"] {
            assert!(json.get(key).is_some(), "{key}: {json}");
        }
    }

    #[test]
    fn a_change_reads_as_the_reason_a_refusal_gives() {
        let from = |name: &str, change| DeclarationChange {
            module: Some("PisaSnap.sa".into()),
            ..DeclarationChange::new(name, change)
        };
        for (change, reason) in [
            (
                DeclarationChange::new("t", Change::Type),
                "statement: t has a different type",
            ),
            (
                from("foo", Change::Value),
                "statement: foo, from PisaSnap.sa, has a different value",
            ),
            (
                DeclarationChange::new("helper", Change::AddedAssumption),
                "statement: the code adds the assumption helper",
            ),
            (
                DeclarationChange::new("kun", Change::Assumption),
                "statement: the assumption kun has a different statement",
            ),
            (
                from("foo", Change::Ambiguous),
                "statement: foo, from PisaSnap.sa, matches more than one declaration of the code \
                 and what it builds on",
            ),
        ] {
            assert_eq!(change.to_string(), reason);
        }
    }

    #[test]
    fn changes_lean_made_from_an_inductive_fold_into_it() {
        let changes = ["S.mk", "t", "S.rec", "T", "T.mk"]
            .map(|n| DeclarationChange::new(n, Change::Type))
            .to_vec();
        let parent = |c: &DeclarationChange| match c.name.as_str() {
            "S.mk" | "S.rec" => Some("S".to_string()),
            "T.mk" => Some("T".to_string()),
            _ => None,
        };
        assert_eq!(
            reasons(grouped(changes, parent)),
            [
                "statement: t has a different type",
                "statement: T has a different type (T.mk)",
                "statement: S has different constructors or what Lean makes from them \
                 (S.mk, S.rec)",
            ]
        );
    }

    #[test]
    fn a_description_is_reported_with_the_hashes_of_its_texts() {
        let described: DescribedDeclaration = serde_json::from_value(serde_json::json!({
            "name": "f", "kind": "def", "axioms": [], "assumes": [],
            "type": "C _.\"Nat\" []\nR 0", "value": "N 1\nR 0",
            "type_refs": [], "value_refs": ["g"], "generated": false
        }))
        .unwrap();
        let reported = ReportedDeclaration::from(described);
        let sha = |text: &str| hex::encode(Sha256::digest(text.as_bytes()));
        assert_eq!(reported.type_hash, sha("C _.\"Nat\" []\nR 0"));
        assert_eq!(reported.value_hash, Some(sha("N 1\nR 0")));
        assert_eq!(reported.value_refs, ["g"]);
        // A theorem has no value to hash.
        let theorem: DescribedDeclaration = serde_json::from_value(serde_json::json!({
            "name": "t", "kind": "theorem", "axioms": [], "assumes": [],
            "type": "S 0\nR 0", "value": null, "type_refs": [], "value_refs": []
        }))
        .unwrap();
        assert_eq!(ReportedDeclaration::from(theorem).value_hash, None);
    }

    #[test]
    fn private_names_lose_their_module() {
        assert_eq!(
            normalised("_private.PisaSnap.s1a2b.0.helper"),
            "_private.helper"
        );
        assert_eq!(
            normalised("_private.PisaSnap.s1a2b.0.ack.match_1.eq_1"),
            "_private.ack.match_1.eq_1"
        );
        assert_eq!(normalised("_private.Main.12.x"), "_private.x");
        for kept in ["helper", "_private", "_private.Mod.x", "_private.Mod.0."] {
            assert_eq!(normalised(kept), kept);
        }
    }

    #[test]
    fn private_and_generated_declarations_compare_as_written() {
        let private = |module: &str, value: &str| Declaration {
            value_hash: Some(value.into()),
            ..decl(&format!("_private.PisaSnap.{module}.0.helper"), "1", None)
        };
        let generated = Declaration {
            generated: true,
            ..decl("f.eq_def", "2", None)
        };
        let original = [private("sA", "3"), generated];
        assert!(statement_changes(&original, &[private("sB", "3")]).is_empty());
        assert_eq!(
            reasons(statement_changes(&original, &[private("sB", "4")])),
            ["statement: _private.PisaSnap.sA.0.helper has a different value"]
        );
    }

    fn reasons(changes: Vec<DeclarationChange>) -> Vec<String> {
        changes.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn statement_changes_catch_missing_retyped_and_redefined() {
        let original = [decl("t", "1", None), decl("f", "2", Some("3"))];
        assert!(statement_changes(&original, &original).is_empty());
        let mut extra = original.to_vec();
        extra.push(decl("helper", "9", None));
        assert!(statement_changes(&original, &extra).is_empty());
        assert_eq!(
            reasons(statement_changes(
                &original,
                &[decl("t", "X", None), decl("f", "2", Some("4"))]
            )),
            [
                "statement: t has a different type",
                "statement: f has a different value"
            ]
        );
        assert_eq!(
            reasons(statement_changes(&original, &[decl("f", "2", Some("3"))])),
            ["statement: t is missing"]
        );
        // A theorem assumed instead of proved: same name, same type.
        let assumed = Declaration {
            kind: "axiom".into(),
            ..decl("t", "1", None)
        };
        assert_eq!(
            reasons(statement_changes(
                &original,
                &[assumed, original[1].clone()]
            )),
            ["statement: t is a different kind of declaration"]
        );
    }

    #[test]
    fn explicit_difference_cuts_both_texts_around_where_they_differ() {
        assert_eq!(explicit_difference("2 = 2", "2 = 2"), None);
        assert_eq!(
            explicit_difference("@Eq Nat 2 2", "@Eq Nat 3 3"),
            Some(("@Eq Nat 2 2".into(), "@Eq Nat 3 3".into()))
        );
        let common = "x".repeat(100);
        let (was, now) = explicit_difference(
            &format!("{common} instA {common}"),
            &format!("{common} instBB {common}"),
        )
        .unwrap();
        // The context counts from the first differing character, after the
        // shared `inst`, and from the last one, before the shared ` `.
        let (before, after) = (
            "x".repeat(DIFFERENCE_CONTEXT - 5),
            "x".repeat(DIFFERENCE_CONTEXT - 1),
        );
        assert_eq!(was, format!("…{before} instA {after}…"));
        assert_eq!(now, format!("…{before} instBB {after}…"));
        // One text extending the other: the extra part is the difference.
        assert_eq!(
            explicit_difference("f a", "f a b"),
            Some(("f a".into(), "f a b".into()))
        );
        // A long differing part is cut too.
        let (was, now) = explicit_difference(&"a".repeat(500), &"b".repeat(500)).unwrap();
        assert_eq!(was.chars().count(), 4 * DIFFERENCE_CONTEXT + 1);
        assert!(now.starts_with('b') && now.ends_with('…'), "{now}");
    }

    #[test]
    fn a_change_that_prints_alike_says_how_it_differs() {
        let mut c = DeclarationChange::new("t", Change::Type);
        c.was = Some("t : P".into());
        c.now = Some("t : P".into());
        assert_eq!(c.to_string(), "statement: t has a different type");
        (c.was_explicit, c.now_explicit) = (Some("@P instA".into()), Some("@P instB".into()));
        assert_eq!(
            c.to_string(),
            "statement: t has a different type; it prints alike, but with every argument shown \
             it was `@P instA` and is now `@P instB`"
        );
    }
}
