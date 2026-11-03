//! Answering queries from Qdrant. Each lane is a prefetch of the
//! declarations collection, fused there by weighted RRF; a formula adds the
//! PageRank prior, intent bonuses and the exact-name bonus. The app resolves
//! label scopes to modules first, then folds deprecated aliases and
//! `to_additive` pairs and renders the hits.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use pisa_protocol::labels::check_labels;
use pisa_protocol::search::{
    DeclDetail, Hit, Query, QueryResult, Resolution, SnapshotHit, SnapshotScope, Suggestion,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::dense::{Embedder, QUERY_INSTRUCTION};
use crate::encode;
use crate::engine::{Lanes, Parsed, flags, parse, render, short_signature, summary};
use crate::index::BundleManifest;
use crate::points::{PointPayload, collection, entries_collection};
use crate::qdrant::{Qdrant, ScoredPoint, any, is};
use crate::symbols::Symbols;
use crate::text::levenshtein;

const MAX_K: usize = 50;

/// The most modules a label scope may cover; a broader one is refused.
pub const MAX_SCOPE_MODULES: usize = 20_000;

/// The most entries a snapshot hit names.
const MAX_HIT_ENTRIES: usize = 10;

/// The library of snapshot declarations.
pub const SNAPSHOT_LIBRARY: &str = pisa_protocol::snapshot::ROOT;

/// Fusion and scoring parameters, tuned on MathlibQR (see `search/eval/`).
///
/// Under Qdrant's RRF a weight below 1 makes a lane's ranks fall off fast:
/// lexical and structural matches lift their top few hits without crowding
/// out the dense lane further down, which retrieves best alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Weights {
    /// RRF's `k`. Qdrant's weights scale a lane's ranks: a result at rank
    /// `r` (from 0) of a lane with weight `w` adds `1/(k + (r+1)/w - 1)`.
    pub rrf_k: u32,
    pub lexical: f64,
    pub name: f64,
    /// The structural lane's weight when the query names no Lean
    /// identifiers, only symbols, and when it does.
    pub structural: f64,
    pub structural_identifiers: f64,
    pub dense: f64,
    pub exact_lane: f64,
    /// Prose against each field.
    pub prose_name: f32,
    pub prose_informal: f32,
    pub prose_statement: f32,
    pub prose_signature: f32,
    pub prose_doc: f32,
    /// Lean text against each field.
    pub lean_signature: f32,
    pub lean_name: f32,
    /// Weight and cap of log PageRank, added after fusion.
    pub pagerank: f64,
    pub pagerank_cap: f64,
    /// Added for a definition when the query asks for one, or a theorem.
    pub intent: f64,
    /// Added for the declaration named exactly.
    pub exact: f64,
    /// Candidates each lane contributes.
    pub depth: usize,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            rrf_k: 30,
            lexical: 0.02,
            name: 3.0,
            structural: 0.02,
            structural_identifiers: 1.0,
            dense: 1.0,
            exact_lane: 1.0,
            prose_name: 1.0,
            prose_informal: 2.0,
            prose_statement: 1.5,
            prose_signature: 1.0,
            prose_doc: 1.2,
            lean_signature: 2.0,
            lean_name: 1.5,
            pagerank: 0.008,
            pagerank_cap: 4.0,
            intent: 0.002,
            exact: 1.0,
            depth: 100,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Unavailable(#[from] anyhow::Error),
}

pub struct Engine {
    pub q: Qdrant,
    pub collection: String,
    pub entries: String,
    pub manifest: BundleManifest,
    pub weights: Weights,
    symbols: Symbols,
    pub embedder: Option<Embedder>,
    /// Whether queries may ask for snapshots.
    pub snapshots: bool,
    /// Query vectors by text, kept while evaluating many weightings.
    pub cache: Option<Mutex<HashMap<String, Vec<f32>>>>,
}

/// A candidate hit.
struct Scored {
    score: f64,
    payload: PointPayload,
    sibling: Option<String>,
}

fn and(filters: Vec<Value>) -> Value {
    json!({"must": filters})
}

const DEF_KINDS: [&str; 5] = ["def", "abbrev", "structure", "class", "inductive"];

/// The payload a hit needs: everything but `/v1/decl`'s lists.
fn hit_payload() -> Value {
    json!({"exclude": ["deps", "used_by"]})
}

impl Engine {
    /// The engine for the bundle loaded for `env_id`, dropping symbol
    /// constants that the bundle doesn't have.
    pub async fn open(
        q: Qdrant,
        env_id: &str,
        symbols: Symbols,
        embedder: Option<Embedder>,
    ) -> anyhow::Result<Engine> {
        let c = collection(env_id);
        let found = q
            .query_batch(
                &c,
                vec![json!({"filter": and(vec![is("source", "manifest")]), "limit": 1, "with_payload": true})],
            )
            .await
            .map_err(|e| anyhow::anyhow!("no bundle loaded for env {env_id}: {e}"))?;
        let manifest = found
            .first()
            .and_then(|p| p.first())
            .and_then(|p| p.payload.as_ref()?.get("manifest").cloned())
            .ok_or_else(|| anyhow::anyhow!("no bundle loaded for env {env_id}"))?;
        let manifest: BundleManifest = serde_json::from_value(manifest)?;
        let constants: Vec<String> = symbols.all_constants().into_iter().collect();
        let searches = constants
            .iter()
            .map(|c| json!({"filter": and(vec![is("name", c.as_str()), is("source", "base")]), "limit": 1}))
            .collect();
        let exists = q.query_batch(&c, searches).await?;
        let known: HashSet<&str> = constants
            .iter()
            .zip(&exists)
            .filter(|(_, found)| !found.is_empty())
            .map(|(c, _)| c.as_str())
            .collect();
        let symbols = symbols.retain_known(|c| known.contains(c));
        Ok(Engine {
            q,
            collection: c,
            entries: entries_collection(env_id),
            manifest,
            weights: Weights::default(),
            symbols,
            embedder,
            snapshots: false,
            cache: None,
        })
    }

    pub fn env_id(&self) -> &str {
        &self.manifest.env_id
    }

    /// Spells a query's library and kind filters as the index does, matching
    /// case-insensitively, and checks its snapshot scope.
    pub fn check_filters(&self, q: &mut Query) -> Result<(), String> {
        match &q.snapshots {
            SnapshotScope::None => {}
            _ if !self.snapshots => return Err("this server doesn't search snapshots".into()),
            SnapshotScope::All => {}
            SnapshotScope::Labels(labels) if labels.is_empty() => {
                return Err("snapshots: no labels given; use \"all\" for every snapshot".into());
            }
            SnapshotScope::Labels(labels) => check_labels(labels)?,
        }
        let snapshot_library = SNAPSHOT_LIBRARY.to_string();
        let mut libraries: Vec<&String> = self.manifest.libraries.keys().collect();
        if self.snapshots {
            libraries.push(&snapshot_library);
        }
        let kinds: Vec<&String> = self.manifest.kinds.iter().collect();
        for (wanted, known, what) in [
            (&mut q.libraries, &libraries, "library"),
            (&mut q.kinds, &kinds, "kind"),
        ] {
            for w in wanted.iter_mut() {
                match known.iter().find(|k| k.eq_ignore_ascii_case(w)) {
                    Some(k) => *w = k.to_string(),
                    None => {
                        let names: Vec<String> = known.iter().map(|k| k.to_string()).collect();
                        return Err(format!("unknown {what} {w:?}; known: {}", names.join(", ")));
                    }
                }
            }
        }
        if q.snapshots.is_none() && q.libraries.contains(&snapshot_library) {
            return Err(format!(
                "library {snapshot_library} needs snapshots: \"all\" or labels"
            ));
        }
        Ok(())
    }

    pub async fn search(&self, q: &Query) -> Result<QueryResult, SearchError> {
        let mut r = self
            .search_batch(std::slice::from_ref(q), Lanes::ALL)
            .await?;
        Ok(r.pop().unwrap_or_default())
    }

    /// Several queries, with one embedding request and one Qdrant request
    /// for all of them; results in order.
    pub async fn search_batch(
        &self,
        queries: &[Query],
        lanes: Lanes,
    ) -> Result<Vec<QueryResult>, SearchError> {
        let parsed: Vec<Parsed> = queries.iter().map(parse).collect();
        let mut results: Vec<QueryResult> = queries
            .iter()
            .map(|q| QueryResult {
                verification: q.verify.then(|| "unavailable".to_string()),
                ..Default::default()
            })
            .collect();
        let vectors = self.embed(queries, &parsed, lanes).await;
        let modules = self.scopes(queries).await?;
        let mut searches = Vec::new();
        let mut owners = Vec::new();
        for (i, q) in queries.iter().enumerate() {
            if q.k == 0 {
                continue;
            }
            let vector = match &vectors[i] {
                Some(Ok(v)) => Some(v.as_slice()),
                Some(Err(e)) => {
                    tracing::warn!("query embedding failed: {e}");
                    results[i].warnings.push(format!(
                        "dense lane unavailable, results from the other lanes only: {e}"
                    ));
                    None
                }
                None => None,
            };
            if let Some(body) = self.request(q, &parsed[i], vector, modules[i].as_deref(), lanes) {
                searches.push(body);
                owners.push(i);
            }
        }
        let found = self.q.query_batch(&self.collection, searches).await?;
        let mut candidates: Vec<Vec<Scored>> = (0..queries.len()).map(|_| Vec::new()).collect();
        for (&i, points) in owners.iter().zip(found) {
            candidates[i] = points.into_iter().filter_map(scored).collect();
        }
        self.fold(&mut candidates).await?;
        let mut picked: Vec<Vec<Scored>> = Vec::new();
        for (q, mut c) in queries.iter().zip(candidates) {
            c.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.payload.name.cmp(&b.payload.name))
            });
            c.truncate(q.k.min(MAX_K));
            picked.push(c);
        }
        let entries = self.hit_entries(queries, &picked).await?;
        for (i, hits) in picked.into_iter().enumerate() {
            results[i].hits = hits
                .into_iter()
                .enumerate()
                .map(|(n, s)| {
                    let snapshot = s.payload.module_id.as_ref().map(|m| SnapshotHit {
                        entries: entries.get(&(i, m.clone())).cloned().unwrap_or_default(),
                        dirty: s.payload.dirty,
                        verified: s.payload.verified,
                    });
                    hit(n + 1, s, snapshot)
                })
                .collect();
        }
        Ok(results)
    }

    /// Each query's vector for the dense lane, or why there is none.
    async fn embed(
        &self,
        queries: &[Query],
        parsed: &[Parsed],
        lanes: Lanes,
    ) -> Vec<Option<Result<Vec<f32>, String>>> {
        let mut out: Vec<Option<Result<Vec<f32>, String>>> = vec![None; queries.len()];
        let Some(embedder) = &self.embedder else {
            return out;
        };
        if !lanes.dense {
            return out;
        }
        let mut wanted: Vec<usize> = Vec::new();
        for i in 0..queries.len() {
            if queries[i].k == 0 || parsed[i].dense_text.is_empty() {
                continue;
            }
            let cached = self
                .cache
                .as_ref()
                .and_then(|c| c.lock().unwrap().get(&parsed[i].dense_text).cloned());
            match cached {
                Some(v) => out[i] = Some(Ok(v)),
                None => wanted.push(i),
            }
        }
        if wanted.is_empty() {
            return out;
        }
        let texts: Vec<String> = wanted
            .iter()
            .map(|&i| format!("{QUERY_INSTRUCTION}{}", parsed[i].dense_text))
            .collect();
        let embedder = embedder.clone();
        let embedded = tokio::task::spawn_blocking(move || embedder.embed_queries(&texts)).await;
        match embedded {
            Ok(Ok(vectors)) => {
                for (&i, v) in wanted.iter().zip(vectors) {
                    if let Some(c) = &self.cache {
                        c.lock()
                            .unwrap()
                            .insert(parsed[i].dense_text.clone(), v.clone());
                    }
                    out[i] = Some(Ok(v));
                }
            }
            Ok(Err(e)) => {
                for &i in &wanted {
                    out[i] = Some(Err(e.to_string()));
                }
            }
            Err(e) => {
                for &i in &wanted {
                    out[i] = Some(Err(e.to_string()));
                }
            }
        }
        out
    }

    /// The modules of each label scope, from the entries collection.
    async fn scopes(&self, queries: &[Query]) -> Result<Vec<Option<Vec<String>>>, SearchError> {
        let lookups = queries.iter().map(|q| async move {
            let SnapshotScope::Labels(labels) = &q.snapshots else {
                return Ok(None);
            };
            let filter = and(labels
                .iter()
                .map(|(k, v)| is("labels", format!("{k}={v}")))
                .collect());
            let modules = self
                .q
                .facet(&self.entries, "module_id", filter, MAX_SCOPE_MODULES + 1)
                .await?;
            if modules.len() > MAX_SCOPE_MODULES {
                return Err(SearchError::BadRequest(format!(
                    "snapshots: these labels cover more than {MAX_SCOPE_MODULES} modules; \
                     add labels or use \"all\""
                )));
            }
            Ok(Some(modules))
        });
        futures::future::try_join_all(lookups).await
    }

    /// The declarations a query may see: its scope, libraries and kinds.
    fn filter(&self, q: &Query, modules: Option<&[String]>) -> Value {
        let base = and(vec![is("source", "base")]);
        let scope = match &q.snapshots {
            SnapshotScope::None => base,
            SnapshotScope::All => json!({"should": [
                base,
                and(vec![is("source", "snapshot"), is("has_entries", true)]),
            ]}),
            SnapshotScope::Labels(_) => match modules {
                Some(ms) if !ms.is_empty() => json!({"should": [
                    base,
                    and(vec![any("module_id", ms)]),
                ]}),
                _ => base,
            },
        };
        let mut must = vec![scope];
        if !q.libraries.is_empty() {
            must.push(any("library", &q.libraries));
        }
        if !q.kinds.is_empty() {
            must.push(any("kind", &q.kinds));
        }
        and(must)
    }

    /// The Query API request for one query, or `None` if no lane applies.
    fn request(
        &self,
        q: &Query,
        p: &Parsed,
        vector: Option<&[f32]>,
        modules: Option<&[String]>,
        lanes: Lanes,
    ) -> Option<Value> {
        let w = &self.weights;
        let scope = self.filter(q, modules);
        let mut hidden = vec![is("hidden", true)];
        if !q.include_sorried {
            hidden.push(is("uses_sorry", true));
        }
        let visible = json!({"must": [scope.clone()], "must_not": hidden});
        let mut prefetch = Vec::new();
        let mut weights = Vec::new();
        let mut lane = |query: Value, using: &str, weight: f64| {
            prefetch.push(json!({
                "query": query, "using": using, "filter": visible, "limit": w.depth,
            }));
            weights.push(weight);
        };
        if lanes.lexical {
            let plan: [(&[String], &str, f32); 7] = [
                (&p.prose, "name", w.prose_name),
                (&p.prose, "informal", w.prose_informal),
                (&p.prose, "statement", w.prose_statement),
                (&p.prose, "signature", w.prose_signature),
                (&p.prose, "doc", w.prose_doc),
                (&p.lean, "signature", w.lean_signature),
                (&p.lean, "name", w.lean_name),
            ];
            let v = encode::lexical_query(&plan);
            if !v.is_empty() {
                lane(json!(v), "lexical", w.lexical);
            }
        }
        if lanes.name
            && let Some(name) = &p.name
        {
            let v = encode::trigrams(name);
            if !v.is_empty() {
                lane(json!(v), "trigrams", w.name);
            }
        }
        let mut consts: Vec<(String, f32)> =
            p.identifiers.iter().map(|i| (i.clone(), 1.0)).collect();
        let all_tokens: Vec<String> = p.prose.iter().chain(&p.lean).cloned().collect();
        consts.extend(self.symbols.constants(&all_tokens));
        if lanes.structural && !consts.is_empty() {
            // Symbols alone are weaker evidence than names written in Lean.
            let weight = if p.identifiers.is_empty() {
                w.structural
            } else {
                w.structural_identifiers
            };
            lane(json!(encode::consts_query(&consts)), "consts", weight);
        }
        if lanes.dense
            && let Some(v) = vector
        {
            lane(json!(v), "dense", w.dense);
        }
        let exact = p.name.as_deref().filter(|_| lanes.name);
        if let Some(name) = exact {
            // Shown whatever it is, so without the visibility filter.
            prefetch.push(json!({
                "filter": and(vec![scope, is("name", name)]), "limit": 5,
            }));
            weights.push(w.exact_lane);
        }
        if prefetch.is_empty() {
            return None;
        }
        let prose: HashSet<&str> = p.prose.iter().map(String::as_str).collect();
        let wants_def = ["definition", "def", "structure", "class", "type"]
            .iter()
            .any(|w| prose.contains(w));
        let wants_thm = [
            "theorem",
            "lemma",
            "inequality",
            "identity",
            "equality",
            "bound",
        ]
        .iter()
        .any(|w| prose.contains(w));
        let mut terms = vec![
            json!("$score"),
            json!({"mult": [w.pagerank, {"min": [{"ln": {"sum": [1.0, "pagerank"]}}, w.pagerank_cap]}]}),
        ];
        if wants_def {
            let kinds: Vec<String> = DEF_KINDS.iter().map(|k| k.to_string()).collect();
            terms.push(json!({"mult": [w.intent, any("kind", &kinds)]}));
        }
        if wants_thm {
            terms.push(json!({"mult": [w.intent, is("kind", "theorem")]}));
        }
        if let Some(name) = exact {
            terms.push(json!({"mult": [w.exact, is("name", name)]}));
        }
        let limit = (q.k.min(MAX_K) * 3).max(30).min(w.depth);
        Some(json!({
            "prefetch": [{
                "prefetch": prefetch,
                "query": {"rrf": {"k": w.rrf_k, "weights": weights}},
                "limit": w.depth,
            }],
            "query": {"formula": {"sum": terms}, "defaults": {"pagerank": 0.0}},
            "limit": limit,
            "with_payload": hit_payload(),
        }))
    }

    /// Folds each query's deprecated aliases into their targets, fetched
    /// when they aren't among the candidates, and `to_additive` pairs into
    /// the better-scored one.
    async fn fold(&self, candidates: &mut [Vec<Scored>]) -> anyhow::Result<()> {
        let mut missing: Vec<String> = Vec::new();
        for c in candidates.iter() {
            let names: HashSet<&str> = c.iter().map(|s| s.payload.name.as_str()).collect();
            for s in c {
                if let Some(t) = &s.payload.deprecated_to
                    && s.payload.source == "base"
                    && !names.contains(t.as_str())
                    && !missing.contains(t)
                {
                    missing.push(t.clone());
                }
            }
        }
        let searches = missing
            .iter()
            .map(|t| {
                json!({
                    "filter": and(vec![is("name", t.as_str()), is("source", "base")]),
                    "limit": 1, "with_payload": hit_payload(),
                })
            })
            .collect();
        let fetched: HashMap<String, PointPayload> = missing
            .iter()
            .cloned()
            .zip(self.q.query_batch(&self.collection, searches).await?)
            .filter_map(|(t, found)| Some((t, scored(found.into_iter().next()?)?.payload)))
            .collect();
        for c in candidates.iter_mut() {
            let mut out: Vec<Scored> = Vec::with_capacity(c.len());
            let mut at: HashMap<String, usize> = HashMap::new();
            for s in std::mem::take(c) {
                let (key, s) = match s.payload.deprecated_to.clone() {
                    Some(t) if s.payload.source == "base" => match fetched.get(&t) {
                        // A deprecated alias counts towards its replacement.
                        Some(target) => (
                            t,
                            Scored {
                                payload: target.clone(),
                                ..s
                            },
                        ),
                        None => (t, s),
                    },
                    _ => (s.payload.name.clone(), s),
                };
                match at.get(&key) {
                    Some(&i) => out[i].score = out[i].score.max(s.score),
                    None => {
                        at.insert(key, out.len());
                        out.push(s);
                    }
                }
            }
            // Fold to_additive pairs: keep the better-scored one, name the other.
            let by_name: HashMap<String, usize> = out
                .iter()
                .enumerate()
                .map(|(i, s)| (s.payload.name.clone(), i))
                .collect();
            let mut dropped = vec![false; out.len()];
            for i in 0..out.len() {
                let Some(j) = out[i]
                    .payload
                    .additive_sibling
                    .as_ref()
                    .and_then(|sib| by_name.get(sib))
                    .copied()
                else {
                    continue;
                };
                if dropped[i] || dropped[j] || i == j {
                    continue;
                }
                let (keep, drop) = if out[i].score >= out[j].score {
                    (i, j)
                } else {
                    (j, i)
                };
                dropped[drop] = true;
                out[keep].sibling = Some(out[drop].payload.name.clone());
            }
            *c = out
                .into_iter()
                .zip(dropped)
                .filter(|(_, d)| !d)
                .map(|(s, _)| s)
                .collect();
        }
        Ok(())
    }

    /// For each query's snapshot hits, the query's entries on their modules,
    /// newest first.
    async fn hit_entries(
        &self,
        queries: &[Query],
        picked: &[Vec<Scored>],
    ) -> anyhow::Result<HashMap<(usize, String), Vec<String>>> {
        let mut keys: Vec<(usize, String)> = Vec::new();
        let mut searches = Vec::new();
        for (i, hits) in picked.iter().enumerate() {
            let mut seen = HashSet::new();
            for m in hits.iter().filter_map(|s| s.payload.module_id.as_ref()) {
                if !seen.insert(m) {
                    continue;
                }
                let mut must = vec![is("module_id", m.as_str())];
                if let SnapshotScope::Labels(labels) = &queries[i].snapshots {
                    must.extend(labels.iter().map(|(k, v)| is("labels", format!("{k}={v}"))));
                }
                searches.push(json!({
                    "query": {"order_by": {"key": "created", "direction": "desc"}},
                    "filter": and(must),
                    "limit": MAX_HIT_ENTRIES,
                    "with_payload": ["entry"],
                }));
                keys.push((i, m.clone()));
            }
        }
        let found = self.q.query_batch(&self.entries, searches).await?;
        Ok(keys
            .into_iter()
            .zip(found)
            .map(|(k, points)| {
                let entries = points
                    .into_iter()
                    .filter_map(|p| p.payload?.get("entry")?.as_str().map(str::to_string))
                    .collect();
                (k, entries)
            })
            .collect())
    }

    /// Whether each name exists, with its replacement if deprecated, and
    /// otherwise the closest existing names: same name in other case, same
    /// last component, or shared trigrams, ranked by edit distance, then
    /// prominence.
    pub async fn resolve(&self, names: &[String]) -> anyhow::Result<Vec<Resolution>> {
        let base = is("source", "base");
        let fields = json!(["name", "kind", "signature", "pagerank", "deprecated_to"]);
        let mut searches = Vec::new();
        for name in names {
            let name = name.trim();
            let lower = name.to_lowercase();
            let last = lower.rsplit('.').next().unwrap_or(&lower).to_string();
            let filter = |field: &str, value: &str| and(vec![base.clone(), is(field, value)]);
            searches
                .push(json!({"filter": filter("name", name), "limit": 1, "with_payload": fields}));
            searches.push(json!({"filter": filter("name_lower", &lower), "limit": 50, "with_payload": fields}));
            searches.push(json!({"filter": filter("last_lower", &last), "limit": 200, "with_payload": fields}));
            let grams = encode::trigrams(&lower);
            if grams.is_empty() {
                searches.push(
                    json!({"filter": filter("name", name), "limit": 1, "with_payload": fields}),
                );
            } else {
                searches.push(json!({
                    "query": grams, "using": "trigrams", "filter": and(vec![base.clone()]),
                    "limit": 100, "with_payload": fields,
                }));
            }
        }
        let found = self.q.query_batch(&self.collection, searches).await?;
        let mut out = Vec::new();
        for (name, found) in names.iter().zip(found.chunks(4)) {
            let name = name.trim();
            if let Some(exact) = found[0].first().and_then(|p| scored(p.clone())) {
                out.push(Resolution {
                    name: name.to_string(),
                    exists: true,
                    deprecated_to: exact.payload.deprecated_to,
                    suggestions: Vec::new(),
                });
                continue;
            }
            let lower = name.to_lowercase();
            let last = lower.rsplit('.').next().unwrap_or(&lower).to_string();
            let mut seen = HashSet::new();
            let mut ranked: Vec<(usize, f32, String, PointPayload)> = found[1..]
                .iter()
                .flatten()
                .filter_map(|p| scored(p.clone()))
                .filter(|s| s.payload.name != name && seen.insert(s.payload.name.clone()))
                .map(|s| {
                    let d = s.payload;
                    let full = levenshtein(&lower, &d.name_lower);
                    let tail = levenshtein(&last, &d.last_lower);
                    (full.min(tail + 1), -d.pagerank, d.name.clone(), d)
                })
                .collect();
            ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
            out.push(Resolution {
                name: name.to_string(),
                exists: false,
                deprecated_to: None,
                suggestions: ranked
                    .into_iter()
                    .take(5)
                    .map(|(_, _, _, d)| {
                        let s = d.stored();
                        Suggestion {
                            name: d.name,
                            kind: d.kind,
                            signature: short_signature(&s),
                        }
                    })
                    .collect(),
            });
        }
        Ok(out)
    }

    /// The bundle's declaration `name` with its description, dependencies and users.
    pub async fn detail(&self, name: &str) -> anyhow::Result<Option<DeclDetail>> {
        let search = json!({
            "filter": and(vec![is("name", name), is("source", "base")]),
            "limit": 1, "with_payload": true,
        });
        let found = self.q.query_batch(&self.collection, vec![search]).await?;
        let Some(s) = found.into_iter().flatten().next().and_then(scored) else {
            return Ok(None);
        };
        let d = s.payload;
        let stored = d.stored();
        Ok(Some(DeclDetail {
            name: d.name.clone(),
            kind: d.kind.clone(),
            library: d.library.clone(),
            module: d.module.clone(),
            signature: d.signature.clone(),
            type_full: d.type_full.clone(),
            docstring: d.docstring.clone(),
            informal: d.informal.clone(),
            flags: flags(&stored, None),
            source_url: d.source_url.clone(),
            deps: d.deps,
            used_by: d.used_by,
        }))
    }
}

