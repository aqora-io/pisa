//! Snapshot modules in search: their declarations, described and embedded as
//! the bundle's are, in an index next to the bundle's that follows the
//! snapshot catalog in the KV store.
//!
//! [`Snapshots::follow`] loads every module's declarations and every entry,
//! then applies the catalog's change log as workers append to it.
//! [`describe`] gives each declaration a description (with an LLM, when one
//! is configured) and a vector, and stores both in the catalog
//! (`described` records), so a restart pays for nothing again.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use futures::StreamExt;
use pisa_kv::catalog::{Catalog, Change, EntryMeta, LOG_RETENTION, SnapshotDecl, SnapshotDecls};
use pisa_protocol::search::{Informal, SnapshotHit, SnapshotScope};
use pisa_protocol::snapshot;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tantivy::collector::{FilterCollector, TopDocs};
use tantivy::query::{Bm25StatisticsProvider, Query as TQuery};
use tantivy::schema::{Field, Value};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, Term};
use tokio::sync::{Semaphore, mpsc};

use crate::corpus::{Corpus, Decl, Manifest};
use crate::dense::{self, Embedder, best, document_view, dot, quantize, text_hash};
use crate::engine::Engine;
use crate::index::{self, Fields, StoredDecl};
use crate::informalize::{self, Config, Context, Prompts, levels, needs_description};
use crate::llm::{self, Llm};

/// The library of every snapshot declaration: its modules' root.
pub const LIBRARY: &str = snapshot::ROOT;

/// How far back each read of the change log starts again: a transaction can
/// commit after a later one, and workers' clocks differ.
const LOOKBACK: Duration = Duration::from_secs(120);

const POLL_EVERY: Duration = Duration::from_secs(2);

/// Keys read per page of a scan.
const PAGE: u32 = 1000;

/// Modules' declaration records read per page: each holds a module's
/// signatures, docstrings and references, up to 6 MiB.
const DECLS_PAGE: u32 = 64;

/// The most entries a hit names.
const MAX_HIT_ENTRIES: usize = 10;

/// Attempts at describing a declaration; after these it keeps its docstring view.
const MAX_ATTEMPTS: u32 = 3;

/// The first wait before describing a module again after a failure; it
/// doubles with each attempt.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// How long describing stops after the LLM refuses the key.
const AUTH_PAUSE: Duration = Duration::from_secs(300);

/// Document views embedded per request.
const EMBED_BATCH: usize = 32;

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
        library: LIBRARY.to_string(),
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

/// A stored declaration as a dependency in a prompt.
fn dependency(s: &StoredDecl) -> Decl {
    Decl {
        name: s.name.clone(),
        kind: s.kind.clone(),
        module: s.module.clone(),
        library: s.library.clone(),
        signature: s.signature.clone(),
        docstring: s.docstring.clone(),
        ..Default::default()
    }
}

fn view_hash(model: &str, d: &Decl, informal: Option<&Informal>) -> String {
    hex::encode(text_hash(model, &document_view(d, informal)))
}

fn encode_vector(v: &[i8]) -> String {
    hex::encode(v.iter().map(|&x| x as u8).collect::<Vec<u8>>())
}

fn decode_vector(s: &str) -> Option<Vec<i8>> {
    let bytes = hex::decode(s).ok()?;
    (bytes.len() == dense::DIMS).then(|| bytes.into_iter().map(|b| b as i8).collect())
}

struct Slot {
    decl: StoredDecl,
    type_refs: Vec<String>,
    module: u32,
    vector: Option<Vec<i8>>,
}

struct ModuleState {
    id: String,
    decls: SnapshotDecls,
    /// The slot of each of `decls`' declarations.
    slots: Vec<u32>,
    described: HashMap<String, Described>,
}

/// The modules a query covers.
pub struct Scope {
    /// Whether each module slot is in it.
    modules: Arc<Vec<bool>>,
    /// The labels its entries have, all of them, unless it is every module.
    labels: Option<BTreeMap<String, String>>,
}

impl Scope {
    fn contains(&self, module: u32) -> bool {
        self.modules.get(module as usize).copied().unwrap_or(false)
    }
}

/// BM25 statistics over the bundle and the snapshot index together, so that
/// scores from either compare.
pub struct Combined<'a>(pub &'a Searcher, pub &'a Searcher);

impl Bm25StatisticsProvider for Combined<'_> {
    fn total_num_tokens(&self, field: Field) -> tantivy::Result<u64> {
        Ok(Bm25StatisticsProvider::total_num_tokens(self.0, field)?
            + Bm25StatisticsProvider::total_num_tokens(self.1, field)?)
    }

    fn total_num_docs(&self) -> tantivy::Result<u64> {
        Ok(Bm25StatisticsProvider::total_num_docs(self.0)?
            + Bm25StatisticsProvider::total_num_docs(self.1)?)
    }

    fn doc_freq(&self, term: &Term) -> tantivy::Result<u64> {
        Ok(Bm25StatisticsProvider::doc_freq(self.0, term)?
            + Bm25StatisticsProvider::doc_freq(self.1, term)?)
    }
}

