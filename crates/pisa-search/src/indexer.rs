//! Snapshot declarations in Qdrant. One search replica at a time, the
//! leader, holds a lease in the workers' database and runs the indexer:
//! - it mirrors the snapshot catalog into Qdrant: a point per declaration
//!   of each module, and a point per entry;
//! - it describes (with an LLM, when one is configured) and embeds those
//!   declarations, keeping descriptions and vectors in the catalog
//!   (`described` records), so that a new leader pays for nothing again.
//!
//! A new leader reconciles Qdrant with the catalog, then follows its
//! change log. It reconciles again every few hours, which also repairs what
//! a deposed leader may have written late.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use futures::StreamExt;
use pisa_db::catalog::{Catalog, Change, SnapshotDecl, SnapshotDecls};
use pisa_db::db::Db;
use pisa_protocol::search::Informal;
use pisa_protocol::snapshot;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, mpsc};

use crate::corpus::{Corpus, Decl, Manifest};
use crate::dense::{DIMS, Embedder, document_view, quantize, text_hash};
use crate::engine::{Engine, SNAPSHOT_LIBRARY};
use crate::index::StoredDecl;
use crate::informalize::{self, Config, Context, Prompts, levels, needs_description};
use crate::llm::{self, Llm};
use crate::points::{self, PointPayload};
use crate::qdrant::{any, is};

/// The lease the leader holds, and for how long it takes it. Every replica
/// tries to take or renew it every third of that, and a leader stops
/// writing when less than a third of it is left.
const LEASE: &str = "search-leader";
pub const LEASE_TTL: Duration = Duration::from_secs(30);

/// How far back each read of the change log starts again: a transaction can
/// commit after a later one, and workers' clocks differ.
const LOOKBACK: Duration = Duration::from_secs(120);
const POLL_EVERY: Duration = Duration::from_secs(2);
const RECONCILE_EVERY: Duration = Duration::from_secs(6 * 3600);

/// Rows read per page of the catalog.
const PAGE: u32 = 1000;

/// Attempts at describing a declaration; after these it keeps its docstring view.
const MAX_ATTEMPTS: u32 = 3;
/// The first wait before describing a module again after a failure; it
/// doubles with each attempt. Embedding waits this long after a failure.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// How long describing stops after the LLM refuses the key.
const AUTH_PAUSE: Duration = Duration::from_secs(300);
/// Document views embedded per request.
const EMBED_BATCH: usize = 32;
/// Points written per upsert.
const UPSERT_BATCH: usize = 256;

/// The search service's record for one declaration of a snapshot module.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Described {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub informal: Option<Informal>,
    /// Failed attempts at describing it, and why the last one failed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The hash ([`text_hash`], in hex) of the document view `vector` embeds,
    /// with the bundle's embedding model, so a stale vector is redone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    /// The bundle's embedding model `vector` is from. A vector of an older
    /// view is still indexed until it is redone; one of another model isn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Quantized like the bundle's vectors, in hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What a declaration of module `m` is to the corpus code.
fn corpus_decl(m: &str, header: &str, d: &SnapshotDecl) -> Decl {
    Decl {
        name: d.name.clone(),
        kind: d.kind.clone(),
        module: snapshot::module(m),
        library: SNAPSHOT_LIBRARY.to_string(),
        signature: d.signature.clone(),
        type_pp_full: d.type_pp_full.clone(),
        type_refs: d.type_refs.clone(),
        value_refs: d.value_refs.clone(),
        docstring: d.docstring.clone(),
        deprecated_to: d.deprecated_to.clone(),
        uses_sorry: d.uses_sorry,
        imports: Some(header.to_string()),
        ..Default::default()
    }
}

/// A declaration of Qdrant as a dependency in a prompt.
fn dependency(p: &PointPayload) -> Decl {
    Decl {
        name: p.name.clone(),
        kind: p.kind.clone(),
        module: p.module.clone(),
        library: p.library.clone(),
        signature: p.signature.clone(),
        docstring: p.docstring.clone(),
        ..Default::default()
    }
}

fn view_hash(model: &str, d: &Decl, informal: Option<&Informal>) -> String {
    hex::encode(text_hash(model, &document_view(d, informal)))
}

