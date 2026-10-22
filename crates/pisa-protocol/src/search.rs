//! The declaration search API (`pisa-search`).
//!
//! | Endpoint | Request → response |
//! | --- | --- |
//! | `POST /v1/search` | [`SearchRequest`] → [`SearchResponse`] |
//! | `POST /v1/resolve` | [`ResolveRequest`] → [`ResolveResponse`] |
//! | `GET /v1/decl/{name}` | → [`DeclResponse`] |
//!
//! Errors use [`crate::api::ErrorResponse`]. Unknown names and empty results
//! are ordinary answers, never errors. Every response names the `env_id` (the
//! project version) its index was built from; a request may pin one.

use serde::{Deserialize, Serialize};

fn default_k() -> usize {
    5
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchRequest {
    pub queries: Vec<Query>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<String>,
}

/// One search. Fill as many fields as you can: each feeds different lanes,
/// and more fields mean better fusion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    /// Free text from a person; classified as natural language, LaTeX, Lean or a name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// A natural-language description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nl: Option<String>,
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
    /// Restrict to these libraries, e.g. `["Physlib"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<String>,
    /// Restrict to these kinds, e.g. `["theorem"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Hits to return (default 5, at most 50).
    #[serde(default = "default_k")]
    pub k: usize,
    /// Also return declarations that depend on `sorry`, flagged.
    #[serde(default)]
    pub include_sorried: bool,
    /// Check hits against `goal` on a Lean worker (not available yet).
    #[serde(default)]
    pub verify: bool,
}

impl Default for Query {
    fn default() -> Self {
        Query {
            q: None,
            nl: None,
            latex: None,
            pattern: None,
            goal: None,
            name: None,
            libraries: Vec::new(),
            kinds: Vec::new(),
            k: default_k(),
            include_sorried: false,
            verify: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub env_id: String,
    /// One per query, in order.
    pub results: Vec<QueryResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryResult {
    pub hits: Vec<Hit>,
    /// Set when `verify` was asked for, e.g. "unavailable".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    /// What made this result worse than usual, e.g. a lane that failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub name: String,
    pub kind: String,
    pub library: String,
    pub module: String,
    /// The declaration's signature, with notation.
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub informal_name: Option<String>,
    /// One or two sentences describing it, when a description exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub flags: Flags,
    /// Fused score; only comparable within one query.
    pub score: f64,
    /// Which lanes found it, e.g. `["lexical", "name"]`.
    pub lanes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// About 150 tokens for a model's context.
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The import header that loads it, when the index's default header
    /// (`import Mathlib` and `import Physlib`) doesn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolveRequest {
    pub names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveResponse {
    pub env_id: String,
    pub results: Vec<Resolution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub name: String,
    pub exists: bool,
    /// For a deprecated name, its replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_to: Option<String>,
    /// Closest existing names, best first; empty for an exact match.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub name: String,
    pub kind: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclResponse {
    pub env_id: String,
    pub decl: DeclDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclDetail {
    pub name: String,
    pub kind: String,
    pub library: String,
    pub module: String,
    pub signature: String,
    /// The type with full names and no notation.
    pub type_full: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub informal: Option<Informal>,
    #[serde(default)]
    pub flags: Flags,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// Declarations it uses, in its statement and its proof or body.
    pub deps: Vec<String>,
    /// Declarations that use it, most prominent first (at most 50).
    pub used_by: Vec<String>,
}

/// A natural-language description of a declaration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn query_defaults() {
        let q: Query = serde_json::from_value(json!({"nl": "trace of a density matrix"})).unwrap();
        assert_eq!(q.k, 5);
        assert!(!q.include_sorried);
        let f = Flags::default();
        assert_eq!(serde_json::to_value(&f).unwrap(), json!({}));
    }
}