/// The snapshot declarations, indexed like the bundle's. A declaration's id
/// is the bundle's size plus its slot, so ids of both never collide. A slot,
/// or a module's number, is reused only once the removal that freed it is
/// committed, so a search never sees a document of what was there before.
pub struct SnapshotIndex {
    base: u32,
    writer: IndexWriter,
    reader: IndexReader,
    pub fields: Fields,
    module_field: Field,
    uncommitted: bool,
    slots: Vec<Option<Slot>>,
    modules: Vec<Option<ModuleState>>,
    /// Slots and module numbers freed since the last commit, and before it.
    freed: Freed,
    free: Freed,
    module_slots: HashMap<String, u32>,
    entries: HashMap<String, EntryMeta>,
    /// Each module's entries, newest first.
    by_module: HashMap<String, BTreeSet<(Reverse<u64>, String)>>,
    labels: HashMap<(String, String), HashSet<String>>,
    by_name: HashMap<String, Vec<u32>>,
    by_lower: HashMap<String, Vec<u32>>,
    by_last: HashMap<String, Vec<u32>>,
}

#[derive(Default)]
struct Freed {
    slots: Vec<u32>,
    modules: Vec<u32>,
}

/// Puts `value` at a free index of `items`, or at the end; returns the index.
fn place<T>(items: &mut Vec<Option<T>>, free: &mut Vec<u32>, value: T) -> u32 {
    match free.pop() {
        Some(i) => {
            items[i as usize] = Some(value);
            i
        }
        None => {
            items.push(Some(value));
            items.len() as u32 - 1
        }
    }
}

fn last_lower(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).to_lowercase()
}

fn unlink(map: &mut HashMap<String, Vec<u32>>, key: &str, slot: u32) {
    if let Some(v) = map.get_mut(key) {
        v.retain(|&s| s != slot);
        if v.is_empty() {
            map.remove(key);
        }
    }
}

impl SnapshotIndex {
    /// An empty index whose ids start at `base`, the bundle's size.
    pub fn new(base: u32) -> anyhow::Result<SnapshotIndex> {
        let (schema, fields, module_field) = index::snapshot_schema();
        let index = Index::create_in_ram(schema);
        index::register_tokenizers(&index);
        let writer = index.writer(100_000_000)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(SnapshotIndex {
            base,
            writer,
            reader,
            fields,
            module_field,
            uncommitted: false,
            slots: Vec::new(),
            modules: Vec::new(),
            freed: Freed::default(),
            free: Freed::default(),
            module_slots: HashMap::new(),
            entries: HashMap::new(),
            by_module: HashMap::new(),
            labels: HashMap::new(),
            by_name: HashMap::new(),
            by_lower: HashMap::new(),
            by_last: HashMap::new(),
        })
    }

    pub fn put_entry(&mut self, e: &str, meta: EntryMeta) {
        self.remove_entry(e);
        for (k, v) in &meta.labels {
            self.labels
                .entry((k.clone(), v.clone()))
                .or_default()
                .insert(e.to_string());
        }
        self.by_module
            .entry(meta.module.clone())
            .or_default()
            .insert((Reverse(meta.created_micros), e.to_string()));
        self.entries.insert(e.to_string(), meta);
    }

    pub fn remove_entry(&mut self, e: &str) {
        let Some(old) = self.entries.remove(e) else {
            return;
        };
        for (k, v) in old.labels {
            let key = (k, v);
            if let Some(set) = self.labels.get_mut(&key) {
                set.remove(e);
                if set.is_empty() {
                    self.labels.remove(&key);
                }
            }
        }
        if let Some(set) = self.by_module.get_mut(&old.module) {
            set.remove(&(Reverse(old.created_micros), e.to_string()));
            if set.is_empty() {
                self.by_module.remove(&old.module);
            }
        }
    }

    fn module(&self, m: &str) -> Option<&ModuleState> {
        self.modules[*self.module_slots.get(m)? as usize].as_ref()
    }

