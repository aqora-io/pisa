//! Answering queries from Qdrant. Each lane is a prefetch of the
//! declarations collection, fused there by weighted RRF; a formula adds the
//! PageRank prior, intent bonuses and the exact-name bonus. The app resolves
//! label scopes to modules first, then folds deprecated aliases and
//! `to_additive` pairs and renders the hits.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lru::LruCache;
use pisa_protocol::api::SnapshotId;
use pisa_protocol::labels::check_labels;
use pisa_protocol::search::{
    DeclarationDetail, Description, Flags, Hit, Query, QueryResult, Resolution, SnapshotScope,
    Suggestion,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

use crate::dense::{Embedder, QUERY_INSTRUCTION};
use crate::encode;
use crate::index::{BundleManifest, StoredDecl};
use crate::points::{PointPayload, collection, entries_collection};
use crate::qdrant::{Qdrant, ScoredPoint, any, is};
use crate::symbols::Symbols;
use crate::text::{latex_to_unicode, lean_identifiers, levenshtein, tokens};

const MAX_LIMIT: usize = 50;

/// The most modules a label scope may cover; a broader one is refused.
pub const MAX_SCOPE_MODULES: usize = 20_000;

/// The most snapshot ids a hit names.
const MAX_HIT_ENTRIES: usize = 10;

/// How many query vectors an engine keeps, about 2 KB each: the embedder is
/// slow, and a search that changes only its filters sends the same text.
const QUERY_VECTORS: NonZeroUsize = NonZeroUsize::new(1024).unwrap();

/// How long a search waits for its query's vector. The embedding goes on
/// after that, and its vector is kept for the next search.
const EMBED_WAIT: Duration = Duration::from_secs(10);

/// How long a query embedding may take before it is given up.
pub const EMBED_TIMEOUT: Duration = Duration::from_secs(60);

/// The most query embedding requests running at once.
const MAX_EMBEDDINGS: usize = 32;

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
    /// Added, scaled by how close a name is to a query name by edit distance.
    pub name_edit: f64,
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
            name_edit: 0.03,
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
    /// The worker version whose snapshots queries may ask for, if any. Hits'
    /// snapshot ids carry it.
    pub snapshots: Option<String>,
    /// Whether this replica leads the snapshot index.
    pub leader: Arc<AtomicBool>,
    /// Query vectors by text, and the texts being embedded: the most recently
    /// used [`QUERY_VECTORS`], or all of them while evaluating many weightings.
    pub cache: Arc<Mutex<LruCache<String, Slot>>>,
    /// How long a search waits for its query's vector ([`EMBED_WAIT`]).
    pub embed_wait: Duration,
    /// A permit for each query embedding request running.
    running: Arc<Semaphore>,
}

/// A query's vector, once the embedder has answered.
type Answer = Option<Result<Vec<f32>, String>>;

/// A cached query: its vector, or the request that will bring it.
pub enum Slot {
    Ready(Vec<f32>),
    Pending(watch::Receiver<Answer>),
}

/// What the dense lane has for a query.
enum Embedded {
    Vector(Vec<f32>),
    /// Still being embedded when the search stopped waiting.
    Pending,
    Unavailable(String),
}

/// One query embedding request. It keeps what it gets in the cache, whether
/// or not a search still waits for it.
struct Flight {
    cache: Arc<Mutex<LruCache<String, Slot>>>,
    texts: Vec<String>,
    senders: Vec<watch::Sender<Answer>>,
    _permit: OwnedSemaphorePermit,
}

