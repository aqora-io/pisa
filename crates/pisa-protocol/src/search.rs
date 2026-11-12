//! The declaration search API (`pisa-search`).
//!
//! | Endpoint | Request → response |
//! | --- | --- |
//! | `POST /search` | [`SearchRequest`] → [`SearchResponse`] |
//! | `POST /resolve` | [`ResolveRequest`] → [`ResolveResponse`] |
//! | `GET /declarations/{name}` | → [`GetDeclarationResponse`] |
//!
//! Errors use [`crate::api::ErrorResponse`]. Unknown names and empty results
//! are ordinary answers, never errors. Every response names the `version` (the
//! project version) its index was built from; a request may name one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::verify::ProofStatus;

fn default_limit() -> usize {
    5
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SearchRequest {
    /// At most 64.
    pub queries: Vec<Query>,
    /// A version, or an alias of one: the index's, or its snapshots'. Any
    /// other fails with `not_found`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// One search. Fill as many fields as you can: each feeds different lanes,
/// and more fields mean better fusion.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Query {
    /// Free text from a person; classified as natural language, LaTeX, Lean or a name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// A natural-language description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The statement in LaTeX, e.g. `S(\rho_{AB}) \le S(\rho_A) + S(\rho_B)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latex: Option<String>,
    /// A Lean expression or pattern, e.g. `Matrix.trace ?A = 1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// A pretty-printed goal state (hypotheses and `⊢ target`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// A declaration name, possibly partial or misspelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Constants the statement is about, by their exact names, e.g. those of a
    /// goal's target read from a Lean term: they rank declarations that
    /// mention them, and theorems that conclude with them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consts: Vec<String>,
    /// Restrict to these libraries, e.g. `["Physlib"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<String>,
    /// Restrict to these kinds, e.g. `["theorem"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Hits to return (default 5, at most 50).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Also return declarations that depend on `sorry`, flagged. By default
    /// only when searching snapshots, so that agents find the skeleton's
    /// lemmas they build on before those are proved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_sorried: Option<bool>,
    /// Snapshots to search as well as the base libraries (default: none):
    /// heads only, those that no other snapshot replaces.
    #[serde(default, skip_serializing_if = "SnapshotScope::is_none")]
    pub snapshots: SnapshotScope,
    /// How the result carries its hits (default `full`).
    #[serde(default, skip_serializing_if = "Format::is_full")]
    pub format: Format,
}

/// How a result carries its hits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// `hits`, with every field.
    #[default]
    Full,
    /// `text` alone: each hit's `text`, one after another, and no `hits`.
    Text,
}

impl Format {
    pub fn is_full(&self) -> bool {
        *self == Format::Full
    }
}

/// Which snapshot modules a query searches: `"none"`, `"all"` (those with a
/// head), or `{"labels": {"k": "v"}}`, the modules of the heads that have
/// every label. A head is a snapshot that no other snapshot replaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotScope {
    #[default]
    None,
    All,
    Labels(BTreeMap<String, String>),
}

impl SnapshotScope {
    pub fn is_none(&self) -> bool {
        *self == SnapshotScope::None
    }
}