    /// Indexes module `m`, with what the search service stored for its
    /// declarations; a vector counts only if it embeds the current view with
    /// `model`. Returns whether the module is new: one already indexed only
    /// takes the flags of `decls`, as a module's declarations don't change.
    pub fn put_module(
        &mut self,
        m: &str,
        decls: SnapshotDecls,
        mut described: HashMap<String, Described>,
        model: Option<&str>,
    ) -> bool {
        if let Some(&module) = self.module_slots.get(m) {
            let state = self.modules[module as usize].as_mut().expect("live module");
            for (d, &slot) in decls.decls.iter().zip(&state.slots) {
                if let Some(s) = self.slots[slot as usize].as_mut() {
                    s.decl.uses_sorry = d.uses_sorry;
                }
            }
            state.decls = decls;
            return false;
        }
        let module = place(
            &mut self.modules,
            &mut self.free.modules,
            ModuleState {
                id: m.to_string(),
                decls: SnapshotDecls {
                    header: String::new(),
                    dirty: false,
                    verified: false,
                    decls: Vec::new(),
                },
                slots: Vec::new(),
                described: HashMap::new(),
            },
        );
        let mut slots = Vec::with_capacity(decls.decls.len());
        for d in &decls.decls {
            let rec = described.get(&d.name);
            let informal = rec.and_then(|r| r.informal.clone());
            let decl = corpus_decl(m, &decls.header, d);
            let vector = match (model, rec) {
                (Some(model), Some(r))
                    if r.view.as_deref() == Some(&view_hash(model, &decl, informal.as_ref())) =>
                {
                    r.vector.as_deref().and_then(decode_vector)
                }
                _ => None,
            };
            let stored = StoredDecl {
                name: d.name.clone(),
                kind: d.kind.clone(),
                module: decl.module.clone(),
                library: LIBRARY.to_string(),
                signature: d.signature.clone(),
                type_full: d.type_pp_full.clone(),
                docstring: d.docstring.clone(),
                deprecated_to: d.deprecated_to.clone(),
                uses_sorry: d.uses_sorry,
                imports: Some(decls.header.clone()),
                informal,
                pagerank: 1.0,
                ..Default::default()
            };
            let slot = place(
                &mut self.slots,
                &mut self.free.slots,
                Slot {
                    decl: stored,
                    type_refs: d.type_refs.clone(),
                    module,
                    vector,
                },
            );
            self.index(slot);
            self.by_name.entry(d.name.clone()).or_default().push(slot);
            self.by_lower
                .entry(d.name.to_lowercase())
                .or_default()
                .push(slot);
            self.by_last
                .entry(last_lower(&d.name))
                .or_default()
                .push(slot);
            slots.push(slot);
        }
        // The slots hold the vectors, so the records needn't.
        for rec in described.values_mut() {
            rec.vector = None;
        }
        let state = self.modules[module as usize].as_mut().expect("just placed");
        state.decls = decls;
        state.slots = slots;
        state.described = described;
        self.module_slots.insert(m.to_string(), module);
        true
    }

    /// Adds the document of the declaration in `slot`.
    fn index(&mut self, slot: u32) {
        let s = self.slots[slot as usize].as_ref().expect("a live slot");
        let id = u64::from(self.base + slot);
        let mut doc = index::document(&self.fields, id, &s.decl, &s.type_refs);
        doc.add_u64(self.module_field, u64::from(s.module));
        // Adding only fails once the writer has panicked.
        let _ = self.writer.add_document(doc);
        self.uncommitted = true;
    }

    pub fn remove_module(&mut self, m: &str) {
        let Some(module) = self.module_slots.remove(m) else {
            return;
        };
        let Some(state) = self.modules[module as usize].take() else {
            return;
        };
        for slot in state.slots {
            if let Some(s) = self.slots[slot as usize].take() {
                let name = &s.decl.name;
                unlink(&mut self.by_name, name, slot);
                unlink(&mut self.by_lower, &name.to_lowercase(), slot);
                unlink(&mut self.by_last, &last_lower(name), slot);
                self.freed.slots.push(slot);
            }
        }
        self.freed.modules.push(module);
        self.writer
            .delete_term(Term::from_field_u64(self.module_field, u64::from(module)));
        self.uncommitted = true;
    }

    /// Takes the search service's new record for declaration `name` of `m`.
    fn set_described(&mut self, m: &str, name: &str, rec: Described) {
        let Some(&module) = self.module_slots.get(m) else {
            return;
        };
        let Some(state) = self.modules[module as usize].as_mut() else {
            return;
        };
        let Some(i) = state.decls.decls.iter().position(|d| d.name == name) else {
            return;
        };
        let slot = state.slots[i];
        let Some(s) = self.slots[slot as usize].as_mut() else {
            return;
        };
        if let Some(v) = rec.vector.as_deref().and_then(decode_vector) {
            s.vector = Some(v);
        }
        let reindex = s.decl.informal != rec.informal;
        s.decl.informal = rec.informal.clone();
        state.described.insert(
            name.to_string(),
            Described {
                vector: None,
                ..rec
            },
        );
        if reindex {
            let id = u64::from(self.base + slot);
            self.writer
                .delete_term(Term::from_field_u64(self.fields.id, id));
            self.index(slot);
        }
    }

