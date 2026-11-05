//! Outcome of the verification policy that workers apply to checked code.
//! Declarations are parsed from the worker's Lean report, so their field
//! names must match the JSON it prints.

use serde::{Deserialize, Serialize};
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
    pub type_hash: String,
    /// For definitions, a hash of the value, so a proof can't change what a
    /// statement means by redefining something it mentions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_hash: Option<String>,
}

/// How a declaration of the original code changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Missing,
    Type,
    /// A definition's value; its type is the same.
    Value,
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
        let what = match self.change {
            Change::Missing => "is missing",
            Change::Type => "has a different type",
            Change::Value => "has a different value",
        };
        write!(f, "statement: {} {what}", self.name)
    }
}

/// Every declaration of the original code must still exist with the same
/// type, and definitions with the same value. New declarations may be added.
pub fn statement_changes(original: &[Declaration], now: &[Declaration]) -> Vec<StatementChange> {
    original
        .iter()
        .filter_map(|d| {
            let change = match now.iter().find(|n| n.name == d.name) {
                None => Change::Missing,
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
        Declaration {
            name: name.into(),
            kind: if value.is_some() { "def" } else { "theorem" }.into(),
            axioms: vec![],
            type_hash: ty.into(),
            value_hash: value.map(Into::into),
        }
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
    }
}