fn encode_vector(v: &[i8]) -> String {
    hex::encode(v.iter().map(|&x| x as u8).collect::<Vec<u8>>())
}

fn decode_vector(s: &str) -> Option<Vec<f32>> {
    let bytes = hex::decode(s).ok()?;
    (bytes.len() == DIMS).then(|| {
        bytes
            .into_iter()
            .map(|b| f32::from(b as i8) / 127.0)
            .collect()
    })
}

fn parse_described(records: Vec<(String, Value)>) -> HashMap<String, Described> {
    records
        .into_iter()
        .filter_map(|(name, record)| Some((name, serde_json::from_value(record).ok()?)))
        .collect()
}

/// A catalog error as an `anyhow` one.
fn store(e: pisa_protocol::api::Error) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// How long until `until`, if that is later than now.
fn remaining(until: &Mutex<Option<Instant>>) -> Option<Duration> {
    let until = (*until.lock().unwrap())?;
    until.checked_duration_since(Instant::now())
}

/// The sooner of two waits.
fn sooner(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// A task aborted when this is dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How snapshot declarations are described and embedded.
pub struct Describe {
    pub llm: Option<(Llm, Config)>,
    pub embedder: Option<Embedder>,
}

/// A module as the catalog has it.
struct Module {
    decls: SnapshotDecls,
    described: HashMap<String, Described>,
    has_entries: bool,
    has_heads: bool,
}

pub struct Indexer {
    engine: Arc<Engine>,
    catalog: Catalog,
    how: Describe,
    owner: String,
    ttl: Duration,
    /// When this replica's lease ends, in microseconds, or 0.
    until: AtomicU64,
    prompts: Prompts,
    /// Bounds the LLM and embedding requests in flight.
    limit: Semaphore,
    concurrency: usize,
    queue: Mutex<Option<mpsc::UnboundedSender<String>>>,
    /// Modules queued or being described, so each is there once, and those
    /// queued again meanwhile, which go back once they are done.
    queued: Mutex<HashSet<String>>,
    again: Mutex<HashSet<String>>,
    /// One writer at a time per module, so that each upsert is of what the
    /// catalog has, read under the lock.
    module_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Set by an authentication error: describing waits until then.
    paused_until: Mutex<Option<Instant>>,
    /// Set by an embedding failure: embedding waits until then.
    embedding_paused_until: Mutex<Option<Instant>>,
}

impl Indexer {
    /// The indexer of the snapshots of `version` in `db`, for `engine`'s
    /// collections, leading with leases of `ttl` (normally [`LEASE_TTL`]).
    pub fn new(
        engine: Arc<Engine>,
        db: Db,
        version: String,
        how: Describe,
        concurrency: usize,
        ttl: Duration,
    ) -> Arc<Indexer> {
        let owner = hex::encode(Sha256::digest(format!(
            "{}-{}",
            std::process::id(),
            now_micros()
        )))[..16]
            .to_string();
        Arc::new(Indexer {
            engine,
            catalog: Catalog::new(db, version),
            how,
            owner,
            ttl,
            until: AtomicU64::new(0),
            prompts: Prompts::new().expect("the shipped glossaries parse"),
            limit: Semaphore::new(concurrency.max(1)),
            concurrency: concurrency.max(1),
            queue: Mutex::new(None),
            queued: Mutex::default(),
            again: Mutex::default(),
            module_locks: Mutex::default(),
            paused_until: Mutex::new(None),
            embedding_paused_until: Mutex::new(None),
        })
    }

    /// Tries for the lease for good, and leads while it holds it.
    pub async fn run(self: Arc<Self>) {
        let mut leading: Option<AbortOnDrop> = None;
        loop {
            match self.catalog.take_lease(LEASE, &self.owner, self.ttl).await {
                Ok(Some(until)) => {
                    self.until.store(until, Ordering::Release);
                    if leading.as_ref().is_none_or(|t| t.0.is_finished()) {
                        tracing::info!(owner = %self.owner, "leading the snapshot index");
                        leading = Some(AbortOnDrop(tokio::spawn(Arc::clone(&self).lead())));
                    }
                }
                Ok(None) => {
                    if leading.take().is_some() {
                        tracing::info!("another replica leads the snapshot index");
                    }
                }
                Err(e) => {
                    tracing::warn!("renewing the search lease failed: {e}");
                    if self.writable().is_err() {
                        leading = None;
                    }
                }
            }
            self.engine
                .leader
                .store(leading.is_some(), Ordering::Release);
            tokio::time::sleep(self.ttl / 3).await;
        }
    }

    /// Whether this replica may still write: more than a third of its lease
    /// is left.
    fn writable(&self) -> anyhow::Result<()> {
        let left = self
            .until
            .load(Ordering::Acquire)
            .saturating_sub(now_micros());
        anyhow::ensure!(
            left > (self.ttl / 3).as_micros() as u64,
            "the search lease is ending"
        );
        Ok(())
    }

    fn enqueue(&self, m: &str) {
        let queue = self.queue.lock().unwrap();
        let Some(tx) = queue.as_ref() else { return };
        if self.queued.lock().unwrap().insert(m.to_string()) {
            let _ = tx.send(m.to_string());
        } else {
            self.again.lock().unwrap().insert(m.to_string());
        }
    }

    /// Marks module `m` done, queueing it again if that was asked for meanwhile.
    fn done(&self, m: &str) {
        self.queued.lock().unwrap().remove(m);
        if self.again.lock().unwrap().remove(m) {
            self.enqueue(m);
        }
    }

    /// What a leader does until it is aborted: describe what is queued, and
    /// reconcile, then follow the change log, again and again.
    async fn lead(self: Arc<Self>) {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.queue.lock().unwrap() = Some(tx);
        self.queued.lock().unwrap().clear();
        self.again.lock().unwrap().clear();
        let _describer = AbortOnDrop(tokio::spawn(Arc::clone(&self).describe_queue(rx)));
        loop {
            let since = now_micros().saturating_sub(LOOKBACK.as_micros() as u64);
            let reconciled = async {
                // Collections loaded before heads were indexed lack these.
                let q = &self.engine.q;
                q.index(&self.engine.entries, "head", "bool").await?;
                q.index(&self.engine.collection, "has_heads", "bool")
                    .await?;
                self.reconcile().await
            };
            if let Err(e) = reconciled.await {
                tracing::warn!("reconciling the snapshot index failed: {e}");
                tokio::time::sleep(POLL_EVERY).await;
                continue;
            }
            self.tail(since).await;
        }
    }

    /// Module `m` as the catalog has it, if it is kept.
    async fn read_module(&self, m: &str) -> anyhow::Result<Option<Module>> {
        let Some(mut decls) = self.catalog.decls(m).await.map_err(store)? else {
            return Ok(None);
        };
        // Exported for the audit's review list, not for search.
        decls.decls.retain(|d| !d.private);
        let described = parse_described(self.catalog.described(m).await.map_err(store)?);
        let has_entries = self.catalog.has_entries(m).await.map_err(store)?;
        let has_heads = self.catalog.has_heads(m).await.map_err(store)?;
        Ok(Some(Module {
            decls,
            described,
            has_entries,
            has_heads,
        }))
    }

    /// A hash of what module `m`'s points are made from.
    fn rev(&self, module: &Module) -> String {
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(&module.decls).unwrap_or_default());
        let described: std::collections::BTreeMap<_, _> = module.described.iter().collect();
        h.update(serde_json::to_vec(&described).unwrap_or_default());
        h.update([u8::from(module.has_entries)]);
        h.update([u8::from(module.has_heads)]);
        let manifest = &self.engine.manifest;
        h.update(serde_json::to_vec(&manifest.field_stats).unwrap_or_default());
        h.update(manifest.embedding_model.as_deref().unwrap_or_default());
        hex::encode(&h.finalize()[..16])
    }

    /// The points of module `m`'s declarations.
    fn module_points(&self, m: &str, module: &Module, rev: &str) -> Vec<Value> {
        let manifest = &self.engine.manifest;
        let decls = &module.decls;
        decls
            .decls
            .iter()
            .map(|d| {
                let rec = module.described.get(&d.name);
                let informal = rec.and_then(|r| r.informal.clone());
                let decl = corpus_decl(m, &decls.header, d);
                let dense = match (&manifest.embedding_model, rec) {
                    (Some(model), Some(r))
                        if r.model.as_ref() == Some(model)
                            || r.view.as_deref()
                                == Some(&view_hash(model, &decl, informal.as_ref())) =>
                    {
                        r.vector.as_deref().and_then(decode_vector)
                    }
                    _ => None,
                };
                let stored = StoredDecl {
                    name: d.name.clone(),
                    kind: d.kind.clone(),
                    module: decl.module.clone(),
                    library: SNAPSHOT_LIBRARY.to_string(),
                    signature: d.signature.clone(),
                    type_full: d.type_pp_full.clone(),
                    type_refs: d.type_refs.clone(),
                    docstring: d.docstring.clone(),
                    deprecated_to: d.deprecated_to.clone(),
                    uses_sorry: d.uses_sorry,
                    imports: Some(decls.header.clone()),
                    informal,
                    pagerank: 1.0,
                    ..Default::default()
                };
                let mut payload = PointPayload::new("snapshot", &stored);
                payload.module_id = Some(m.to_string());
                payload.status = Some(d.status);
                payload.verified = decls.verified;
                payload.has_entries = module.has_entries;
                payload.has_heads = module.has_heads;
                payload.rev = Some(rev.to_string());
                let id = points::snapshot_id(m, &d.name);
                points::point(
                    id,
                    &payload,
                    &stored,
                    dense.as_deref(),
                    &manifest.field_stats,
                )
            })
            .collect()
    }

    fn module_lock(&self, m: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.module_locks.lock().unwrap();
        locks.entry(m.to_string()).or_default().clone()
    }

    /// Makes module `m`'s points what the catalog has, reading it under the
    /// module's lock. Skips the write if Qdrant's revision is already that:
    /// `known`, as the caller found it, or else read from Qdrant. Returns the
    /// module, or `None` if it is gone.
    async fn refresh(&self, m: &str, known: Option<&str>) -> anyhow::Result<Option<Module>> {
        let lock = self.module_lock(m);
        let guard = lock.lock().await;
        let refreshed = self.refresh_locked(m, known).await;
        drop(guard);
        let mut locks = self.module_locks.lock().unwrap();
        // Only the map and this call hold it, so nobody waits for it.
        if Arc::strong_count(&lock) <= 2 {
            locks.remove(m);
        }
        refreshed
    }

    async fn refresh_locked(&self, m: &str, known: Option<&str>) -> anyhow::Result<Option<Module>> {
        let Some(module) = self.read_module(m).await? else {
            self.delete_module(m).await?;
            return Ok(None);
        };
        let rev = self.rev(&module);
        let current = match known {
            Some(rev) => Some(rev.to_string()),
            None => self.current_rev(m).await?,
        };
        if current.as_deref() != Some(rev.as_str()) {
            for batch in self.module_points(m, &module, &rev).chunks(UPSERT_BATCH) {
                self.writable()?;
                self.engine
                    .q
                    .upsert(&self.engine.collection, batch.to_vec())
                    .await?;
            }
        }
        Ok(Some(module))
    }

    /// The revision of module `m`'s points in Qdrant.
    async fn current_rev(&self, m: &str) -> anyhow::Result<Option<String>> {
        let filter = json!({"must": [is("module_id", m)]});
        let (page, _) = self
            .engine
            .q
            .scroll(&self.engine.collection, filter, json!(["rev"]), 1, None)
            .await?;
        Ok(page
            .first()
            .and_then(|p| p.payload.as_ref()?.get("rev")?.as_str().map(str::to_string)))
    }

    async fn delete_module(&self, m: &str) -> anyhow::Result<()> {
        self.writable()?;
        let filter = json!({"must": [is("module_id", m)]});
        self.engine
            .q
            .delete_where(&self.engine.collection, filter)
            .await
    }

    /// Whether module `m` lacks descriptions or vectors it could have.
    fn needs_work(&self, m: &str, module: &Module) -> bool {
        // A module with no entry, as a refused create keeps its build, waits
        // for one: search can't find it until then.
        if !module.has_entries {
            return false;
        }
        let model = self.engine.manifest.embedding_model.as_deref();
        module.decls.decls.iter().any(|d| {
            let decl = corpus_decl(m, &module.decls.header, d);
            let rec = module.described.get(&d.name);
            let describe = self.how.llm.is_some()
                && needs_description(&decl)
                && rec.is_none_or(|r| r.informal.is_none() && r.attempts < MAX_ATTEMPTS);
            let embed = match (&self.how.embedder, model) {
                (Some(_), Some(model)) => {
                    let informal = rec.and_then(|r| r.informal.as_ref());
                    rec.and_then(|r| r.view.as_deref()) != Some(&view_hash(model, &decl, informal))
                }
                _ => false,
            };
            describe || embed
        })
    }

    /// Makes Qdrant hold exactly the catalog's modules and entries, and
    /// queues the modules that lack descriptions or vectors.
    async fn reconcile(&self) -> anyhow::Result<()> {
        let started = Instant::now();
        let q = &self.engine.q;
        let (c, entries_c) = (&self.engine.collection, &self.engine.entries);
        // What Qdrant has.
        let mut have: HashMap<String, String> = HashMap::new();
        let mut offset = None;
        loop {
            let filter = json!({"must": [is("source", "snapshot")]});
            let fields = json!(["module_id", "rev"]);
            let (page, next) = q.scroll(c, filter, fields, 10_000, offset).await?;
            for p in page {
                let payload = p.payload.unwrap_or_default();
                if let (Some(m), Some(rev)) =
                    (payload["module_id"].as_str(), payload["rev"].as_str())
                {
                    have.insert(m.to_string(), rev.to_string());
                }
            }
            match next {
                Some(next) => offset = Some(next),
                None => break,
            }
        }
        let mut have_entries: HashMap<String, Value> = HashMap::new();
        let mut offset = None;
        loop {
            let (page, next) = q
                .scroll(entries_c, json!({}), json!(true), 10_000, offset)
                .await?;
            have_entries.extend(
                page.into_iter()
                    .map(|p| (p.id(), p.payload.unwrap_or_default())),
            );
            match next {
                Some(next) => offset = Some(next),
                None => break,
            }
        }
        // The entries: those missing or different, then those gone.
        let mut entries: Vec<Value> = Vec::new();
        let mut want_entries: HashSet<String> = HashSet::new();
        let mut after: Option<String> = None;
        loop {
            let page = self
                .catalog
                .entries_after(after.as_deref(), PAGE)
                .await
                .map_err(store)?;
            let ids: Vec<String> = page.iter().map(|(e, _)| e.clone()).collect();
            let replaced = self.catalog.replaced_among(&ids).await.map_err(store)?;
            for (e, meta) in &page {
                let id = points::entry_id(e);
                want_entries.insert(id.clone());
                let head = !replaced.contains(e);
                let point =
                    points::entry_point(e, &meta.module, meta.created_micros, &meta.labels, head);
                if have_entries.get(&id) != Some(&point["payload"]) {
                    entries.push(point);
                }
            }
            match page.last() {
                Some((e, _)) if page.len() == PAGE as usize => after = Some(e.clone()),
                _ => break,
            }
        }
        for batch in entries.chunks(UPSERT_BATCH) {
            self.writable()?;
            q.upsert(entries_c, batch.to_vec()).await?;
        }
        let stale: Vec<String> = have_entries
            .keys()
            .filter(|id| !want_entries.contains(*id))
            .cloned()
            .collect();
        for ids in stale.chunks(1000) {
            self.writable()?;
            q.delete_ids(entries_c, ids).await?;
        }
        // The modules.
        let mut want: HashSet<String> = HashSet::new();
        let mut after: Option<String> = None;
        let mut failed = 0;
        loop {
            let page = self
                .catalog
                .modules_with_decls_after(after.as_deref(), PAGE)
                .await
                .map_err(store)?;
            for m in &page {
                want.insert(m.clone());
                // A module not in Qdrant yet has a revision none has.
                let known = have.get(m).map_or("", String::as_str);
                match self.refresh(m, Some(known)).await {
                    Ok(Some(module)) => {
                        if self.needs_work(m, &module) {
                            self.enqueue(m);
                        }
                    }
                    Ok(None) => {}
                    // The others still get indexed; the next reconcile tries again.
                    Err(e) => {
                        failed += 1;
                        tracing::warn!(module = %m, "indexing snapshot module failed: {e}");
                    }
                }
            }
            match page.last() {
                Some(m) if page.len() == PAGE as usize => after = Some(m.clone()),
                _ => break,
            }
        }
        for m in have.keys().filter(|m| !want.contains(*m)) {
            self.delete_module(m).await?;
        }
        tracing::info!(
            modules = want.len(),
            failed,
            entries = want_entries.len(),
            elapsed = format!("{:.1}s", started.elapsed().as_secs_f64()),
            "reconciled the snapshot index"
        );
        Ok(())
    }

    /// Applies the change log from `since` for [`RECONCILE_EVERY`], or until
    /// it can't be read for long.
    async fn tail(&self, since: u64) {
        let started = Instant::now();
        let mut from = since;
        let mut seen: HashMap<i64, u64> = HashMap::new();
        let mut last_read = Instant::now();
        while started.elapsed() < RECONCILE_EVERY {
            tokio::time::sleep(POLL_EVERY).await;
            match self.poll(from, &mut seen).await {
                Ok(()) => last_read = Instant::now(),
                Err(e) => {
                    tracing::warn!("applying the snapshot change log failed: {e}");
                    if last_read.elapsed() > LOOKBACK {
                        return;
                    }
                    continue;
                }
            }
            // A worker whose clock runs ahead mustn't move this past others' changes.
            let newest = seen
                .values()
                .copied()
                .max()
                .unwrap_or(from)
                .min(now_micros());
            from = from.max(newest.saturating_sub(LOOKBACK.as_micros() as u64));
            seen.retain(|_, &mut t| t >= from);
        }
    }

    /// Applies each change logged at `from` or later that isn't in `seen`.
    async fn poll(&self, from: u64, seen: &mut HashMap<i64, u64>) -> anyhow::Result<()> {
        let mut after = None;
        loop {
            let page = self
                .catalog
                .changes(from, after, PAGE)
                .await
                .map_err(store)?;
            for logged in &page {
                if seen.contains_key(&logged.id) {
                    continue;
                }
                self.apply(&logged.change).await?;
                seen.insert(logged.id, logged.logged_micros);
            }
            match page.last() {
                Some(last) if page.len() == PAGE as usize => {
                    after = Some((last.logged_micros, last.id));
                }
                _ => return Ok(()),
            }
        }
    }

    /// Brings Qdrant up to date with an entry and a module that changed.
    pub async fn apply(&self, change: &Change) -> anyhow::Result<()> {
        let q = &self.engine.q;
        if let Some(e) = &change.entry {
            self.writable()?;
            match self.catalog.find_entry(e).await.map_err(store)? {
                Some(meta) => {
                    let head = self.catalog.replacers(e).await.map_err(store)?.is_empty();
                    let point = points::entry_point(
                        e,
                        &meta.module,
                        meta.created_micros,
                        &meta.labels,
                        head,
                    );
                    q.upsert(&self.engine.entries, vec![point]).await?;
                }
                None => {
                    q.delete_ids(&self.engine.entries, &[points::entry_id(e)])
                        .await?
                }
            }
        }
        if let Some(m) = &change.module
            && let Some(module) = self.refresh(m, None).await?
            && self.needs_work(m, &module)
        {
            self.enqueue(m);
        }
        Ok(())
    }

    /// Describes and embeds the queued modules, `concurrency` at a time.
    async fn describe_queue(self: Arc<Self>, rx: mpsc::UnboundedReceiver<String>) {
        let modules =
            futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|m| (m, rx)) });
        let concurrency = self.concurrency;
        modules
            .for_each_concurrent(concurrency, |m| {
                let this = Arc::clone(&self);
                async move {
                    let wait = match this.module(&m).await {
                        Ok(wait) => wait,
                        Err(e) => {
                            tracing::warn!(module = %m, "describing snapshot module failed: {e}");
                            Some(RETRY_AFTER)
                        }
                    };
                    this.done(&m);
                    if let Some(wait) = wait {
                        let this = Arc::clone(&this);
                        tokio::spawn(async move {
                            tokio::time::sleep(wait).await;
                            this.enqueue(&m);
                        });
                    }
                }
            })
            .await;
    }

    /// Describes and embeds what module `m` lacks, and stores it. Returns
    /// when to try again, if some of it failed.
    async fn module(&self, m: &str) -> anyhow::Result<Option<Duration>> {
        let Some(mut module) = self.read_module(m).await? else {
            return Ok(None);
        };
        let header = module.decls.header.clone();
        let decls: Vec<Decl> = module
            .decls
            .decls
            .iter()
            .map(|d| corpus_decl(m, &header, d))
            .collect();
        // A vector at once, from the docstring until a description lands.
        let mut retry = self.embed(m, &decls, &mut module).await?;
        if let Some((llm, cfg)) = &self.how.llm {
            let targets: Vec<&Decl> = decls
                .iter()
                .filter(|d| needs_description(d))
                .filter(|d| {
                    module
                        .described
                        .get(&d.name)
                        .is_none_or(|r| r.informal.is_none() && r.attempts < MAX_ATTEMPTS)
                })
                .collect();
            if !targets.is_empty() {
                let wait = match remaining(&self.paused_until) {
                    Some(wait) => Some(wait),
                    None => {
                        self.describe(m, llm, cfg, &decls, &targets, &mut module)
                            .await?
                    }
                };
                retry = sooner(retry, wait);
                retry = sooner(retry, self.embed(m, &decls, &mut module).await?);
            }
        }
        Ok(retry)
    }

    /// Embeds the declarations of `decls` whose vector isn't of their
    /// current view, and stores them.
    async fn embed(
        &self,
        m: &str,
        decls: &[Decl],
        module: &mut Module,
    ) -> anyhow::Result<Option<Duration>> {
        let (Some(embedder), Some(model)) =
            (&self.how.embedder, &self.engine.manifest.embedding_model)
        else {
            return Ok(None);
        };
        if let Some(wait) = remaining(&self.embedding_paused_until) {
            return Ok(Some(wait));
        }
        let stale: Vec<(&Decl, String, String)> = decls
            .iter()
            .filter_map(|d| {
                let rec = module.described.get(&d.name);
                let view = document_view(d, rec.and_then(|r| r.informal.as_ref()));
                let hash = hex::encode(text_hash(model, &view));
                (rec.and_then(|r| r.view.as_deref()) != Some(&hash)).then_some((d, view, hash))
            })
            .collect();
        let mut changed = BTreeSet::new();
        let mut retry = None;
        for chunk in stale.chunks(EMBED_BATCH) {
            let texts: Vec<String> = chunk.iter().map(|(_, v, _)| v.clone()).collect();
            let embedder = embedder.clone();
            let _permit = self.limit.acquire().await?;
            let vectors = tokio::task::spawn_blocking(move || embedder.embed(&texts)).await?;
            match vectors {
                Ok(vectors) => {
                    for ((d, _, hash), v) in chunk.iter().zip(vectors) {
                        let rec = module.described.entry(d.name.clone()).or_default();
                        rec.view = Some(hash.clone());
                        rec.model = Some(model.clone());
                        rec.vector = Some(encode_vector(&quantize(&v)));
                        changed.insert(d.name.clone());
                    }
                }
                Err(e) => {
                    tracing::warn!(module = %m, "embedding snapshot declarations failed: {e}");
                    *self.embedding_paused_until.lock().unwrap() =
                        Some(Instant::now() + RETRY_AFTER);
                    retry = Some(RETRY_AFTER);
                    break;
                }
            }
        }
        self.save(m, module, &mut changed).await?;
        Ok(retry)
    }

    /// Stores the records of `module` that `changed` names in the catalog,
    /// then writes the module's points from what it now has; empties
    /// `changed`.
    async fn save(
        &self,
        m: &str,
        module: &Module,
        changed: &mut BTreeSet<String>,
    ) -> anyhow::Result<()> {
        if changed.is_empty() {
            return Ok(());
        }
        let records: Vec<(String, Value)> = changed
            .iter()
            .map(|name| Ok((name.clone(), serde_json::to_value(&module.described[name])?)))
            .collect::<anyhow::Result<_>>()?;
        changed.clear();
        self.writable()?;
        if self
            .catalog
            .put_described(m, &records)
            .await
            .map_err(store)?
        {
            self.refresh(m, None).await?;
        }
        Ok(())
    }

    /// Describes `targets`, declarations of `decls`, level by level, into
    /// `module`, storing each level as it finishes. Returns when to try
    /// again if some failed.
    async fn describe(
        &self,
        m: &str,
        llm: &Llm,
        cfg: &Config,
        decls: &[Decl],
        targets: &[&Decl],
        module: &mut Module,
    ) -> anyhow::Result<Option<Duration>> {
        // The context: this module's declarations and what they use.
        let own: HashSet<&str> = decls.iter().map(|d| d.name.as_str()).collect();
        let deps: Vec<String> = targets
            .iter()
            .flat_map(|d| d.description_deps())
            .filter(|dep| !own.contains(dep.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut known: HashMap<String, Informal> = HashMap::new();
        let mut context: Vec<Decl> = decls.to_vec();
        if !deps.is_empty() {
            // The bundle's first: a snapshot's declaration of the same name
            // may be another module's.
            let fields = json!([
                "name",
                "kind",
                "library",
                "module",
                "signature",
                "docstring",
                "informal"
            ]);
            let searches = ["base", "snapshot"]
                .map(|source| {
                    json!({
                        "filter": {"must": [any("name", &deps), is("source", source)]},
                        "limit": deps.len() * 2,
                        "with_payload": fields,
                    })
                })
                .to_vec();
            let found = self
                .engine
                .q
                .query_batch(&self.engine.collection, searches)
                .await?;
            let mut added = HashSet::new();
            for p in found.into_iter().flatten() {
                let Some(payload) = p.payload else { continue };
                let Ok(p) = serde_json::from_value::<PointPayload>(payload) else {
                    continue;
                };
                if !added.insert(p.name.clone()) {
                    continue;
                }
                if let Some(i) = &p.informal {
                    known.insert(p.name.clone(), i.clone());
                }
                context.push(dependency(&p));
            }
        }
        for (name, rec) in &module.described {
            if let Some(i) = &rec.informal {
                known.insert(name.clone(), i.clone());
            }
        }
        let corpus = Corpus {
            manifest: Manifest::default(),
            decls: context,
            modules: Vec::new(),
        };
        let mut ctx = Context::new(&corpus, known);
        let mut retry = None;
        let mut changed = BTreeSet::new();
        for level in levels(targets) {
            let jobs = level
                .iter()
                .map(|&i| (i, targets[i], ctx.prompt(targets[i])))
                .collect();
            let mut answers = std::pin::pin!(informalize::complete(
                llm,
                cfg,
                &self.prompts,
                jobs,
                &self.limit
            ));
            while let Some((i, result)) = answers.next().await {
                let d = targets[i];
                let error = match result {
                    Ok(r) => {
                        let outcome = ctx.outcome(cfg, d, r);
                        match outcome.entry {
                            Some(entry) => {
                                ctx.described.insert(d.name.clone(), entry.informal.clone());
                                let rec = module.described.entry(d.name.clone()).or_default();
                                rec.informal = Some(entry.informal);
                                rec.error = None;
                                changed.insert(d.name.clone());
                                continue;
                            }
                            None => outcome.error.unwrap_or_else(|| "no description".into()),
                        }
                    }
                    Err(e @ llm::Error::Auth { .. }) => {
                        tracing::warn!("describing paused for {AUTH_PAUSE:?}: {e}");
                        *self.paused_until.lock().unwrap() = Some(Instant::now() + AUTH_PAUSE);
                        self.save(m, module, &mut changed).await?;
                        return Ok(Some(AUTH_PAUSE));
                    }
                    Err(e) => e.to_string(),
                };
                tracing::warn!(name = %d.name, "describing failed: {error}");
                let rec = module.described.entry(d.name.clone()).or_default();
                rec.attempts += 1;
                rec.error = Some(error);
                changed.insert(d.name.clone());
                if rec.attempts < MAX_ATTEMPTS {
                    let wait = RETRY_AFTER * 2u32.pow(rec.attempts - 1);
                    retry = sooner(retry, Some(wait));
                }
            }
            self.save(m, module, &mut changed).await?;
        }
        Ok(retry)
    }
}