    /// Makes what changed visible to searches.
    pub fn commit(&mut self) -> anyhow::Result<()> {
        if self.uncommitted {
            self.writer.commit()?;
            self.reader.reload()?;
            self.uncommitted = false;
            let freed = std::mem::take(&mut self.freed);
            self.free.slots.extend(freed.slots);
            self.free.modules.extend(freed.modules);
        }
        Ok(())
    }

    pub fn modules(&self) -> usize {
        self.module_slots.len()
    }

    pub fn decls(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    pub fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }

    /// The modules `scope` covers, or `None` for none.
    pub fn scope(&self, scope: &SnapshotScope) -> Option<Scope> {
        let mut modules = vec![false; self.modules.len()];
        let labels = match scope {
            SnapshotScope::None => return None,
            SnapshotScope::All => {
                for m in self.by_module.keys() {
                    if let Some(&slot) = self.module_slots.get(m) {
                        modules[slot as usize] = true;
                    }
                }
                None
            }
            SnapshotScope::Labels(labels) => {
                let sets: Option<Vec<&HashSet<String>>> = labels
                    .iter()
                    .map(|(k, v)| self.labels.get(&(k.clone(), v.clone())))
                    .collect();
                if let Some(smallest) = sets.and_then(|s| s.into_iter().min_by_key(|s| s.len())) {
                    for e in smallest {
                        let meta = &self.entries[e];
                        if labels.iter().all(|(k, v)| meta.labels.get(k) == Some(v))
                            && let Some(&slot) = self.module_slots.get(&meta.module)
                        {
                            modules[slot as usize] = true;
                        }
                    }
                }
                Some(labels.clone())
            }
        };
        Some(Scope {
            modules: Arc::new(modules),
            labels,
        })
    }

    fn slot_of(&self, id: u32) -> Option<&Slot> {
        self.slots
            .get(id.checked_sub(self.base)? as usize)?
            .as_ref()
    }

    pub fn decl(&self, id: u32) -> Option<&StoredDecl> {
        self.slot_of(id).map(|s| &s.decl)
    }

    pub fn has_vector(&self, id: u32) -> bool {
        self.slot_of(id).is_some_and(|s| s.vector.is_some())
    }

    fn in_scope(&self, slot: u32, scope: &Scope) -> bool {
        self.slots[slot as usize]
            .as_ref()
            .is_some_and(|s| scope.contains(s.module))
    }

    /// The best `n` in `scope` for `query`, from `searcher` (one of this
    /// index's) and scored with `stats`.
    pub fn top(
        &self,
        searcher: &Searcher,
        query: &dyn TQuery,
        stats: &dyn Bm25StatisticsProvider,
        n: usize,
        scope: &Scope,
    ) -> Vec<(f32, u32)> {
        let modules = Arc::clone(&scope.modules);
        let collector = FilterCollector::new(
            "module".to_string(),
            move |m: u64| modules.get(m as usize).copied().unwrap_or(false),
            TopDocs::with_limit(n).order_by_score(),
        );
        let Ok(top) = searcher.search_with_statistics_provider(query, &collector, stats) else {
            return Vec::new();
        };
        top.into_iter()
            .filter_map(|(score, addr)| {
                let doc: TantivyDocument = searcher.doc(addr).ok()?;
                let id = doc.get_first(self.fields.id)?.as_u64()? as u32;
                // A search can see a removed module until the next commit.
                self.slot_of(id)?;
                Some((score, id))
            })
            .collect()
    }

