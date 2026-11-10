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
}

/// A new declaration and the axioms it depends on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Declaration {
    pub name: String,
    pub kind: String,
    pub axioms: Vec<String>,
    /// Its axioms that are assumptions: declared as `axiom` in this code or
    /// in a snapshot it builds on.
    pub assumes: Vec<String>,
    /// What its axioms make it, by [`ProofStatus`].
    pub status: ProofStatus,
    /// The SHA-256 of its type's canonical text, with private names taken
    /// without their module, and with the internal auxiliaries it uses
    /// following it.
    pub type_hash: String,
    /// For definitions, the same for the value, so a proof can't change what
    /// a statement means by redefining something it mentions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_hash: Option<String>,
    /// The constants of this code and of the snapshots it builds on that its
    /// type uses, through internal auxiliaries.
    pub type_refs: Vec<String>,
    /// The same for a definition's value, or an inductive's constructors and
    /// recursor.
    pub value_refs: Vec<String>,
    /// Made by Lean on demand, such as an equation lemma: code with the same
    /// statements needn't have it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
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
            name: d.name,
            kind: d.kind,
            axioms: d.axioms,
            assumes: d.assumes,
            type_refs: d.type_refs,
            value_refs: d.value_refs,
            generated: d.generated,
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
        }
    }
}

/// What a declaration's axioms make it, besides the allowed ones (by default
/// `propext`, `Classical.choice` and `Quot.sound`), worst last.
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

/// How a declaration of the original code changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Missing,
    /// What it is, e.g. an `axiom` where it was a `theorem`.
    Kind,
    Type,
    /// A definition's value; its type is the same.
    Value,
}

impl Change {
    /// How a reason says it, e.g. `has a different type`.
    pub fn says(self) -> &'static str {
        match self {
            Change::Missing => "is missing",
            Change::Kind => "is a different kind of declaration",
            Change::Type => "has a different type",
            Change::Value => "has a different value",
        }
    }
}

/// A declaration of the original code that the new code doesn't keep as it
/// was. Displays as a reason, e.g. `statement: t has a different type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementChange {
    pub name: String,
    pub change: Change,
}

impl std::fmt::Display for StatementChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "statement: {} {}", self.name, self.change.says())
    }
}

/// Every declaration of the original code must still exist as the same kind
/// of declaration, so a theorem can't become an axiom, with the same type,
/// and definitions with the same value. New declarations may be added, and
/// what Lean generated needn't stay. Names are matched across modules (see
/// [`normalised`]).
pub fn statement_changes(original: &[Declaration], now: &[Declaration]) -> Vec<StatementChange> {
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
            Some(StatementChange {
                name: d.name.clone(),
                change,
            })
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

    fn reasons(changes: Vec<StatementChange>) -> Vec<String> {
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
}
