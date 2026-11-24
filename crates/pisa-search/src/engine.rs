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
use pisa_db::description_flags::DescriptionFlags;
use pisa_protocol::api::SnapshotId;
use pisa_protocol::labels::check_labels;
use pisa_protocol::search::{
    DeclarationDetail, Description, Flags, Format, Hit, Query, QueryResult, Resolution,
    SnapshotScope, Suggestion,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

use crate::dense::{Embedder, QUERY_INSTRUCTION};
use crate::encode;
use crate::index::{BundleManifest, StoredDecl};
use crate::informal_store::description_hash;
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

/// How long a search waits for the readers' flag counts, which come from the
/// metadata store's shared pool; past it the hits carry no counts.
const FLAG_COUNTS_WAIT: Duration = Duration::from_secs(1);

/// `lookup`, or an error if it takes longer than `limit`.
async fn bounded<T>(
    limit: Duration,
    lookup: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    match tokio::time::timeout(limit, lookup).await {
        Ok(found) => found,
        Err(_) => anyhow::bail!("timed out after {} ms", limit.as_millis()),
    }
}

/// The library of snapshot declarations.
pub const SNAPSHOT_LIBRARY: &str = pisa_protocol::snapshot::ROOT;

/// Fusion and scoring parameters, tuned on the dev splits of the physics sets
/// with MathlibQR as a guard (see `search/eval/`).
///
/// Under Qdrant's RRF a weight below 1 makes a lane's ranks fall off fast:
/// lexical and structural matches lift their top few hits without crowding
/// out the dense lane further down, which retrieves best alone. A lane
/// weighted 0 isn't asked.
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
    /// The conclusion lane's weight: a goal's target or a pattern against
    /// theorems' conclusions.
    pub concl: f64,
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
            rrf_k: 20,
            lexical: 0.05,
            name: 3.0,
            structural: 0.02,
            structural_identifiers: 1.0,
            // Off: on the physics sets it lowered MRR, and it added nothing
            // even to premises that conclude like their goal; see
            // search/eval/v4.36.0-rc2-1e541805-physics.md.
            concl: 0.0,
            dense: 1.0,
            exact_lane: 1.0,
            prose_name: 1.0,
            prose_informal: 2.0,
            prose_statement: 1.5,
            prose_signature: 1.0,
            prose_doc: 1.2,
            lean_signature: 2.0,
            lean_name: 1.5,
            pagerank: 0.016,
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
    /// Readers' flags on descriptions, which hits count, if kept.
    pub description_flags: Option<DescriptionFlags>,
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
            description_flags: None,
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
        let queries: Vec<Query> = (queries.iter())
            .map(|q| Query {
                snapshots: in_request_scope(&q.snapshots),
                ..q.clone()
            })
            .collect();
        let queries = queries.as_slice();
        let mut parsed: Vec<Parsed> = queries.iter().map(parse).collect();
        let known = self.known_names(&parsed).await?;
        for p in &mut parsed {
            p.resolve(&known);
        }
        let mut results: Vec<QueryResult> = vec![QueryResult::default(); queries.len()];
        let vectors = self.embed(queries, &parsed, lanes).await;
        let modules = self.scopes(queries).await?;
        let mut searches = Vec::new();
        let mut owners = Vec::new();
        for (i, q) in queries.iter().enumerate() {
            if q.limit == 0 {
                continue;
            }
            let words: Vec<String> = parsed[i]
                .prose
                .iter()
                .chain(&parsed[i].lean)
                .cloned()
                .collect();
            results[i].hints = self.symbols.hints(&words);
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
        let keys: Vec<(String, String)> = picked.iter().flatten().filter_map(flag_key).collect();
        let flagged = match self.flag_counts(&keys).await {
            Ok(counts) => counts,
            Err(e) => {
                for r in &mut results {
                    r.warnings
                        .push(format!("description flags unavailable: {e}"));
                }
                HashMap::new()
            }
        };
        for (i, hits) in picked.into_iter().enumerate() {
            results[i].hits = hits
                .into_iter()
                .enumerate()
                .map(|(n, s)| {
                    let snapshot_ids = match &s.payload.module_id {
                        Some(m) => entries.get(&(i, m.clone())).cloned().unwrap_or_default(),
                        None => Vec::new(),
                    };
                    let flags = flag_key(&s).and_then(|k| flagged.get(&k)).copied();
                    hit(n + 1, s, snapshot_ids, flags.unwrap_or(0))
                })
                .collect();
            if queries[i].format == Format::Text {
                let r = &mut results[i];
                let texts: Vec<String> = r.hits.drain(..).map(|h| h.text).collect();
                r.text = Some(texts.join("\n\n"));
            }
        }
        Ok(results)
    }

    /// The readers' flags on each of `keys`' descriptions, in one request;
    /// none if flags aren't kept.
    async fn flag_counts(
        &self,
        keys: &[(String, String)],
    ) -> anyhow::Result<HashMap<(String, String), u32>> {
        match &self.description_flags {
            Some(f) if !keys.is_empty() => {
                bounded(FLAG_COUNTS_WAIT, async { Ok(f.counts(keys).await?) }).await
            }
            _ => Ok(HashMap::new()),
        }
    }

    /// Which of the queries' identifiers name a declaration of the index or
    /// of a snapshot, in one request.
    async fn known_names(&self, parsed: &[Parsed]) -> Result<HashSet<String>, SearchError> {
        let mut names: Vec<&String> = (parsed.iter())
            .flat_map(|p| p.identifiers.iter().chain(&p.concl_identifiers))
            .collect();
        names.sort();
        names.dedup();
        if names.is_empty() {
            return Ok(HashSet::new());
        }
        let searches = (names.iter())
            .map(|n| json!({"filter": and(vec![is("name", n.as_str())]), "limit": 1}))
            .collect();
        let found = self.q.query_batch(&self.collection, searches).await?;
        Ok(names
            .into_iter()
            .zip(found)
            .filter(|(_, f)| !f.is_empty())
            .map(|(n, _)| n.clone())
            .collect())
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

    /// The modules of each label scope's heads, from the entries collection.
    async fn scopes(&self, queries: &[Query]) -> Result<Vec<Option<Vec<String>>>, SearchError> {
        let lookups = queries.iter().map(|q| async move {
            let SnapshotScope::Labels(labels) = &q.snapshots else {
                return Ok(None);
            };
            let mut must = vec![is("head", true)];
            must.extend(labels.iter().map(|(k, v)| is("labels", format!("{k}={v}"))));
            let filter = and(must);
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
                and(vec![is("source", "snapshot"), is("has_heads", true)]),
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
        let visible = json!({"must": [scope.clone()], "must_not": hidden(q)});
        let mut prefetch = Vec::new();
        let mut weights = Vec::new();
        let mut lane = |query: Value, using: &str, weight: f64| {
            if weight <= 0.0 {
                return;
            }
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
        if lanes.structural
            && let Some((consts, weight)) = structural(p, &self.symbols, w)
        {
            lane(json!(encode::consts_query(&consts)), "consts", weight);
        }
        if lanes.concl {
            let consts = conclusion(p, &self.symbols);
            if !consts.is_empty() {
                lane(json!(encode::consts_query(&consts)), "concl", w.concl);
            }
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

    /// For each query's snapshot hits, the ids of the query's heads on their
    /// modules, newest first. Without a snapshot version there are none.
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
                let mut must = vec![is("module_id", m.as_str()), is("head", true)];
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
    /// prominence. Names are the bundle's, and also those of the snapshot
    /// modules `snapshots` reaches, within the request's scope, as a
    /// search's are.
    pub async fn resolve(
        &self,
        names: &[String],
        snapshots: &SnapshotScope,
    ) -> Result<Vec<Resolution>, SearchError> {
        let q = Query {
            snapshots: in_request_scope(snapshots),
            ..Default::default()
        };
        let modules = self.scopes(std::slice::from_ref(&q)).await?;
        let base = self.filter(&q, modules[0].as_deref());
        let fields = json!([
            "name",
            "kind",
            "signature",
            "pagerank",
            "deprecated_to",
            "imports"
        ]);
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
                    imports: exact.payload.imports,
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
                imports: None,
                suggestions: ranked
                    .into_iter()
                    .take(5)
                    .map(|(_, _, _, d)| {
                        let s = d.stored();
                        Suggestion {
                            name: d.name,
                            kind: d.kind,
                            signature: short_signature(&s),
                            imports: d.imports,
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
        // Within a scope, only a module with a head in it.
        let scope = pisa_db::scope::current();
        let module = match module.filter(|_| self.snapshots.is_some()) {
            Some(m) if !scope.is_empty() => {
                let q = Query {
                    snapshots: SnapshotScope::Labels(scope.labels().clone()),
                    ..Default::default()
                };
                let modules = self.scopes(std::slice::from_ref(&q)).await?;
                let wanted = snapshot_module_id(m);
                (modules.into_iter().flatten().flatten())
                    .any(|id| Some(&id) == wanted.as_ref())
                    .then_some(m)
            }
            m => m,
        };
        if let Some(m) = module {
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
        let flagged = match flag_key(&s) {
            Some(key) => match self.flag_counts(std::slice::from_ref(&key)).await {
                Ok(counts) => counts.get(&key).copied().unwrap_or(0),
                Err(e) => {
                    tracing::warn!("description flags unavailable: {e}");
                    0
                }
            },
            None => 0,
        };
        let d = s.payload;
        let stored = d.stored();
        let mut flags = flags(&stored, None);
        flags.description_flags = flagged;
        Ok(Some(DeclarationDetail {
            name: d.name.clone(),
            kind: d.kind.clone(),
            library: d.library.clone(),
            module: d.module.clone(),
            signature: d.signature.clone(),
            type_full: d.type_full.clone(),
            value: d.value.clone(),
            docstring: d.docstring.clone(),
            description: d.informal.clone().map(Description::from),
            flags,
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

/// A point's name and the hash of its description, which readers' flags are
/// on, if it has one.
fn flag_key(s: &Scored) -> Option<(String, String)> {
    let i = s.payload.informal.as_ref()?;
    let hash = description_hash(
        &i.informal_name,
        &i.statement_nl,
        i.statement_latex.as_deref(),
    );
    Some((s.payload.name.clone(), hash))
}

/// A query's `n`th hit, from its point; `snapshot_ids` are a snapshot
/// declaration's, and `description_flags` the readers' flags on its
/// description.
fn hit(n: usize, s: Scored, snapshot_ids: Vec<String>, description_flags: u32) -> Hit {
    let d = s.payload.stored();
    let mut flags = flags(&d, s.sibling);
    flags.description_flags = description_flags;
    let summary = summary(&d);
    let description_source = match &d.informal {
        Some(i) => Some(i.source.clone()),
        None => summary.as_ref().map(|_| "docstring".to_string()),
    };
    let snapshot = s.payload.module_id.is_some();
    Hit {
        name: d.name.clone(),
        kind: d.kind.clone(),
        library: d.library.clone(),
        module: d.module.clone(),
        signature: d.signature.clone(),
        value: d.value.clone(),
        title: d.informal.as_ref().map(|i| i.informal_name.clone()),
        text: render(n, &d, summary.as_deref(), &flags),
        summary,
        description_source,
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
        concl: false,
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
        configs.push((
            "concl",
            Lanes {
                concl: true,
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
    pub concl: bool,
    pub dense: bool,
}

impl Lanes {
    pub const ALL: Lanes = Lanes {
        lexical: true,
        name: true,
        structural: true,
        concl: true,
        dense: true,
    };
}

/// A query split by what it contains.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub prose: Vec<String>,
    pub lean: Vec<String>,
    pub identifiers: Vec<String>,
    /// The identifiers and tokens of a goal's target or a pattern: what a
    /// conclusion should look like.
    pub concl_identifiers: Vec<String>,
    pub concl_tokens: Vec<String>,
    /// Constants the query names exactly.
    pub consts: Vec<String>,
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
    let mut lean: Vec<String> = Vec::new();
    // What a conclusion should look like: a goal's target, or a pattern.
    let mut concl: Vec<String> = Vec::new();
    let mut lean_text = |text: String, lean: &mut Vec<String>| {
        let text = text.replace("|-", "⊢");
        let target = text.rsplit_once('⊢').map_or(text.as_str(), |(_, t)| t);
        concl.push(target.to_string());
        lean.push(text);
    };
    for text in field(&q.pattern).into_iter().chain(field(&q.goal)) {
        lean_text(text, &mut lean);
    }
    p.name = field(&q.name);
    p.consts = (q.consts.iter())
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    if let Some(text) = &field(&q.text) {
        match classify(text) {
            "lean" => lean_text(text.clone(), &mut lean),
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
    for c in &concl {
        p.concl_tokens.extend(tokens(c));
        p.concl_identifiers.extend(lean_identifiers(c));
    }
    for ids in [&mut p.identifiers, &mut p.concl_identifiers] {
        let mut seen = HashSet::new();
        ids.retain(|i| seen.insert(i.clone()));
    }
    if let Some(name) = &p.name {
        p.lean.extend(tokens(name));
        dense.push(name.clone());
    }
    p.dense_text = dense.join("\n");
    p
}

impl Parsed {
    /// Keeps only the identifiers that name a declaration in `known`: a
    /// hypothesis name, `fun` or a variable names nothing.
    pub fn resolve(&mut self, known: &HashSet<String>) {
        self.identifiers.retain(|i| known.contains(i));
        self.concl_identifiers.retain(|i| known.contains(i));
    }
}

/// Weighted constants, each once at its highest weight.
fn merge(consts: impl IntoIterator<Item = (String, f32)>) -> Vec<(String, f32)> {
    let mut out: Vec<(String, f32)> = Vec::new();
    for (c, w) in consts {
        match out.iter_mut().find(|(name, _)| *name == c) {
            Some(existing) => existing.1 = existing.1.max(w),
            None => out.push((c, w)),
        }
    }
    out
}

/// The structural lane's constants and weight, or `None` without any: the
/// query's names, the constants it names, and those its symbols suggest.
/// Symbols alone are weaker evidence than names, so they weigh less.
pub fn structural(p: &Parsed, symbols: &Symbols, w: &Weights) -> Option<(Vec<(String, f32)>, f64)> {
    let all_tokens: Vec<String> = p.prose.iter().chain(&p.lean).cloned().collect();
    let named = p
        .identifiers
        .iter()
        .chain(&p.consts)
        .map(|i| (i.clone(), 1.0));
    let consts = merge(named.chain(symbols.constants(&all_tokens)));
    if consts.is_empty() {
        return None;
    }
    let named = !p.identifiers.is_empty() || !p.consts.is_empty();
    let weight = if named {
        w.structural_identifiers
    } else {
        w.structural
    };
    Some((consts, weight))
}

/// The conclusion lane's constants: those of a goal's target or a pattern,
/// named or suggested by its symbols, and those the query names.
pub fn conclusion(p: &Parsed, symbols: &Symbols) -> Vec<(String, f32)> {
    let named = p
        .concl_identifiers
        .iter()
        .chain(&p.consts)
        .map(|i| (i.clone(), 1.0));
    merge(named.chain(symbols.constants(&p.concl_tokens)))
}

pub fn flags(d: &StoredDecl, sibling: Option<String>) -> Flags {
    Flags {
        uses_sorry: d.uses_sorry,
        informal: d.informal_result,
        deprecated_to: d.deprecated_to.clone(),
        additive_sibling: sibling.or_else(|| d.additive_sibling.clone()),
        imports: d.imports.clone(),
        description_flags: 0,
    }
}

/// The signature without its leading name, universe parameters or instance
/// arguments (`[Fintype n]`), shortened for display.
pub fn short_signature(d: &StoredDecl) -> String {
    let sig = after_name(&d.name, &d.signature);
    let sig: String = sig.split_whitespace().collect::<Vec<_>>().join(" ");
    let (binders, rest) = binders(&sig);
    let binders: Vec<&str> = binders.into_iter().filter(|b| !is_instance(b)).collect();
    let whole = binders.iter().chain([&rest]).filter(|p| !p.is_empty());
    let whole = whole.copied().collect::<Vec<_>>().join(" ");
    if whole.chars().count() <= MAX_SIGNATURE {
        return whole;
    }
    // Binders go from the middle, so that the conclusion stays: `… : type`.
    let Some(room) = MAX_SIGNATURE.checked_sub(rest.chars().count() + 2) else {
        return format!("… {}", truncate(rest, MAX_SIGNATURE - 2));
    };
    let mut out = String::new();
    for b in binders {
        if out.chars().count() + b.chars().count() + 1 > room {
            break;
        }
        out.push_str(b);
        out.push(' ');
    }
    out.push_str("… ");
    out.push_str(rest);
    out
}

/// The longest signature a hit's text shows, in characters.
const MAX_SIGNATURE: usize = 400;

/// `signature` past `name` and its universe parameters (`.{u_1, u_2}`).
pub(crate) fn after_name<'s>(name: &str, signature: &'s str) -> &'s str {
    let sig = signature.strip_prefix(name).unwrap_or(signature);
    match sig.strip_prefix(".{").and_then(|rest| rest.split_once('}')) {
        Some((_, rest)) => rest,
        None => sig,
    }
}

/// `[...]` binders, and `{inst✝ : ...}` ones.
pub(crate) fn is_instance(binder: &str) -> bool {
    binder.starts_with('[')
        || (binder.starts_with('{') && binder[1..].trim_start().starts_with("inst"))
}

/// A signature's binder groups, and the rest from its type's `:`.
pub(crate) fn binders(sig: &str) -> (Vec<&str>, &str) {
    let mut out = Vec::new();
    let mut rest = sig;
    loop {
        let trimmed = rest.trim_start();
        if !trimmed.starts_with(['(', '[', '{', '⦃']) {
            return (out, trimmed);
        }
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
            return (out, trimmed);
        };
        out.push(&trimmed[..end]);
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

/// The first sentences of the description, or of the docstring: two at
/// most within 240 characters, or the first cut at 240.
pub fn summary(d: &StoredDecl) -> Option<String> {
    const MAX: usize = 240;
    let text = d
        .informal
        .as_ref()
        .map(|i| i.statement_nl.as_str())
        .or(d.docstring.as_deref())?;
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let ends = sentence_ends(&text);
    let first = ends.first().map_or(text.len(), |&e| e);
    if text[..first].chars().count() > MAX {
        return Some(truncate(&text[..first], MAX));
    }
    let end = match ends.get(1) {
        Some(&second) if text[..second].chars().count() <= MAX => second,
        _ => first,
    };
    let s = text[..end].trim_end();
    (!s.is_empty()).then(|| s.to_string())
}

/// Where each sentence of `text` ends (after its `.`, `!` or `?`): before a
/// space and a word that doesn't start in lower case, and not after an
/// abbreviation or an initial. A decimal point has no space after it.
fn sentence_ends(text: &str) -> Vec<usize> {
    const ABBREVIATIONS: [&str; 7] = ["i.e.", "e.g.", "resp.", "cf.", "vs.", "etc.", "approx."];
    let mut ends = Vec::new();
    for (i, c) in text.char_indices() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let end = i + 1;
        let next = text[end..].strip_prefix(' ').and_then(|r| r.chars().next());
        if end < text.len() && next.is_none_or(char::is_lowercase) {
            continue;
        }
        if c == '.' {
            let word = text[..end].rsplit(' ').next().unwrap_or("");
            let word = word.trim_start_matches(|c: char| !c.is_alphanumeric());
            let mut letters = word.chars();
            let initial = matches!((letters.next(), letters.next()), (Some(l), Some('.')) if l.is_uppercase())
                && letters.next().is_none();
            if initial || ABBREVIATIONS.iter().any(|a| word.eq_ignore_ascii_case(a)) {
                continue;
            }
        }
        ends.push(end);
    }
    ends
}

/// About 150 tokens for a model: name, kind and library; signature; the start
/// of a definition's value; a summary; a line of flags when there are any.
pub fn render(n: usize, d: &StoredDecl, summary: Option<&str>, flags: &Flags) -> String {
    let mut out = format!(
        "{n}. {}   {} · {}\n   {}",
        d.name,
        d.kind,
        d.library,
        short_signature(d)
    );
    if let Some(v) = &d.value {
        let v: String = v.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str("\n   := ");
        out.push_str(&truncate(&v, 200));
    }
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
    match flags.description_flags {
        0 => {}
        1 => notes.push("description flagged once: check it against the signature".into()),
        n => notes.push(format!(
            "description flagged {n} times: check it against the signature"
        )),
    }
    if !notes.is_empty() {
        out.push_str("\n   ⚠ ");
        out.push_str(&notes.join("; "));
    }
    out
}

/// `snapshots` within the request's scope (see `pisa_db::scope`): every
/// snapshot becomes those with the scope's labels, labels are ANDed with
/// them, and labels that contradict them reach no snapshot.
fn in_request_scope(snapshots: &SnapshotScope) -> SnapshotScope {
    let scope = pisa_db::scope::current();
    if scope.is_empty() {
        return snapshots.clone();
    }
    match snapshots {
        SnapshotScope::None => SnapshotScope::None,
        SnapshotScope::All => SnapshotScope::Labels(scope.labels().clone()),
        SnapshotScope::Labels(labels) => {
            let mut labels = labels.clone();
            match scope.merge_into(&mut labels) {
                Ok(()) => SnapshotScope::Labels(labels),
                Err(_) => SnapshotScope::None,
            }
        }
    }
}

/// The id in a snapshot module's name, `PisaSnap.s<id>`.
fn snapshot_module_id(module: &str) -> Option<String> {
    pisa_protocol::snapshot::parse_module(module).ok().flatten()
}

/// What a query's hits leave out: hidden points, and those that use `sorry`
/// unless the query includes them, which by default it does only with
/// snapshots in scope.
fn hidden(q: &Query) -> Vec<Value> {
    let mut hidden = vec![is("hidden", true)];
    if !q.include_sorried.unwrap_or(!q.snapshots.is_none()) {
        hidden.push(is("uses_sorry", true));
    }
    hidden
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_slow_count_lookup_is_an_error_not_a_hang() {
        let slow = bounded(Duration::from_millis(20), async {
            std::future::pending::<()>().await;
            Ok(())
        });
        let e = slow.await.unwrap_err();
        assert!(e.to_string().contains("timed out"), "{e}");
        let fast = bounded(Duration::from_millis(20), async { Ok(1) });
        assert_eq!(fast.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_query_keeps_to_the_request_scope() {
        use pisa_db::scope::{Scope, within};
        let labels = |pairs: &[(&str, &str)]| {
            (pairs.iter())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let all = SnapshotScope::All;
        let run = SnapshotScope::Labels(labels(&[("run", "1")]));
        assert_eq!(in_request_scope(&all), all);
        within(Scope::parse("user=a").unwrap(), async {
            assert_eq!(
                in_request_scope(&all),
                SnapshotScope::Labels(labels(&[("user", "a")]))
            );
            assert_eq!(
                in_request_scope(&run),
                SnapshotScope::Labels(labels(&[("run", "1"), ("user", "a")]))
            );
            let other = SnapshotScope::Labels(labels(&[("user", "b")]));
            assert_eq!(in_request_scope(&other), SnapshotScope::None);
            assert_eq!(in_request_scope(&SnapshotScope::None), SnapshotScope::None);
        })
        .await;
    }

    #[test]
    fn sorried_declarations_show_when_searching_snapshots() {
        let sorried_hidden = |q: Query| hidden(&q).contains(&is("uses_sorry", true));
        assert!(sorried_hidden(Query::default()));
        let in_snapshots = Query {
            snapshots: SnapshotScope::All,
            ..Default::default()
        };
        assert!(!sorried_hidden(in_snapshots.clone()));
        assert!(sorried_hidden(Query {
            include_sorried: Some(false),
            ..in_snapshots
        }));
        assert!(!sorried_hidden(Query {
            include_sorried: Some(true),
            ..Default::default()
        }));
    }

    fn documented(doc: &str) -> StoredDecl {
        StoredDecl {
            name: "f".into(),
            kind: "def".into(),
            signature: "f : ℕ".into(),
            docstring: Some(doc.into()),
            ..Default::default()
        }
    }

    /// A summary is whole sentences, two at most within 240 characters: an
    /// abbreviation, an initial or a decimal point doesn't end one.
    #[test]
    fn a_summary_keeps_whole_sentences() {
        let s = |doc: &str| summary(&documented(doc)).unwrap();
        assert_eq!(
            s("The trace, i.e. the sum of the diagonal. It is linear. It is cyclic."),
            "The trace, i.e. the sum of the diagonal. It is linear."
        );
        assert_eq!(
            s("Due to J. von Neumann, e.g. in his book. See also\n  Wigner. And more."),
            "Due to J. von Neumann, e.g. in his book. See also Wigner."
        );
        assert_eq!(
            s("About 3.14 is close to π! Is it rational? No."),
            "About 3.14 is close to π! Is it rational?"
        );
        // A second sentence that doesn't fit is left out.
        let first = format!("{}.", "a".repeat(200));
        assert_eq!(s(&format!("{first} B{}.", "b".repeat(50))), first);
        // A first sentence too long on its own is cut.
        let long = s(&format!("{}. Short.", "c".repeat(300)));
        assert_eq!(long.chars().count(), 241);
        assert!(long.ends_with('…'));
        assert_eq!(s("No full stop"), "No full stop");
    }

    /// Past 400 characters, binders go from the middle, so that the
    /// conclusion stays; one too long on its own is cut.
    #[test]
    fn a_long_signature_keeps_its_conclusion() {
        let binders: String = (0..40).map(|i| format!("(h{i} : a{i} ≤ b{i}) ")).collect();
        let d = StoredDecl {
            name: "f".into(),
            signature: format!("f {binders}[Fintype n] : a0 + a1 = b0 + b1"),
            ..Default::default()
        };
        let s = short_signature(&d);
        assert!(s.chars().count() <= 400, "{s}");
        assert!(s.starts_with("(h0 : a0 ≤ b0) (h1 : a1 ≤ b1)"), "{s}");
        assert!(s.ends_with(") … : a0 + a1 = b0 + b1"), "{s}");
        let d = StoredDecl {
            name: "f".into(),
            signature: format!("f (x : ℕ) : {}x = x", "x + ".repeat(200)),
            ..Default::default()
        };
        let s = short_signature(&d);
        assert!(s.starts_with("… : x + x"), "{s}");
        assert!(s.chars().count() <= 401 && s.ends_with('…'), "{s}");
        // A short one is whole.
        let d = StoredDecl {
            name: "f".into(),
            signature: "f (x : ℕ) [Fintype n] : x = x".into(),
            ..Default::default()
        };
        assert_eq!(short_signature(&d), "(x : ℕ) : x = x");
    }

    /// A definition's rendering shows the start of its value.
    #[test]
    fn a_rendered_definition_shows_its_value() {
        let d = StoredDecl {
            value: Some("m * n\n  / m.gcd n".into()),
            ..documented("The least common multiple.")
        };
        let text = render(1, &d, summary(&d).as_deref(), &Flags::default());
        assert_eq!(
            text,
            "1. f   def · \n   : ℕ\n   := m * n / m.gcd n\n   The least common multiple."
        );
        let long = StoredDecl {
            value: Some("x".repeat(500)),
            ..d
        };
        let text = render(1, &long, None, &Flags::default());
        let value = text.lines().last().unwrap();
        assert_eq!(value.chars().count(), "   := ".len() + 201);
    }

    #[test]
    fn signatures_drop_instance_binders() {
        let short = |sig: &str| {
            short_signature(&StoredDecl {
                name: "f".into(),
                signature: format!("f {sig}"),
                ..Default::default()
            })
        };
        assert_eq!(
            short(
                "{n : Type u_3} [Fintype n] [DecidableEq n] {inst✝ : Ring R} (A : Matrix n n R) : (A * 1).trace = A.trace"
            ),
            "{n : Type u_3} (A : Matrix n n R) : (A * 1).trace = A.trace"
        );
        // Brackets in the type are left alone.
        assert_eq!(short(": [a, b].length = 2"), ": [a, b].length = 2");
        assert_eq!(
            short("[(c : C) → AddCommGroup (V c)] (x : V c) : x = x"),
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
        // A pattern is what a conclusion looks like.
        assert_eq!(p.concl_identifiers, ["Matrix.trace"]);
    }

    /// A goal's hypotheses feed the structural lane, its target the
    /// conclusion lane too.
    #[test]
    fn a_goal_splits_at_its_turnstile() {
        let p = parse(&Query {
            goal: Some("x : ℝ\nh : Real.sqrt x = 0\n⊢ Matrix.trace 1 = Fintype.card n".into()),
            ..Default::default()
        });
        assert_eq!(p.identifiers, ["Real.sqrt", "Matrix.trace", "Fintype.card"]);
        assert_eq!(p.concl_identifiers, ["Matrix.trace", "Fintype.card"]);
        assert!(p.concl_tokens.contains(&"=".to_string()));
        assert!(!p.concl_tokens.contains(&"real.sqrt".to_string()));
        // Lean text in `text` with a turnstile is a goal.
        let t = parse(&Query {
            text: Some("h : 0 ≤ a ⊢ Real.sqrt a ^ 2 = a".into()),
            ..Default::default()
        });
        assert_eq!(t.concl_identifiers, ["Real.sqrt"]);
    }

    #[test]
    fn consts_are_taken_as_given() {
        let p = parse(&Query {
            consts: vec!["Matrix.trace".into(), " ".into()],
            ..Default::default()
        });
        assert_eq!(p.consts, ["Matrix.trace"]);
    }

    /// Only identifiers that name something count: an unknown hypothesis
    /// name, `fun` or `ℝ` neither go to the structural lane nor raise its
    /// weight.
    #[test]
    fn identifiers_count_only_when_they_name_something() {
        let w = Weights::default();
        let symbols = Symbols::default();
        let mut p = parse(&Query {
            goal: Some("hx : fun y => y\n⊢ Matrix.trace A = ℝ".into()),
            ..Default::default()
        });
        assert!(p.identifiers.contains(&"hx".to_string()));
        let known: HashSet<String> = ["Matrix.trace".to_string()].into();
        p.resolve(&known);
        assert_eq!(p.identifiers, ["Matrix.trace"]);
        assert_eq!(p.concl_identifiers, ["Matrix.trace"]);
        let (consts, weight) = structural(&p, &symbols, &w).unwrap();
        assert_eq!(consts, [("Matrix.trace".to_string(), 1.0)]);
        assert_eq!(weight, w.structural_identifiers);
        p.resolve(&HashSet::new());
        assert!(structural(&p, &symbols, &w).is_none());
        // Constants a caller names count as names.
        p.consts = vec!["Fintype.card".into()];
        assert_eq!(
            structural(&p, &symbols, &w).unwrap().1,
            w.structural_identifiers
        );
        assert_eq!(
            conclusion(&p, &symbols),
            [("Fintype.card".to_string(), 1.0)]
        );
    }
}