    /// Whether a snapshot declaration is named `name`.
    pub fn has_name(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// The ids in `scope` of the declarations named `name`.
    pub fn exact(&self, name: &str, scope: &Scope) -> Vec<u32> {
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter(|&&slot| self.in_scope(slot, scope))
            .map(|&slot| self.base + slot)
            .collect()
    }

    /// The ids in `scope` named like `name`, by case or last component, as
    /// candidates for its closest matches.
    pub fn name_candidates(&self, lower: &str, last: &str, scope: &Scope) -> Vec<u32> {
        let in_scope = |slot: &&u32| self.in_scope(**slot, scope);
        let by_lower = self.by_lower.get(lower).into_iter().flatten();
        let by_last = self.by_last.get(last).into_iter().flatten();
        by_lower
            .filter(in_scope)
            .chain(by_last.filter(in_scope).take(200))
            .map(|&slot| self.base + slot)
            .collect()
    }

    /// The `n` vectors in `scope` nearest to `query`, best first.
    pub fn nearest(&self, query: &[i8], n: usize, scope: &Scope) -> Vec<(i32, u32)> {
        let scored: Vec<(i32, u32)> = self
            .slots
            .par_iter()
            .enumerate()
            .filter_map(|(slot, s)| {
                let s = s.as_ref()?;
                if !scope.contains(s.module) {
                    return None;
                }
                Some((dot(s.vector.as_ref()?, query), self.base + slot as u32))
            })
            .collect();
        best(scored, n)
    }

    /// What a hit on `id` says about its module, for `scope`.
    pub fn hit(&self, id: u32, scope: &Scope) -> Option<SnapshotHit> {
        let s = self.slot_of(id)?;
        let state = self.modules[s.module as usize].as_ref()?;
        let entries = self
            .by_module
            .get(&state.id)
            .into_iter()
            .flatten()
            .filter(|(_, e)| match &scope.labels {
                None => true,
                Some(labels) => {
                    let meta = &self.entries[e];
                    labels.iter().all(|(k, v)| meta.labels.get(k) == Some(v))
                }
            })
            .take(MAX_HIT_ENTRIES)
            .map(|(_, e)| e.clone())
            .collect();
        Some(SnapshotHit {
            entries,
            dirty: state.decls.dirty,
            verified: state.decls.verified,
        })
    }
}

/// The snapshot side of a search server.
pub struct Snapshots {
    pub index: RwLock<SnapshotIndex>,
    loaded: AtomicBool,
    catalog: Catalog,
    /// The bundle's embedding model, which snapshot vectors must share.
    model: Option<String>,
    base: u32,
    queue: mpsc::UnboundedSender<String>,
    /// Modules on the queue, so each is there once.
    queued: Mutex<HashSet<String>>,
}

/// The modules waiting to be described and embedded.
pub struct Queue(mpsc::UnboundedReceiver<String>);

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// The time a change-log key names.
fn logged_at(key: &[u8]) -> u64 {
    let parts = pisa_kv::kv::decode(key);
    parts
        .get(3)
        .and_then(|t| u64::from_str_radix(t, 16).ok())
        .unwrap_or(0)
}

/// A catalog error as an `anyhow` one.
fn store(e: pisa_protocol::api::Error) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

fn parse_described(records: Vec<(String, Vec<u8>)>) -> HashMap<String, Described> {
    records
        .into_iter()
        .filter_map(|(name, bytes)| Some((name, serde_json::from_slice(&bytes).ok()?)))
        .collect()
}

impl Snapshots {
    /// Follows the catalog of version `version` in `kv`; `base` is the
    /// bundle's size and `model` its embedding model.
    pub fn new(
        kv: pisa_kv::kv::Kv,
        version: String,
        base: u32,
        model: Option<String>,
    ) -> anyhow::Result<(Arc<Snapshots>, Queue)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let snapshots = Snapshots {
            index: RwLock::new(SnapshotIndex::new(base)?),
            loaded: AtomicBool::new(false),
            catalog: Catalog::new(kv, version),
            model,
            base,
            queue: tx,
            queued: Mutex::default(),
        };
        Ok((Arc::new(snapshots), Queue(rx)))
    }

    /// Whether the first load has finished.
    pub fn loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    fn enqueue(&self, m: &str) {
        if self.queued.lock().unwrap().insert(m.to_string()) {
            let _ = self.queue.send(m.to_string());
        }
    }

    /// Loads the catalog, then applies its change log, for good. Starts over
    /// with a full load when it can't read the log for half of the time the
    /// log is kept.
    pub async fn follow(self: Arc<Self>) {
        loop {
            let since = match self.load().await {
                Ok(since) => since,
                Err(e) => {
                    tracing::warn!("loading snapshots failed, retrying: {e}");
                    tokio::time::sleep(POLL_EVERY).await;
                    continue;
                }
            };
            self.tail(since).await;
        }
    }

    /// Up to `*size` records of kind `kind` after `after`, and whether there
    /// may be more. Records large together can exceed what one response
    /// carries, so a read that fails is tried again at half the size, down to
    /// one record; `*size` grows back towards `max` after each page.
    async fn page(
        &self,
        kind: &str,
        after: Option<&[u8]>,
        size: &mut u32,
        max: u32,
    ) -> anyhow::Result<(Vec<(Vec<u8>, Vec<String>, Vec<u8>)>, bool)> {
        loop {
            match self.catalog.scan(kind, after, *size).await {
                Ok(page) => {
                    let more = page.len() == *size as usize;
                    *size = (*size * 2).min(max);
                    return Ok((page, more));
                }
                Err(e) if *size > 1 => {
                    tracing::debug!(kind, size = *size, "reading a page failed, halving it: {e}");
                    *size /= 2;
                }
                Err(e) => return Err(store(e)),
            }
        }
    }

