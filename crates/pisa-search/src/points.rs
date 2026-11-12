//! What the Qdrant collections hold: a point per declaration, of the bundle
//! or of a snapshot module, in `pisa-<env_id>`; a point per snapshot entry in
//! `pisa-<env_id>-entries`; and the loaded bundle's manifest.

use std::collections::BTreeMap;

use pisa_protocol::api::ProofStatus;
use pisa_protocol::search::Informal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::corpus::{is_generated, is_tooling};
use crate::encode::{self, Sparse};
use crate::index::StoredDecl;
use crate::qdrant::uuid;

/// The declarations collection of `env_id`.
pub fn collection(env_id: &str) -> String {
    format!("pisa-{env_id}")
}

/// The snapshot entries collection of `env_id`.
pub fn entries_collection(env_id: &str) -> String {
    format!("pisa-{env_id}-entries")
}

fn id(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    uuid(&h.finalize()[..16].try_into().expect("16 bytes"))
}

/// The point of the bundle's declaration `name`.
pub fn base_id(name: &str) -> String {
    id(&["base", name])
}

/// The point of declaration `name` of snapshot module `m`.
pub fn snapshot_id(m: &str, name: &str) -> String {
    id(&["snap", m, name])
}

/// The point of snapshot entry `e`.
pub fn entry_id(e: &str) -> String {
    id(&["entry", e])
}

/// What a declaration's point carries besides its vectors. Every field has
/// a default, so a request for some of them reads as one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PointPayload {
    /// `base` or `snapshot`.
    pub source: String,
    pub name: String,
    pub name_lower: String,
    pub last_lower: String,
    pub kind: String,
    pub library: String,
    pub module: String,
    pub signature: String,
    pub type_full: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub informal: Option<Informal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additive_sibling: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default)]
    pub uses_sorry: bool,
    #[serde(default)]
    pub informal_result: bool,
    /// Generated or tooling: shown only when asked for by name.
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub pagerank: f32,
    /// What it uses and what uses it, by name, without plumbing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<String>,
    /// For a snapshot declaration: its module's id, its own status, and its
    /// module's state (whether it has entries, and heads among them) and
    /// revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ProofStatus>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub has_entries: bool,
    #[serde(default)]
    pub has_heads: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
}

impl PointPayload {
    pub fn new(source: &str, s: &StoredDecl) -> PointPayload {
        PointPayload {
            source: source.to_string(),
            name: s.name.clone(),
            name_lower: s.name.to_lowercase(),
            last_lower: s.name.rsplit('.').next().unwrap_or(&s.name).to_lowercase(),
            kind: s.kind.clone(),
            library: s.library.clone(),
            module: s.module.clone(),
            signature: s.signature.clone(),
            type_full: s.type_full.clone(),
            docstring: s.docstring.clone(),
            informal: s.informal.clone(),
            deprecated_to: s.deprecated_to.clone(),
            additive_sibling: s.additive_sibling.clone(),
            imports: s.imports.clone(),
            source_url: s.source_url.clone(),
            uses_sorry: s.uses_sorry,
            informal_result: s.informal_result,
            hidden: is_generated(&s.name) || is_tooling(&s.name),
            pagerank: s.pagerank,
            ..Default::default()
        }
    }

    /// The declaration as hits render it.
    pub fn stored(&self) -> StoredDecl {
        StoredDecl {
            name: self.name.clone(),
            kind: self.kind.clone(),
            module: self.module.clone(),
            library: self.library.clone(),
            signature: self.signature.clone(),
            type_full: self.type_full.clone(),
            docstring: self.docstring.clone(),
            deprecated_to: self.deprecated_to.clone(),
            additive_sibling: self.additive_sibling.clone(),
            uses_sorry: self.uses_sorry,
            imports: self.imports.clone(),
            informal_result: self.informal_result,
            informal: self.informal.clone(),
            source_url: self.source_url.clone(),
            pagerank: self.pagerank,
            ..Default::default()
        }
    }
}

/// A declaration's point: its payload, its sparse vectors from `s` (with the
/// bundle's `field_stats`), and `dense` if it has one.
pub fn point(
    id: String,
    payload: &PointPayload,
    s: &StoredDecl,
    dense: Option<&[f32]>,
    field_stats: &BTreeMap<String, f32>,
) -> Value {
    let mut vector = serde_json::Map::new();
    if let Some(v) = dense {
        vector.insert("dense".into(), json!(v));
    }
    let sparse: [(&str, Sparse); 4] = [
        ("lexical", encode::lexical_doc(s, field_stats)),
        ("consts", encode::consts_doc(&s.type_refs)),
        ("concl", encode::consts_doc(&s.concl)),
        ("trigrams", encode::trigrams(&s.name)),
    ];
    for (name, v) in sparse {
        if !v.is_empty() {
            vector.insert(name.into(), json!(v));
        }
    }
    json!({"id": id, "vector": vector, "payload": payload})
}