fn scored(p: ScoredPoint) -> Option<Scored> {
    let payload: PointPayload = serde_json::from_value(p.payload?).ok()?;
    Some(Scored {
        score: f64::from(p.score),
        payload,
        sibling: None,
    })
}

fn hit(n: usize, s: Scored, snapshot: Option<SnapshotHit>) -> Hit {
    let d = s.payload.stored();
    let flags = flags(&d, s.sibling);
    let summary = summary(&d);
    Hit {
        name: d.name.clone(),
        kind: d.kind.clone(),
        library: d.library.clone(),
        module: d.module.clone(),
        signature: d.signature.clone(),
        informal_name: d.informal.as_ref().map(|i| i.informal_name.clone()),
        text: render(n, &d, summary.as_deref(), &flags),
        summary,
        flags,
        score: s.score,
        lanes: Vec::new(),
        source_url: d.source_url.clone(),
        snapshot,
    }
}

/// The lanes fused for a weighting, by label, for reports.
pub fn lane_configs(fused_only: bool) -> Vec<(&'static str, Lanes)> {
    let none = Lanes {
        lexical: false,
        name: false,
        structural: false,
        dense: false,
    };
    let mut configs = vec![("fused", Lanes::ALL)];
    if !fused_only {
        configs.push((
            "lexical",
            Lanes {
                lexical: true,
                ..none
            },
        ));
        configs.push((
            "structural",
            Lanes {
                structural: true,
                ..none
            },
        ));
        configs.push(("name", Lanes { name: true, ..none }));
        configs.push((
            "dense",
            Lanes {
                dense: true,
                ..none
            },
        ));
    }
    configs
}

/// Labels for a list of weightings in a report.
pub fn weight_labels(n: usize) -> Vec<String> {
    if n <= 1 {
        return vec![String::new()];
    }
    (1..=n).map(|i| format!(" #{i}")).collect()
}