    /// Builds a new index from every entry and module in the catalog and
    /// returns the time to read the change log from.
    pub async fn load(&self) -> anyhow::Result<u64> {
        let since = now_micros().saturating_sub(LOOKBACK.as_micros() as u64);
        let started = Instant::now();
        let mut index = SnapshotIndex::new(self.base)?;
        let (mut after, mut size): (Option<Vec<u8>>, u32) = (None, PAGE);
        loop {
            let (page, more) = self
                .page("entry", after.as_deref(), &mut size, PAGE)
                .await?;
            for (_, parts, bytes) in &page {
                if let (Some(e), Ok(meta)) = (parts.first(), serde_json::from_slice(bytes)) {
                    index.put_entry(e, meta);
                }
            }
            match page.last() {
                Some((k, ..)) if more => after = Some(k.clone()),
                _ => break,
            }
        }
        let (mut after, mut size): (Option<Vec<u8>>, u32) = (None, DECLS_PAGE);
        loop {
            let (page, more) = self
                .page("decls", after.as_deref(), &mut size, DECLS_PAGE)
                .await?;
            let modules: Vec<(&String, SnapshotDecls)> = page
                .iter()
                .filter_map(|(_, parts, bytes)| {
                    Some((parts.first()?, serde_json::from_slice(bytes).ok()?))
                })
                .collect();
            let described = futures::future::try_join_all(
                modules.iter().map(|(m, _)| self.catalog.described(m)),
            )
            .await
            .map_err(store)?;
            for ((m, decls), described) in modules.into_iter().zip(described) {
                let described = parse_described(described);
                index.put_module(m, decls, described, self.model.as_deref());
            }
            match page.last() {
                Some((k, ..)) if more => after = Some(k.clone()),
                _ => break,
            }
        }
        index.commit()?;
        tracing::info!(
            modules = index.modules(),
            decls = index.decls(),
            entries = index.entries.len(),
            elapsed = format!("{:.1}s", started.elapsed().as_secs_f64()),
            "loaded snapshots"
        );
        // Oldest first, by each module's first entry.
        let mut modules: Vec<(u64, String)> = index
            .module_slots
            .keys()
            .map(|m| {
                let first = index.by_module.get(m).and_then(|e| e.last());
                (first.map_or(u64::MAX, |(Reverse(t), _)| *t), m.clone())
            })
            .collect();
        modules.sort();
        *self.index.write().unwrap() = index;
        self.loaded.store(true, Ordering::Release);
        for (_, m) in modules {
            self.enqueue(&m);
        }
        Ok(since)
    }

    /// Applies the change log from `since` until it can't be read for half
    /// of [`LOG_RETENTION`].
    async fn tail(&self, since: u64) {
        let mut from = since;
        let mut seen: HashMap<Vec<u8>, u64> = HashMap::new();
        let mut last_read = Instant::now();
        loop {
            tokio::time::sleep(POLL_EVERY).await;
            match self.poll(from, &mut seen).await {
                Ok(()) => last_read = Instant::now(),
                Err(e) => {
                    tracing::warn!("reading the snapshot change log failed: {e}");
                    if last_read.elapsed() > LOG_RETENTION / 2 {
                        return;
                    }
                    continue;
                }
            }
            let newest = seen.values().copied().max().unwrap_or(from);
            from = from.max(newest.saturating_sub(LOOKBACK.as_micros() as u64));
            seen.retain(|_, &mut t| t >= from);
        }
    }

    /// Applies each change logged at `from` or later that isn't in `seen`.
    async fn poll(&self, from: u64, seen: &mut HashMap<Vec<u8>, u64>) -> anyhow::Result<()> {
        let mut after: Option<Vec<u8>> = None;
        let mut applied = false;
        loop {
            let page = self
                .catalog
                .changes(from, after.as_deref(), PAGE)
                .await
                .map_err(store)?;
            for (key, change) in &page {
                if seen.contains_key(key) {
                    continue;
                }
                self.apply(change).await?;
                seen.insert(key.clone(), logged_at(key));
                applied = true;
            }
            match page.last() {
                Some((k, _)) if page.len() == PAGE as usize => after = Some(k.clone()),
                _ => break,
            }
        }
        if applied {
            self.index.write().unwrap().commit()?;
        }
        Ok(())
    }