/// The declarations collection: one shard, as IDF is per shard.
pub fn collection_config(replication: u32) -> Value {
    let sparse = json!({"modifier": "idf"});
    json!({
        "vectors": {"dense": {
            "size": crate::dense::DIMS, "distance": "Cosine", "on_disk": true,
            "quantization_config": {"scalar": {"type": "int8", "always_ram": true}},
        }},
        "sparse_vectors": {"lexical": sparse, "consts": sparse, "concl": sparse, "trigrams": sparse},
        "shard_number": 1,
        "replication_factor": replication,
        "on_disk_payload": true,
    })
}

/// The payload indexes of the declarations collection.
pub const INDEXES: &[(&str, &str)] = &[
    ("source", "keyword"),
    ("name", "keyword"),
    ("name_lower", "keyword"),
    ("last_lower", "keyword"),
    ("kind", "keyword"),
    ("library", "keyword"),
    ("module_id", "keyword"),
    ("hidden", "bool"),
    ("uses_sorry", "bool"),
    ("has_entries", "bool"),
    ("has_heads", "bool"),
];

/// The entries collection: payload only.
pub fn entries_config(replication: u32) -> Value {
    json!({"shard_number": 1, "replication_factor": replication})
}

pub const ENTRY_INDEXES: &[(&str, &str)] = &[
    ("module_id", "keyword"),
    ("labels", "keyword"),
    ("created", "integer"),
    ("head", "bool"),
];

/// A snapshot entry's point; `head` if no entry replaces it.
pub fn entry_point(
    e: &str,
    module: &str,
    created: u64,
    labels: &BTreeMap<String, String>,
    head: bool,
) -> Value {
    let labels: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
    json!({
        "id": entry_id(e),
        "vector": {},
        "payload": {
            "entry": e, "module_id": module, "created": created, "labels": labels, "head": head,
        },
    })
}

/// Dependencies and users that a declaration's detail leaves out: instances
/// and classes, generated names, and core plumbing of non-core declarations.
pub fn is_plumbing(d: &StoredDecl, other: &StoredDecl) -> bool {
    let core = |l: &str| matches!(l, "Init" | "Std" | "Lean");
    matches!(other.kind.as_str(), "instance" | "class")
        || is_generated(&other.name)
        || (core(&other.library) && !core(&d.library))
}

/// For each declaration, the 50 most prominent declarations that use it.
pub fn used_by(decls: &[StoredDecl]) -> Vec<Vec<u32>> {
    let mut used_by: Vec<Vec<u32>> = vec![Vec::new(); decls.len()];
    for (i, d) in decls.iter().enumerate() {
        for &dep in &d.deps {
            used_by[dep as usize].push(i as u32);
        }
    }
    for users in &mut used_by {
        users.sort_by(|a, b| {
            let (pa, pb) = (decls[*a as usize].pagerank, decls[*b as usize].pagerank);
            pb.total_cmp(&pa).then(a.cmp(b))
        });
        users.truncate(50);
    }
    used_by
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_differ_by_kind_and_are_stable() {
        assert_eq!(base_id("Nat.add_comm"), base_id("Nat.add_comm"));
        assert_ne!(base_id("x"), snapshot_id("m", "x"));
        assert_ne!(snapshot_id("m", "x"), snapshot_id("n", "x"));
        assert_eq!(base_id("x").len(), 36);
    }

    #[test]
    fn payloads_round_trip_what_hits_render() {
        let s = StoredDecl {
            name: "Mathlib.Tactic.foo".into(),
            kind: "def".into(),
            signature: "foo : Nat".into(),
            pagerank: 2.0,
            ..Default::default()
        };
        let p = PointPayload::new("base", &s);
        assert!(p.hidden);
        assert_eq!(
            (p.name_lower.as_str(), p.last_lower.as_str()),
            ("mathlib.tactic.foo", "foo")
        );
        assert_eq!(p.stored().signature, "foo : Nat");
        let v = point(base_id(&s.name), &p, &s, None, &BTreeMap::new());
        assert!(v["vector"].get("dense").is_none() && v["vector"].get("consts").is_none());
        assert!(v["vector"]["trigrams"]["indices"].is_array());
        assert!(v["vector"].get("concl").is_none());
        let t = StoredDecl {
            concl: vec!["Eq".into(), "Matrix.trace".into()],
            ..s
        };
        let v = point(base_id(&t.name), &p, &t, None, &BTreeMap::new());
        assert_eq!(v["vector"]["concl"]["indices"].as_array().unwrap().len(), 2);
        assert!(collection_config(1)["sparse_vectors"]["concl"].is_object());
    }
}