impl Flight {
    /// Runs on a blocking thread, which is also where the embedder's blocking
    /// client may be dropped.
    fn run(self, embedder: &Embedder) {
        let inputs: Vec<String> = self
            .texts
            .iter()
            .map(|t| format!("{QUERY_INSTRUCTION}{t}"))
            .collect();
        let embedded = embedder
            .embed_queries(&inputs, EMBED_TIMEOUT)
            .map_err(|e| format!("{e:#}"));
        if let Err(e) = &embedded {
            tracing::warn!(queries = inputs.len(), "query embedding failed: {e}");
        }
        let mut cache = self.cache.lock().unwrap();
        for (i, (text, tx)) in self.texts.into_iter().zip(&self.senders).enumerate() {
            let answer = match &embedded {
                Ok(vectors) => {
                    cache.put(text, Slot::Ready(vectors[i].clone()));
                    Ok(vectors[i].clone())
                }
                Err(e) => {
                    // Not kept, so the next search asks again; a newer
                    // request's entry stays.
                    if let Some(Slot::Pending(rx)) = cache.peek(&text)
                        && rx.same_channel(&tx.subscribe())
                    {
                        cache.pop(&text);
                    }
                    Err(e.clone())
                }
            };
            tx.send_replace(Some(answer));
        }
    }
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

/// The payload a hit needs: everything but `/declarations`' lists.
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
            snapshots: None,
            leader: Arc::default(),
            cache: Arc::new(Mutex::new(LruCache::new(QUERY_VECTORS))),
            embed_wait: EMBED_WAIT,
            running: Arc::new(Semaphore::new(MAX_EMBEDDINGS)),
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
            _ if self.snapshots.is_none() => {
                return Err("this server doesn't search snapshots".into());
            }
            SnapshotScope::All => {}
            SnapshotScope::Labels(labels) if labels.is_empty() => {
                return Err("snapshots: no labels given; use \"all\" for every snapshot".into());
            }
            SnapshotScope::Labels(labels) => check_labels(labels)?,
        }
        let snapshot_library = SNAPSHOT_LIBRARY.to_string();
        let mut libraries: Vec<&String> = self.manifest.libraries.keys().collect();
        if self.snapshots.is_some() {
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
        let mut results: Vec<QueryResult> = vec![QueryResult::default(); queries.len()];
        let vectors = self.embed(queries, &parsed, lanes).await;
        let modules = self.scopes(queries).await?;
        let mut searches = Vec::new();
        let mut owners = Vec::new();
        for (i, q) in queries.iter().enumerate() {
            if q.limit == 0 {
                continue;
            }
            let unavailable = "dense lane unavailable, results from the other lanes only";
            let vector = match &vectors[i] {
                Some(Embedded::Vector(v)) => Some(v.as_slice()),
                Some(Embedded::Pending) => {
                    results[i].dense_pending = true;
                    results[i].warnings.push(format!(
                        "{unavailable}: the query is still being embedded after {} s; ask again shortly to include it",
                        self.embed_wait.as_secs_f64()
                    ));
                    None
                }
                Some(Embedded::Unavailable(e)) => {
                    results[i].warnings.push(format!("{unavailable}: {e}"));
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
        // Names ranked by trigrams only roughly: the closest by edit distance
        // gets a bonus, as a misspelled name usually means it.
        for (p, c) in parsed.iter().zip(&mut candidates) {
            let Some(name) = p.name.as_deref().filter(|_| lanes.name) else {
                continue;
            };
            let lower = name.to_lowercase();
            let last = lower.rsplit('.').next().unwrap_or(&lower).to_string();
            let len = lower.chars().count().max(1) as f64;
            for s in c.iter_mut() {
                let d = edit_distance(&lower, &last, &s.payload.name) as f64;
                s.score += self.weights.name_edit * (1.0 - d / len).max(0.0);
            }
        }
        let mut picked: Vec<Vec<Scored>> = Vec::new();
        for (q, mut c) in queries.iter().zip(candidates) {
            c.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.payload.name.cmp(&b.payload.name))
            });
            c.truncate(q.limit.min(MAX_LIMIT));
            picked.push(c);
        }
        let entries = self.hit_entries(queries, &picked).await?;
        for (i, hits) in picked.into_iter().enumerate() {
            results[i].hits = hits
                .into_iter()
                .enumerate()
                .map(|(n, s)| {
                    let snapshot_ids = match &s.payload.module_id {
                        Some(m) => entries.get(&(i, m.clone())).cloned().unwrap_or_default(),
                        None => Vec::new(),
                    };
                    hit(n + 1, s, snapshot_ids)
                })
                .collect();
        }
        Ok(results)
    }

    /// Each query's vector for the dense lane, or why there is none. A text
    /// that no request is embedding yet goes into one of this search's own,
    /// which carries on after the search stops waiting.
    async fn embed(
        &self,
        queries: &[Query],
        parsed: &[Parsed],
        lanes: Lanes,
    ) -> Vec<Option<Embedded>> {
        let mut out: Vec<Option<Embedded>> = (0..queries.len()).map(|_| None).collect();
        let Some(embedder) = &self.embedder else {
            return out;
        };
        if !lanes.dense {
            return out;
        }
        let deadline = tokio::time::Instant::now() + self.embed_wait;
        let mut waits = Vec::new();
        let mut flight: Option<Flight> = None;
        let mut busy = false;
        {
            let mut cache = self.cache.lock().unwrap();
            for i in 0..queries.len() {
                let text = &parsed[i].dense_text;
                if queries[i].limit == 0 || text.is_empty() {
                    continue;
                }
                match cache.get(text) {
                    Some(Slot::Ready(v)) => {
                        out[i] = Some(Embedded::Vector(v.clone()));
                        continue;
                    }
                    // A closed channel's request is gone without an answer.
                    Some(Slot::Pending(rx)) if rx.has_changed().is_ok() => {
                        waits.push((i, rx.clone()));
                        continue;
                    }
                    _ => {}
                }
                if flight.is_none() && !busy {
                    match self.running.clone().try_acquire_owned() {
                        Ok(permit) => {
                            flight = Some(Flight {
                                cache: self.cache.clone(),
                                texts: Vec::new(),
                                senders: Vec::new(),
                                _permit: permit,
                            })
                        }
                        Err(_) => {
                            tracing::warn!("{MAX_EMBEDDINGS} query embeddings are already running");
                            busy = true;
                        }
                    }
                }
                let Some(f) = &mut flight else {
                    out[i] = Some(Embedded::Unavailable(format!(
                        "{MAX_EMBEDDINGS} query embeddings are already running"
                    )));
                    continue;
                };
                // In the cache at once, so that the same text later in this
                // batch, or in another search, waits for this request.
                let (tx, rx) = watch::channel(None);
                cache.put(text.clone(), Slot::Pending(rx.clone()));
                f.texts.push(text.clone());
                f.senders.push(tx);
                waits.push((i, rx));
            }
        }
        if let Some(f) = flight {
            let embedder = embedder.clone();
            drop(tokio::task::spawn_blocking(move || f.run(&embedder)));
        }
        for (i, mut rx) in waits {
            let answer = tokio::time::timeout_at(deadline, rx.wait_for(Option::is_some))
                .await
                .map(|r| r.ok().and_then(|v| v.clone()));
            out[i] = Some(match answer {
                Ok(Some(Ok(v))) => Embedded::Vector(v),
                Ok(Some(Err(e))) => Embedded::Unavailable(e),
                Ok(None) => {
                    Embedded::Unavailable("the query embedding stopped without an answer".into())
                }
                Err(_) => Embedded::Pending,
            });
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
        let limit = (q.limit.min(MAX_LIMIT) * 3).max(30).min(w.depth);
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
            // A declaration is its module's and its name, as names can
            // repeat across snapshot modules.
            let mut at: HashMap<(Option<String>, String), usize> = HashMap::new();
            for s in std::mem::take(c) {
                let module = s.payload.module_id.clone();
                let (name, s) = match s.payload.deprecated_to.clone() {
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
                let key = (module, name);
                match at.get(&key) {
                    Some(&i) => {
                        out[i].score = out[i].score.max(s.score);
                        // The replacement shows, not the alias that counted towards it.
                        if out[i].payload.name != key.1 && s.payload.name == key.1 {
                            out[i].payload = s.payload;
                        }
                    }
                    None => {
                        at.insert(key, out.len());
                        out.push(s);
                    }
                }
            }
            // Fold to_additive pairs: keep the better-scored one, name the other.
            let by_name: HashMap<(Option<String>, String), usize> = out
                .iter()
                .enumerate()
                .map(|(i, s)| ((s.payload.module_id.clone(), s.payload.name.clone()), i))
                .collect();
            let mut dropped = vec![false; out.len()];
            for i in 0..out.len() {
                let Some(j) = out[i]
                    .payload
                    .additive_sibling
                    .as_ref()
                    .and_then(|sib| by_name.get(&(out[i].payload.module_id.clone(), sib.clone())))
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

    /// For each query's snapshot hits, the ids of the query's snapshots of
    /// their modules, newest first. Without a snapshot version there are none.
    async fn hit_entries(
        &self,
        queries: &[Query],
        picked: &[Vec<Scored>],
    ) -> anyhow::Result<HashMap<(usize, String), Vec<String>>> {
        let Some(version) = self.snapshots.as_deref() else {
            return Ok(HashMap::new());
        };
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
        let id = |entry: &str| {
            SnapshotId {
                version: version.to_string(),
                entry: entry.to_string(),
            }
            .to_string()
        };
        Ok(keys
            .into_iter()
            .zip(found)
            .map(|(k, points)| {
                let entries = points
                    .into_iter()
                    .filter_map(|p| Some(id(p.payload?.get("entry")?.as_str()?)))
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
                    (
                        edit_distance(&lower, &last, &d.name),
                        -d.pagerank,
                        d.name.clone(),
                        d,
                    )
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

    /// Declaration `name` with its description, dependencies and users: the
    /// one in snapshot module `module`, when this searches snapshots and the
    /// module has it, else the bundle's.
    pub async fn detail(
        &self,
        name: &str,
        module: Option<&str>,
    ) -> anyhow::Result<Option<DeclarationDetail>> {
        let mut searches = Vec::new();
        if let Some(m) = module.filter(|_| self.snapshots.is_some()) {
            let snapshot = vec![
                is("name", name),
                is("source", "snapshot"),
                is("has_entries", true),
                is("module", m),
            ];
            searches.push(json!({"filter": and(snapshot), "limit": 1, "with_payload": true}));
        }
        searches.push(json!({
            "filter": and(vec![is("name", name), is("source", "base")]),
            "limit": 1, "with_payload": true,
        }));
        let found = self.q.query_batch(&self.collection, searches).await?;
        let Some(s) = found.into_iter().flatten().next().and_then(scored) else {
            return Ok(None);
        };
        let d = s.payload;
        let stored = d.stored();
        Ok(Some(DeclarationDetail {
            name: d.name.clone(),
            kind: d.kind.clone(),
            library: d.library.clone(),
            module: d.module.clone(),
            signature: d.signature.clone(),
            type_full: d.type_full.clone(),
            docstring: d.docstring.clone(),
            description: d.informal.clone().map(Description::from),
            flags: flags(&stored, None),
            source_url: d.source_url.clone(),
            dependencies: d.deps,
            used_by: d.used_by,
        }))
    }
}

/// How far `name` is from a query name, lowercased whole (`lower`) and its
/// last component (`last`): the closer of the two, the last component
/// counting one more.
fn edit_distance(lower: &str, last: &str, name: &str) -> usize {
    let name = name.to_lowercase();
    let name_last = name.rsplit('.').next().unwrap_or(&name);
    levenshtein(lower, &name).min(levenshtein(last, name_last) + 1)
}

fn scored(p: ScoredPoint) -> Option<Scored> {
    let payload: PointPayload = serde_json::from_value(p.payload?).ok()?;
    Some(Scored {
        score: f64::from(p.score),
        payload,
        sibling: None,
    })
}

/// A query's `n`th hit, from its point; `snapshot_ids` are a snapshot
/// declaration's.
fn hit(n: usize, s: Scored, snapshot_ids: Vec<String>) -> Hit {
    let d = s.payload.stored();
    let flags = flags(&d, s.sibling);
    let summary = summary(&d);
    let snapshot = s.payload.module_id.is_some();
    Hit {
        name: d.name.clone(),
        kind: d.kind.clone(),
        library: d.library.clone(),
        module: d.module.clone(),
        signature: d.signature.clone(),
        title: d.informal.as_ref().map(|i| i.informal_name.clone()),
        text: render(n, &d, summary.as_deref(), &flags),
        summary,
        flags,
        score: s.score,
        source_url: d.source_url.clone(),
        snapshot_ids,
        status: s.payload.status,
        verified: snapshot.then_some(s.payload.verified),
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

/// Which lanes a search may use; all by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lanes {
    pub lexical: bool,
    pub name: bool,
    pub structural: bool,
    pub dense: bool,
}

impl Lanes {
    pub const ALL: Lanes = Lanes {
        lexical: true,
        name: true,
        structural: true,
        dense: true,
    };
}

/// A query split by what it contains.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub prose: Vec<String>,
    pub lean: Vec<String>,
    pub identifiers: Vec<String>,
    pub name: Option<String>,
    /// Text for the dense lane.
    pub dense_text: String,
}

/// Classifies free text: Lean if it has `⊢`, `?x` or `:=`; LaTeX if it has
/// backslash commands or `^{`/`_{`; a name if it is one identifier with a dot
/// or underscore; natural language otherwise.
pub fn classify(q: &str) -> &'static str {
    let t = q.trim();
    if t.contains('⊢')
        || t.contains(":=")
        || t.contains("|-")
        || t.split_whitespace().any(|w| w.starts_with('?'))
    {
        "lean"
    } else if t.contains('\\') || t.contains("^{") || t.contains("_{") {
        "latex"
    } else if !t.contains(char::is_whitespace) && (t.contains('.') || t.contains('_')) {
        "name"
    } else {
        "nl"
    }
}

pub fn parse(q: &Query) -> Parsed {
    // Blank fields count as absent.
    let field = |f: &Option<String>| {
        f.as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    let mut p = Parsed::default();
    let mut nl = field(&q.description);
    let mut latex = field(&q.latex);
    let mut lean: Vec<String> = field(&q.pattern)
        .into_iter()
        .chain(field(&q.goal))
        .collect();
    p.name = field(&q.name);
    if let Some(text) = &field(&q.text) {
        match classify(text) {
            "lean" => lean.push(text.clone()),
            "latex" => latex = Some(latex.map_or(text.clone(), |l| format!("{l} {text}"))),
            "name" => p.name = p.name.clone().or(Some(text.clone())),
            _ => nl = Some(nl.map_or(text.clone(), |n| format!("{n} {text}"))),
        }
    }
    let mut dense = Vec::new();
    if let Some(nl) = &nl {
        p.prose.extend(tokens(nl));
        dense.push(nl.clone());
    }
    if let Some(latex) = &latex {
        let unicode = latex_to_unicode(latex);
        // Single-letter variables (`s`, `a`, `x`) match nearly everything.
        p.prose.extend(tokens(&unicode).into_iter().filter(|t| {
            let mut cs = t.chars();
            !matches!((cs.next(), cs.next()), (Some(c), None) if c.is_ascii_alphanumeric())
        }));
        dense.push(latex.clone());
    }
    for l in &lean {
        p.lean.extend(tokens(l));
        p.identifiers.extend(lean_identifiers(l));
        dense.push(l.clone());
    }
    if let Some(name) = &p.name {
        p.lean.extend(tokens(name));
        dense.push(name.clone());
    }
    p.dense_text = dense.join("\n");
    p
}

pub fn flags(d: &StoredDecl, sibling: Option<String>) -> Flags {
    Flags {
        uses_sorry: d.uses_sorry,
        informal: d.informal_result,
        deprecated_to: d.deprecated_to.clone(),
        additive_sibling: sibling.or_else(|| d.additive_sibling.clone()),
        imports: d.imports.clone(),
    }
}

/// The signature without its leading name, universe parameters or instance
/// arguments (`[Fintype n]`), shortened for display.
pub fn short_signature(d: &StoredDecl) -> String {
    let mut sig = d.signature.strip_prefix(&d.name).unwrap_or(&d.signature);
    // Universe parameters: `.{u_1, u_2}`.
    if let Some(rest) = sig.strip_prefix(".{")
        && let Some(end) = rest.find('}')
    {
        sig = &rest[end + 1..];
    }
    let sig = drop_instance_binders(sig.trim());
    let sig: String = sig.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&sig, 400)
}

/// Removes `[...]` binders, and `{inst✝ : ...}` ones, from the binders before
/// a signature's type; brackets inside the type are left alone.
fn drop_instance_binders(sig: &str) -> String {
    let mut out = String::new();
    let mut rest = sig;
    loop {
        let trimmed = rest.trim_start();
        let Some(open) = trimmed.chars().next().filter(|c| "([{⦃".contains(*c)) else {
            out.push_str(trimmed);
            return out;
        };
        let mut depth = 0usize;
        let mut end = None;
        for (i, c) in trimmed.char_indices() {
            match c {
                '(' | '[' | '{' | '⦃' => depth += 1,
                ')' | ']' | '}' | '⦄' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + c.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            out.push_str(trimmed);
            return out;
        };
        let group = &trimmed[..end];
        let instance = open == '[' || (open == '{' && group[1..].trim_start().starts_with("inst"));
        if !instance {
            out.push_str(group);
            out.push(' ');
        }
        rest = &trimmed[end..];
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The first sentence of the description, or of the docstring.
pub fn summary(d: &StoredDecl) -> Option<String> {
    let text = d
        .informal
        .as_ref()
        .map(|i| i.statement_nl.as_str())
        .or(d.docstring.as_deref())?;
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = match text.find(". ") {
        Some(i) => &text[..=i],
        None => &text,
    };
    (!first.is_empty()).then(|| truncate(first, 240))
}

/// About 150 tokens for a model: name, kind and library; signature; a summary;
/// a line of flags when there are any.
pub fn render(n: usize, d: &StoredDecl, summary: Option<&str>, flags: &Flags) -> String {
    let mut out = format!(
        "{n}. {}   {} · {}\n   {}",
        d.name,
        d.kind,
        d.library,
        short_signature(d)
    );
    if let Some(s) = summary {
        out.push_str("\n   ");
        out.push_str(s);
    }
    let mut notes = Vec::new();
    if flags.uses_sorry {
        notes.push("depends on sorry: not usable as a proof step".to_string());
    }
    if flags.informal {
        notes.push("informal result: stated, not proved".to_string());
    }
    if let Some(t) = &flags.deprecated_to {
        notes.push(format!("deprecated: use {t}"));
    }
    if let Some(s) = &flags.additive_sibling {
        notes.push(format!("additive/multiplicative counterpart: {s}"));
    }
    if let Some(h) = &flags.imports {
        notes.push(format!("load with `{h}` instead of the default imports"));
    }
    if !notes.is_empty() {
        out.push_str("\n   ⚠ ");
        out.push_str(&notes.join("; "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_drop_instance_binders() {
        assert_eq!(
            drop_instance_binders(
                "{n : Type u_3} [Fintype n] [DecidableEq n] {inst✝ : Ring R} (A : Matrix n n R) : (A * 1).trace = A.trace"
            ),
            "{n : Type u_3} (A : Matrix n n R) : (A * 1).trace = A.trace"
        );
        assert_eq!(
            drop_instance_binders(": [a, b].length = 2"),
            ": [a, b].length = 2"
        );
        assert_eq!(
            drop_instance_binders("[(c : C) → AddCommGroup (V c)] (x : V c) : x = x"),
            "(x : V c) : x = x"
        );
    }

    #[test]
    fn classifies_free_text() {
        assert_eq!(classify("⊢ Matrix.trace A = 1"), "lean");
        assert_eq!(classify("Matrix.trace ?A = 1"), "lean");
        assert_eq!(classify(r"S(\rho_{AB}) \le S(\rho_A)"), "latex");
        assert_eq!(classify("Real.sqrt_mul_self"), "name");
        assert_eq!(classify("the trace of a density matrix is one"), "nl");
    }

    #[test]
    fn parse_routes_fields() {
        let p = parse(&Query {
            description: Some("trace is one".into()),
            pattern: Some("Matrix.trace ?A = 1".into()),
            ..Default::default()
        });
        assert_eq!(p.prose, ["trace", "is", "one"]);
        assert_eq!(p.identifiers, ["Matrix.trace"]);
        assert!(p.lean.contains(&"matrix.trace".to_string()));
    }
}