    /// Brings the index up to date with an entry and a module that changed.
    pub async fn apply(&self, change: &Change) -> anyhow::Result<()> {
        if let Some(e) = &change.entry {
            let found = self.catalog.find_entry(e).await.map_err(store)?;
            let mut index = self.index.write().unwrap();
            match found {
                Some(meta) => index.put_entry(e, meta),
                None => index.remove_entry(e),
            }
        }
        if let Some(m) = &change.module {
            match self.catalog.decls(m).await.map_err(store)? {
                Some(decls) => {
                    let known = self.index.read().unwrap().module(m).is_some();
                    let described = if known {
                        HashMap::new()
                    } else {
                        parse_described(self.catalog.described(m).await.map_err(store)?)
                    };
                    let new = self.index.write().unwrap().put_module(
                        m,
                        decls,
                        described,
                        self.model.as_deref(),
                    );
                    if new {
                        self.enqueue(m);
                    }
                }
                None => self.index.write().unwrap().remove_module(m),
            }
        }
        Ok(())
    }
}

/// How modules are described and embedded.
pub struct Describe {
    pub llm: Option<(Llm, Config)>,
    pub embedder: Option<Embedder>,
}

/// Describes and embeds the modules on `queue`, `concurrency` at a time,
/// for good. `engine` gives the bundle's declarations as context.
pub async fn describe(engine: Arc<Engine>, queue: Queue, how: Describe, concurrency: usize) {
    let Some(snapshots) = engine.snapshots.clone() else {
        return;
    };
    let worker = Arc::new(Describer {
        engine,
        snapshots,
        how,
        prompts: Prompts::new().expect("the shipped glossaries parse"),
        limit: Semaphore::new(concurrency.max(1)),
        paused_until: Mutex::new(None),
        embedding_paused_until: Mutex::new(None),
    });
    let modules =
        futures::stream::unfold(
            queue.0,
            |mut rx| async move { rx.recv().await.map(|m| (m, rx)) },
        );
    modules
        .for_each_concurrent(concurrency.max(1), |m| {
            let worker = Arc::clone(&worker);
            async move {
                worker.snapshots.queued.lock().unwrap().remove(&m);
                match worker.module(&m).await {
                    Ok(Some(wait)) => worker.retry(m, wait),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(module = %m, "describing snapshot module failed: {e}");
                        worker.retry(m, RETRY_AFTER);
                    }
                }
            }
        })
        .await;
}