impl Default for Query {
    fn default() -> Self {
        Query {
            text: None,
            description: None,
            latex: None,
            pattern: None,
            goal: None,
            name: None,
            consts: Vec::new(),
            libraries: Vec::new(),
            kinds: Vec::new(),
            limit: default_limit(),
            include_sorried: None,
            snapshots: SnapshotScope::None,
            format: Format::Full,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SearchResponse {
    /// The version the index was built from.
    pub version: String,
    /// One per query, in order.
    pub results: Vec<QueryResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct QueryResult {
    /// Empty with `format: "text"`.
    pub hits: Vec<Hit>,
    /// With `format: "text"`: the hits rendered, separated by blank lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// What made this result worse than usual, e.g. a lane that failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// The dense lane was left out because the query is still being
    /// embedded; asking again shortly will likely include it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dense_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Hit {
    pub name: String,
    pub kind: String,
    /// `PisaSnap` for a snapshot's declaration.
    pub library: String,
    pub module: String,
    /// The declaration's signature, with notation.
    pub signature: String,
    /// What a definition, abbreviation or opaque is defined as, with
    /// notation, past the parameters the signature names; cut at 1,000
    /// characters, ending in ` …`. A snapshot's, as its build printed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// What it is called in words, when a description exists, e.g.
    /// "Subadditivity of von Neumann entropy".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// One or two sentences describing it, when a description exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[schema(required = true)]
    #[serde(default)]
    pub flags: Flags,
    /// Fused score; only comparable within one query.
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// About 150 tokens for a model's context.
    pub text: String,
    /// For a declaration of a snapshot module (named in `module`, with the
    /// header that loads it in `flags.imports`): the ids of the query's
    /// heads on the module, newest first, at most 10.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshot_ids: Vec<String>,
    /// For a snapshot module's declaration: what its own axioms make it, as
    /// in the snapshot API, so a proved lemma next to a `sorry` is `proved`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ProofStatus>,
    /// For a snapshot module's declaration: its module and everything the
    /// module imports passed the kernel re-check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Flags {
    /// Depends on `sorry`: not usable as a proof step.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub uses_sorry: bool,
    /// A Physlib informal result: stated, not proved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub informal: bool,
    /// Deprecated; use this name instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_to: Option<String>,
    /// The `to_additive` counterpart, folded into this hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additive_sibling: Option<String>,
    /// The import header that loads it, one import a line, when the index's
    /// default header (`import Mathlib` and `import Physlib`) doesn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ResolveRequest {
    /// At most 64.
    pub names: Vec<String>,
    /// A version, or an alias of one: the index's, or its snapshots'. Any
    /// other fails with `not_found`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Search's state in a server's `GET /health`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SearchHealth {
    /// The index is open and Qdrant answers, so searches can be served.
    pub ready: bool,
    /// The version the index was built from.
    pub version: String,
    /// Qdrant answers.
    pub qdrant: bool,
    /// This replica leads the snapshot index.
    pub leader: bool,
    /// Why the index isn't open yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveResponse {
    /// The version the index was built from.
    pub version: String,
    /// One per name, in order.
    pub results: Vec<Resolution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Resolution {
    pub name: String,
    pub exists: bool,
    /// For a deprecated name, its replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_to: Option<String>,
    /// For an existing name, the header that loads it if the default
    /// imports don't, as in a hit's `flags.imports`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<String>,
    /// Closest existing names, best first; empty for an exact match.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Suggestion {
    pub name: String,
    pub kind: String,
    pub signature: String,
    /// The header that loads it if the default imports don't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GetDeclarationResponse {
    /// The version the index was built from.
    pub version: String,
    pub declaration: DeclarationDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeclarationDetail {
    pub name: String,
    pub kind: String,
    pub library: String,
    pub module: String,
    pub signature: String,
    /// The type with full names and no notation.
    pub type_full: String,
    /// As in [`Hit::value`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    /// What it states, in words, when a description exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Description>,
    #[schema(required = true)]
    #[serde(default)]
    pub flags: Flags,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// Declarations it uses, in its statement and its proof or body.
    pub dependencies: Vec<String>,
    /// Declarations that use it, most prominent first (at most 50).
    pub used_by: Vec<String>,
}

/// A natural-language description of a declaration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Informal {
    pub informal_name: String,
    pub statement_nl: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_latex: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Where it came from, e.g. "lsv2-v4.28.0-rc1" or a model name.
    pub source: String,
}

/// A natural-language description of a declaration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Description {
    /// What the declaration is called in words, e.g. "Subadditivity of von
    /// Neumann entropy".
    pub title: String,
    /// What it states, in words.
    pub text: String,
    /// What it states, in LaTeX.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latex: Option<String>,
    /// Other names it goes by.
    pub aliases: Vec<String>,
    /// Where it came from, e.g. "lsv2-v4.28.0-rc1" or a model name.
    pub source: String,
}

impl From<Informal> for Description {
    fn from(i: Informal) -> Self {
        Description {
            title: i.informal_name,
            text: i.statement_nl,
            latex: i.statement_latex,
            aliases: i.aliases,
            source: i.source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn query_defaults() {
        let q: Query =
            serde_json::from_value(json!({"description": "trace of a density matrix"})).unwrap();
        assert_eq!(q.limit, 5);
        assert_eq!(q.include_sorried, None);
        let f = Flags::default();
        assert_eq!(serde_json::to_value(&f).unwrap(), json!({}));
        assert_eq!(q.snapshots, SnapshotScope::None);
        assert!(serde_json::to_value(&q).unwrap().get("snapshots").is_none());
    }

    #[test]
    fn a_description_is_an_informal_under_public_names() {
        let informal = Informal {
            informal_name: "Name".into(),
            statement_nl: "Words.".into(),
            statement_latex: Some("x = x".into()),
            aliases: vec!["other".into()],
            source: "model".into(),
        };
        assert_eq!(
            serde_json::to_value(Description::from(informal)).unwrap(),
            json!({"title": "Name", "text": "Words.", "latex": "x = x",
                   "aliases": ["other"], "source": "model"})
        );
    }

    #[test]
    fn snapshot_scopes() {
        for (scope, wire) in [
            (SnapshotScope::None, json!("none")),
            (SnapshotScope::All, json!("all")),
            (
                SnapshotScope::Labels(BTreeMap::from([("run".into(), "7".into())])),
                json!({"labels": {"run": "7"}}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&scope).unwrap(), wire);
            let q: Query = serde_json::from_value(json!({"text": "x", "snapshots": wire})).unwrap();
            assert_eq!(q.snapshots, scope);
        }
        for bad in [json!("some"), json!({"label": {}}), json!(["all"])] {
            let q = serde_json::from_value::<Query>(json!({"text": "x", "snapshots": bad}));
            assert!(q.is_err(), "{bad}");
        }
    }
}