struct Describer {
    engine: Arc<Engine>,
    snapshots: Arc<Snapshots>,
    how: Describe,
    prompts: Prompts,
    /// Bounds the LLM and embedding requests in flight.
    limit: Semaphore,
    /// Set by an authentication error: describing waits until then.
    paused_until: Mutex<Option<Instant>>,
    /// Set by an embedding failure: embedding waits until then.
    embedding_paused_until: Mutex<Option<Instant>>,
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

impl Describer {
    fn retry(&self, m: String, wait: Duration) {
        let snapshots = Arc::clone(&self.snapshots);
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            snapshots.enqueue(&m);
        });
    }

    /// Describes and embeds what module `m` lacks, and stores it. Returns
    /// when to try again, if some of it failed.
    async fn module(&self, m: &str) -> anyhow::Result<Option<Duration>> {
        let (header, decls, mut described) = {
            let index = self.snapshots.index.read().unwrap();
            let Some(state) = index.module(m) else {
                return Ok(None);
            };
            // The index keeps vectors in the slots only; records carry them.
            let mut described = state.described.clone();
            for (d, &slot) in state.decls.decls.iter().zip(&state.slots) {
                let vector = index.slots[slot as usize]
                    .as_ref()
                    .and_then(|s| s.vector.as_ref());
                if let (Some(rec), Some(v)) = (described.get_mut(&d.name), vector) {
                    rec.vector = Some(encode_vector(v));
                }
            }
            (
                state.decls.header.clone(),
                state.decls.decls.clone(),
                described,
            )
        };
        let decls: Vec<Decl> = decls.iter().map(|d| corpus_decl(m, &header, d)).collect();
        let mut retry: Option<Duration> = None;

        if let Some((llm, cfg)) = &self.how.llm {
            let targets: Vec<&Decl> = decls
                .iter()
                .filter(|d| needs_description(d))
                .filter(|d| {
                    described
                        .get(&d.name)
                        .is_none_or(|r| r.informal.is_none() && r.attempts < MAX_ATTEMPTS)
                })
                .collect();
            if !targets.is_empty() {
                match remaining(&self.paused_until) {
                    Some(wait) => retry = Some(wait),
                    None => {
                        let wait = self
                            .describe(m, llm, cfg, &decls, &targets, &mut described)
                            .await?;
                        retry = sooner(retry, wait);
                    }
                }
            }
        }
        if self.snapshots.index.read().unwrap().module(m).is_none() {
            return Ok(None);
        }

        if let (Some(embedder), Some(model)) = (&self.how.embedder, &self.snapshots.model) {
            if let Some(wait) = remaining(&self.embedding_paused_until) {
                return Ok(sooner(retry, Some(wait)));
            }
            let stale: Vec<(&Decl, String, String)> = decls
                .iter()
                .filter_map(|d| {
                    let informal = described.get(&d.name).and_then(|r| r.informal.as_ref());
                    let view = document_view(d, informal);
                    let hash = hex::encode(text_hash(model, &view));
                    let current = described.get(&d.name).and_then(|r| r.view.as_deref());
                    (current != Some(&hash)).then_some((d, view, hash))
                })
                .collect();
            let mut changed = BTreeSet::new();
            for chunk in stale.chunks(EMBED_BATCH) {
                let texts: Vec<String> = chunk.iter().map(|(_, v, _)| v.clone()).collect();
                let embedder = embedder.clone();
                let _permit = self.limit.acquire().await?;
                let vectors = tokio::task::spawn_blocking(move || embedder.embed(&texts)).await?;
                match vectors {
                    Ok(vectors) => {
                        for ((d, _, hash), v) in chunk.iter().zip(vectors) {
                            let rec = described.entry(d.name.clone()).or_default();
                            rec.view = Some(hash.clone());
                            rec.vector = Some(encode_vector(&quantize(&v)));
                            changed.insert(d.name.clone());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(module = %m, "embedding snapshot declarations failed: {e}");
                        *self.embedding_paused_until.lock().unwrap() =
                            Some(Instant::now() + RETRY_AFTER);
                        retry = sooner(retry, Some(RETRY_AFTER));
                        break;
                    }
                }
            }
            self.save(m, &described, &mut changed).await?;
        }
        Ok(retry)
    }

    /// Stores the records of `described` that `changed` names, and puts them
    /// in the index, unless the module is gone; empties `changed`.
    async fn save(
        &self,
        m: &str,
        described: &HashMap<String, Described>,
        changed: &mut BTreeSet<String>,
    ) -> anyhow::Result<()> {
        if changed.is_empty() {
            return Ok(());
        }
        let records: Vec<(String, Vec<u8>)> = changed
            .iter()
            .map(|name| Ok((name.clone(), serde_json::to_vec(&described[name])?)))
            .collect::<anyhow::Result<_>>()?;
        let stored = self
            .snapshots
            .catalog
            .put_described(m, &records)
            .await
            .map_err(store)?;
        if stored {
            let mut index = self.snapshots.index.write().unwrap();
            for name in changed.iter() {
                index.set_described(m, name, described[name].clone());
            }
            index.commit()?;
        }
        changed.clear();
        Ok(())
    }

    /// Describes `targets`, declarations of `decls`, level by level, into
    /// `described`, storing each level as it finishes. Returns when to try
    /// again if some failed.
    async fn describe(
        &self,
        m: &str,
        llm: &Llm,
        cfg: &Config,
        decls: &[Decl],
        targets: &[&Decl],
        described: &mut HashMap<String, Described>,
    ) -> anyhow::Result<Option<Duration>> {
        // The context: this module's declarations and what they use.
        let own: HashSet<&str> = decls.iter().map(|d| d.name.as_str()).collect();
        let mut known: HashMap<String, Informal> = HashMap::new();
        let mut context: Vec<Decl> = decls.to_vec();
        {
            let index = self.snapshots.index.read().unwrap();
            let mut added: HashSet<String> = HashSet::new();
            for d in targets {
                for dep in d.description_deps() {
                    if own.contains(dep.as_str()) || !added.insert(dep.clone()) {
                        continue;
                    }
                    let stored = self.engine.base_decl(dep).or_else(|| {
                        let slot = *index.by_name.get(dep)?.first()?;
                        index.decl(index.base + slot)
                    });
                    if let Some(s) = stored {
                        if let Some(i) = &s.informal {
                            known.insert(s.name.clone(), i.clone());
                        }
                        context.push(dependency(s));
                    }
                }
            }
        }
        for (name, rec) in described.iter() {
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
                                let rec = described.entry(d.name.clone()).or_default();
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
                        self.save(m, described, &mut changed).await?;
                        return Ok(Some(AUTH_PAUSE));
                    }
                    Err(e) => e.to_string(),
                };
                tracing::warn!(name = %d.name, "describing failed: {error}");
                let rec = described.entry(d.name.clone()).or_default();
                rec.attempts += 1;
                rec.error = Some(error);
                changed.insert(d.name.clone());
                if rec.attempts < MAX_ATTEMPTS {
                    let wait = RETRY_AFTER * 2u32.pow(rec.attempts - 1);
                    retry = sooner(retry, Some(wait));
                }
            }
            self.save(m, described, &mut changed).await?;
        }
        Ok(retry)
    }
}
