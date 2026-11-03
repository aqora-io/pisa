//! Snapshot metadata in the KV store: the modules, which import each other,
//! and the entries, which keep them.
//!
//! KV keys, per pool version (every key starts with [`ROOT`]):
//! - `(pisa, <version>, "mod", <m>)`: module `m` ([`ModuleMeta`]); its files
//!   are in object storage under the build named in the record, uploaded
//!   before the record is written
//! - `(pisa, <version>, "child", <parent>, <m>)`: empty; `m` imports `parent`
//! - `(pisa, <version>, "ref", <m>, <e>)`: empty; entry `e` points at `m`
//! - `(pisa, <version>, "entry", <e>)`: entry `e` ([`EntryMeta`])
//! - `(pisa, <version>, "key", <k>)`: the id of the entry made with
//!   idempotency key `k`
//! - `(pisa, <version>, "new", <R>, <e>)`: empty; every entry, newest first.
//!   `R` is its creation time in microseconds counted down from `u64::MAX`,
//!   as 16 hex digits, and `R` followed by `e` is a listing cursor
//! - `(pisa, <version>, "label", <key>, <value>, <R>, <e>)`: empty; one per
//!   label of entry `e`, newest first
//! - `(pisa, <version>, "decls", <m>)`: module `m`'s declarations for the
//!   search service ([`SnapshotDecls`]), written with the module
//! - `(pisa, <version>, "described", <m>, <name>)`: the search service's
//!   description and vector of a declaration of `m`, which only it reads
//! - `(pisa, <version>, "lease", <name>)`: a [`Lease`] held by one process,
//!   e.g. the search replica that indexes snapshots
//! - `(pisa, <version>, "log", <T>, <id>)`: a [`Change`]; one per transaction
//!   that changes an entry or a module, for the search service to follow.
//!   `T` is the time in microseconds as 16 hex digits, `id` random. Kept for
//!   [`LOG_RETENTION`]
//!
//! A module is kept while an entry points at it or a kept module imports it.
//! Each transaction that relies on a `mod` record it only reads locks it, so
//! that it conflicts with one that removes the module. A transaction that adds
//! an entry with a key reads the key first and writes it with the entry, so
//! that two creates with the same key conflict instead of both succeeding.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use pisa_protocol::api::{CheckResult, Error, ErrorKind};
use pisa_protocol::labels::check_labels;
use pisa_protocol::snapshot;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::kv::{self, IsConflict, Kv, KvError, ROOT, Txn, decode, key, prefix};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleMeta {
    /// Ids of the modules it imports.
    pub imports: Vec<String>,
    /// Every module it needs, its own last.
    pub modules: Vec<Module>,
    /// See [`SnapshotResponse::header`](pisa_protocol::api::SnapshotResponse::header).
    pub header: String,
    pub code: String,
    pub dirty: bool,
    /// See [`SnapshotResponse::verified`](pisa_protocol::api::SnapshotResponse::verified).
    #[serde(default)]
    pub verified: bool,
    /// Its code or anything it imports uses macros, which can expand in code
    /// on top of it.
    #[serde(default)]
    pub macros: bool,
    pub result: CheckResult,
    pub created: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub id: String,
    /// A random name for the build that made these files, so that the files
    /// of the same module built again never share a path with these.
    pub build: String,
    /// The `.olean` last: it is written last, because Lean imports a
    /// module-system `.olean` without its other files as an empty module.
    pub files: Vec<ModuleFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleFile {
    /// What follows `s<id>.` in the file's name, e.g. `olean.server`.
    pub ext: String,
    pub sha256: String,
}

/// A snapshot entry: a labelled reference to a module.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EntryMeta {
    /// The id of the module it points at.
    pub module: String,
    pub labels: BTreeMap<String, String>,
    /// The idempotency key it was made with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Microseconds since the Unix epoch.
    pub created_micros: u64,
}

impl EntryMeta {
    /// The `R` component of the entry's `new` and `label` keys.
    fn rank(&self) -> String {
        format!("{:016x}", u64::MAX - self.created_micros)
    }
}

/// One declaration of a module, as the search service indexes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDecl {
    pub name: String,
    pub kind: String,
    /// The declaration's signature, with notation.
    pub signature: String,
    /// The type with full names and no notation.
    pub type_pp_full: String,
    #[serde(default)]
    pub type_refs: Vec<String>,
    #[serde(default)]
    pub value_refs: Vec<String>,
    #[serde(default)]
    pub docstring: Option<String>,
    #[serde(default)]
    pub deprecated_to: Option<String>,
    /// Depends on `sorry`, by the module's check result.
    #[serde(default)]
    pub uses_sorry: bool,
}

/// A module's declarations for the search service, with the module's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDecls {
    /// See [`ModuleMeta::header`].
    pub header: String,
    pub dirty: bool,
    pub verified: bool,
    pub decls: Vec<SnapshotDecl>,
}

impl SnapshotDecls {
    /// `decls` for the module `meta`, flagged from its check result.
    pub fn new(meta: &ModuleMeta, decls: Vec<SnapshotDecl>) -> SnapshotDecls {
        let mut out = SnapshotDecls {
            header: meta.header.clone(),
            dirty: false,
            verified: false,
            decls,
        };
        out.refresh(meta);
        out
    }

    /// Takes `dirty`, `verified` and each declaration's `uses_sorry` from `meta`.
    fn refresh(&mut self, meta: &ModuleMeta) {
        self.dirty = meta.dirty;
        self.verified = meta.verified;
        for d in &mut self.decls {
            d.uses_sorry = meta
                .result
                .declarations
                .iter()
                .any(|r| r.name == d.name && r.axioms.iter().any(|a| a == "sorryAx"));
        }
    }
}

/// What a transaction changed, by id: readers look up the current state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

/// Who holds a lease, and until when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub owner: String,
    /// Microseconds since the Unix epoch.
    pub until_micros: u64,
}

/// How long [`Change`]s are kept; see [`Catalog::prune_log`].
pub const LOG_RETENTION: Duration = Duration::from_secs(24 * 3600);

/// The most `log` keys one [`Catalog::prune_log`] transaction deletes.
const PRUNE_BATCH: u32 = 1000;

/// What a new entry is given.
pub struct NewEntry {
    pub labels: BTreeMap<String, String>,
    pub key: Option<String>,
}

/// The most bytes of JSON a record can have. The TiKV client reads up to
/// 16 MiB in one message.
const MAX_RECORD_BYTES: usize = 6 * 1024 * 1024;

/// The most index keys one listing reads; see [`Catalog::list`].
const MAX_SCAN: usize = 10_000;

/// The fewest keys a listing scans at once.
const SCAN_CHUNK: usize = 256;

pub struct Catalog {
    kv: Kv,
    version: String,
    /// [`MAX_SCAN`], except in tests.
    max_scan: usize,
}

/// Why a transaction attempt failed: the store, which [`kv::retry`] runs
/// again on a conflict, or the request.
enum Fail {
    Kv(KvError),
    Api(Error),
}

impl From<KvError> for Fail {
    fn from(e: KvError) -> Self {
        Fail::Kv(e)
    }
}

impl From<serde_json::Error> for Fail {
    fn from(e: serde_json::Error) -> Self {
        Fail::Api(internal(e))
    }
}

impl IsConflict for Fail {
    fn is_conflict(&self) -> bool {
        matches!(self, Fail::Kv(e) if e.is_conflict())
    }
}

impl From<Fail> for Error {
    fn from(f: Fail) -> Self {
        match f {
            Fail::Kv(e) => e.into(),
            Fail::Api(e) => e,
        }
    }
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, e.to_string())
}

/// Whether `value`'s JSON fits in a record.
pub fn record_fits(value: &impl Serialize) -> bool {
    serde_json::to_vec(value).is_ok_and(|b| b.len() <= MAX_RECORD_BYTES)
}

/// A record's JSON, refused as a `bad_request` if it is over
/// [`MAX_RECORD_BYTES`]: it could be written but never read back.
fn to_record(value: &impl Serialize) -> Result<Vec<u8>, Fail> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Fail::Api(Error::new(
            ErrorKind::BadRequest,
            format!(
                "snapshot record too large: {} bytes, limit 6 MiB",
                bytes.len()
            ),
        )));
    }
    Ok(bytes)
}

/// The error for a module that isn't kept.
pub fn unknown_module(m: &str) -> Error {
    Error::new(
        ErrorKind::NotFound,
        format!("unknown snapshot {}", snapshot::module(m)),
    )
}

fn unknown_entry(e: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("unknown snapshot entry {e}"))
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

impl Catalog {
    pub fn new(kv: Kv, version: String) -> Catalog {
        Catalog {
            kv,
            version,
            max_scan: MAX_SCAN,
        }
    }

    /// This catalog with listings that read at most `max_scan` index keys.
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_max_scan(self, max_scan: usize) -> Catalog {
        Catalog { max_scan, ..self }
    }

    fn mod_key(&self, m: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "mod", m])
    }

    fn child_key(&self, parent: &str, m: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "child", parent, m])
    }

    fn ref_key(&self, m: &str, e: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "ref", m, e])
    }

    fn entry_key(&self, e: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "entry", e])
    }

    fn idem_key(&self, k: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "key", k])
    }

    fn new_key(&self, r: &str, e: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "new", r, e])
    }

    fn label_key(&self, k: &str, value: &str, r: &str, e: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "label", k, value, r, e])
    }

    fn decls_key(&self, m: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "decls", m])
    }

    fn described_key(&self, m: &str, name: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "described", m, name])
    }

    fn log_key(&self, micros: u64, id: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "log", &format!("{micros:016x}"), id])
    }

    /// Records in `txn` that it changes entry `entry` and/or module `module`.
    async fn log(
        &self,
        txn: &mut Txn,
        entry: Option<&str>,
        module: Option<&str>,
    ) -> Result<(), Fail> {
        let change = Change {
            entry: entry.map(str::to_string),
            module: module.map(str::to_string),
        };
        let id = uuid::Uuid::new_v4().simple().to_string();
        txn.put(
            self.log_key(now_micros(), &id),
            serde_json::to_vec(&change)?,
        )
        .await?;
        Ok(())
    }

    /// Reads one JSON record.
    async fn read<T: DeserializeOwned>(&self, key: &[u8]) -> Result<Option<T>, Error> {
        let value = kv::retry(|| async { self.kv.begin().await?.get(key).await }).await?;
        value
            .map(|v| serde_json::from_slice(&v).map_err(internal))
            .transpose()
    }

    pub async fn module(&self, m: &str) -> Result<ModuleMeta, Error> {
        self.read(&self.mod_key(m))
            .await?
            .ok_or_else(|| unknown_module(m))
    }

    /// Rewrites module `m`'s record with `f` if the module's own build is
    /// still `build`. `None` if it isn't, because the module was deleted and
    /// built again: the record is then left alone.
    pub async fn update_module(
        &self,
        m: &str,
        build: &str,
        f: impl Fn(&mut ModuleMeta),
    ) -> Result<Option<ModuleMeta>, Error> {
        let mod_key = self.mod_key(m);
        let updated = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            let Some(bytes) = txn.get(&mod_key).await? else {
                return Err(Fail::Api(unknown_module(m)));
            };
            let mut meta: ModuleMeta = serde_json::from_slice(&bytes)?;
            if meta.modules.last().map(|own| own.build.as_str()) != Some(build) {
                return Ok(None);
            }
            f(&mut meta);
            txn.put(mod_key.clone(), to_record(&meta)?).await?;
            let decls_key = self.decls_key(m);
            if let Some(bytes) = txn.get(&decls_key).await? {
                let mut decls: SnapshotDecls = serde_json::from_slice(&bytes)?;
                decls.refresh(&meta);
                txn.put(decls_key, to_record(&decls)?).await?;
            }
            self.log(&mut txn, None, Some(m)).await?;
            txn.commit().await?;
            Ok(Some(meta))
        })
        .await?;
        Ok(updated)
    }

    /// The entry made with idempotency key `k`, if there is one.
    pub async fn keyed(&self, k: &str) -> Result<Option<(String, EntryMeta)>, Error> {
        let found = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            self.keyed_in(&mut txn, k).await
        })
        .await?;
        Ok(found)
    }

    /// [`keyed`](Self::keyed) in `txn`.
    async fn keyed_in(&self, txn: &mut Txn, k: &str) -> Result<Option<(String, EntryMeta)>, Fail> {
        let Some(id) = txn.get(&self.idem_key(k)).await? else {
            return Ok(None);
        };
        let e = String::from_utf8(id).map_err(|e| Fail::Api(internal(e)))?;
        let Some(bytes) = txn.get(&self.entry_key(&e)).await? else {
            return Err(Fail::Api(internal(format!(
                "idempotency key {k} names the missing entry {e}"
            ))));
        };
        Ok(Some((e, serde_json::from_slice(&bytes)?)))
    }

    /// Stores a freshly built module (unless it exists) with its child links,
    /// plus a new entry on it. Returns the entry and the module's stored
    /// record, which is another worker's if it stored the module first. A
    /// missing parent is `not_found`, and so is one deleted and built again
    /// since `meta` was made from it, whose files `meta` would name.
    ///
    /// If `entry`'s key is already on an entry, nothing is written and that
    /// entry comes back, unchanged. On the same module, with its stored
    /// record; on another, with `meta` in place of a record, which callers
    /// tell by the entry's `module`. A `meta` or `decls` too large to store
    /// is a `bad_request`, and nothing is written either. `decls` is stored
    /// with the module, if this stores it.
    pub async fn commit(
        &self,
        m: &str,
        meta: &ModuleMeta,
        decls: Option<&SnapshotDecls>,
        entry: NewEntry,
    ) -> Result<(String, EntryMeta, ModuleMeta), Error> {
        let record = to_record(meta)?;
        let decls = decls.map(to_record).transpose()?;
        let mod_key = self.mod_key(m);
        let added = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            if let Some(k) = &entry.key
                && let Some((e, taken)) = self.keyed_in(&mut txn, k).await?
            {
                let stored = if taken.module == m {
                    let Some(bytes) = txn.get(&mod_key).await? else {
                        return Err(Fail::Api(unknown_module(m)));
                    };
                    serde_json::from_slice(&bytes)?
                } else {
                    meta.clone()
                };
                return Ok((e, taken, stored));
            }
            for parent in &meta.imports {
                let parent_key = self.mod_key(parent);
                txn.lock(parent_key.clone()).await?;
                let Some(bytes) = txn.get(&parent_key).await? else {
                    return Err(Fail::Api(unknown_module(parent)));
                };
                let parent_meta: ModuleMeta = serde_json::from_slice(&bytes)?;
                if !parent_meta.modules.iter().all(|p| meta.modules.contains(p)) {
                    return Err(Fail::Api(Error::new(
                        ErrorKind::NotFound,
                        format!(
                            "{} was deleted and built again while this snapshot was being built",
                            snapshot::module(parent)
                        ),
                    )));
                }
            }
            // Another worker may have just built the same code: keep theirs.
            txn.lock(mod_key.clone()).await?;
            let stored = match txn.get(&mod_key).await? {
                Some(bytes) => serde_json::from_slice(&bytes)?,
                None => {
                    txn.put(mod_key.clone(), record.clone()).await?;
                    for parent in &meta.imports {
                        txn.put(self.child_key(parent, m), Vec::new()).await?;
                    }
                    if let Some(decls) = &decls {
                        txn.put(self.decls_key(m), decls.clone()).await?;
                    }
                    meta.clone()
                }
            };
            let (e, entry_meta) = self.add(&mut txn, m, &entry).await?;
            self.log(&mut txn, Some(&e), Some(m)).await?;
            txn.commit().await?;
            Ok((e, entry_meta, stored))
        })
        .await?;
        Ok(added)
    }

    /// A new entry on an existing module; `None` if the module is gone. If
    /// `entry`'s key is already on an entry, that one comes back instead,
    /// unchanged, and it may be on another module.
    pub async fn add_entry(
        &self,
        m: &str,
        entry: NewEntry,
    ) -> Result<Option<(String, EntryMeta)>, Error> {
        let mod_key = self.mod_key(m);
        let added = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            if let Some(k) = &entry.key
                && let Some(taken) = self.keyed_in(&mut txn, k).await?
            {
                return Ok(Some(taken));
            }
            txn.lock(mod_key.clone()).await?;
            if txn.get(&mod_key).await?.is_none() {
                return Ok(None);
            }
            let added = self.add(&mut txn, m, &entry).await?;
            self.log(&mut txn, Some(&added.0), Some(m)).await?;
            txn.commit().await?;
            Ok::<_, Fail>(Some(added))
        })
        .await?;
        Ok(added)
    }

    /// Writes a new entry on module `m`, which `txn` has locked and found, and
    /// its key, which `txn` has found unused.
    async fn add(
        &self,
        txn: &mut Txn,
        m: &str,
        entry: &NewEntry,
    ) -> Result<(String, EntryMeta), Fail> {
        let e = uuid::Uuid::new_v4().simple().to_string();
        let meta = EntryMeta {
            module: m.to_string(),
            labels: entry.labels.clone(),
            key: entry.key.clone(),
            created_micros: now_micros(),
        };
        txn.put(self.entry_key(&e), to_record(&meta)?).await?;
        txn.put(self.ref_key(m, &e), Vec::new()).await?;
        if let Some(k) = &entry.key {
            txn.put(self.idem_key(k), e.clone().into_bytes()).await?;
        }
        let r = meta.rank();
        txn.put(self.new_key(&r, &e), Vec::new()).await?;
        for (k, value) in &meta.labels {
            txn.put(self.label_key(k, value, &r, &e), Vec::new())
                .await?;
        }
        Ok((e, meta))
    }

    pub async fn entry(&self, e: &str) -> Result<EntryMeta, Error> {
        self.read(&self.entry_key(e))
            .await?
            .ok_or_else(|| unknown_entry(e))
    }

    /// Removes the entry; returns its module's record if that module was
    /// cleaned up too.
    pub async fn remove_entry(&self, e: &str) -> Result<(EntryMeta, Option<ModuleMeta>), Error> {
        let entry_key = self.entry_key(e);
        let removed = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            let Some(bytes) = txn.get(&entry_key).await? else {
                return Err(Fail::Api(unknown_entry(e)));
            };
            let meta: EntryMeta = serde_json::from_slice(&bytes)?;
            self.remove(&mut txn, e, &meta).await?;
            let module = self.clean(&mut txn, &meta.module).await?;
            self.log(&mut txn, Some(e), Some(&meta.module)).await?;
            txn.commit().await?;
            Ok((meta, module))
        })
        .await?;
        Ok(removed)
    }

    /// Deletes entry `e` and every key that points at it.
    async fn remove(&self, txn: &mut Txn, e: &str, meta: &EntryMeta) -> Result<(), Fail> {
        txn.delete(self.entry_key(e)).await?;
        txn.delete(self.ref_key(&meta.module, e)).await?;
        if let Some(k) = &meta.key {
            txn.delete(self.idem_key(k)).await?;
        }
        let r = meta.rank();
        txn.delete(self.new_key(&r, e)).await?;
        for (k, value) in &meta.labels {
            txn.delete(self.label_key(k, value, &r, e)).await?;
        }
        Ok(())
    }

    /// The entries, newest first, that have every `(key, value)` in `labels`
    /// (every entry if there are none), at most `limit` of them, starting
    /// after the cursor `after` (see [`is_cursor`]). Also returns the cursor
    /// to go on from unless the range ran out: the last entry's if there are
    /// `limit`, else the last key's it read before reaching [`MAX_SCAN`]. So
    /// a page can be short, even empty, and still have one.
    ///
    /// It scans the first label's keys, at least 256 at a time, and looks up
    /// the others' for each entry found, all in one read transaction. Every
    /// key scanned or looked up counts towards [`MAX_SCAN`], and an entry
    /// that reaches it is still looked up to the end.
    ///
    /// [`is_cursor`]: pisa_protocol::labels::is_cursor
    pub async fn list(
        &self,
        labels: &[(String, String)],
        limit: usize,
        after: Option<&str>,
    ) -> Result<(Vec<(String, EntryMeta)>, Option<String>), Error> {
        let v = self.version.as_str();
        let (scanned, others) = match labels.split_first() {
            Some(((k, value), others)) => (vec![ROOT, v, "label", k, value], others),
            None => (vec![ROOT, v, "new"], labels),
        };
        let mut start = prefix(&scanned);
        if let Some(after) = after {
            let (r, e) = after.split_at(16);
            let cursor = key(&[scanned.as_slice(), &[r, e]].concat());
            start.start = [cursor, vec![0]].concat();
        }
        let chunk = limit.max(SCAN_CHUNK);
        let listed = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            let mut range = start.clone();
            let mut entries = Vec::new();
            let mut budget = self.max_scan;
            let mut last = None;
            loop {
                if budget == 0 {
                    return Ok((entries, last));
                }
                let wanted = chunk.min(budget);
                let page = txn.scan(range.clone(), wanted as u32).await?;
                let exhausted = page.len() < wanted;
                'scanned: for (found, _) in page {
                    if budget == 0 {
                        return Ok((entries, last));
                    }
                    budget -= 1;
                    range.start = [found.as_slice(), &[0]].concat();
                    let parts = decode(&found);
                    let [.., r, e] = parts.as_slice() else {
                        continue;
                    };
                    last = Some(format!("{r}{e}"));
                    for (k, value) in others {
                        budget = budget.saturating_sub(1);
                        if txn.get(&self.label_key(k, value, r, e)).await?.is_none() {
                            continue 'scanned;
                        }
                    }
                    let Some(bytes) = txn.get(&self.entry_key(e)).await? else {
                        return Err(Fail::Api(internal(format!(
                            "the listing index names the missing entry {e}"
                        ))));
                    };
                    entries.push((e.clone(), serde_json::from_slice(&bytes)?));
                    if entries.len() == limit {
                        return Ok((entries, last));
                    }
                }
                if exhausted {
                    return Ok((entries, None));
                }
            }
        })
        .await?;
        Ok(listed)
    }

    /// Removes the labels `remove` names from entry `e`, then sets those in
    /// `set`, and returns the entry. The labels that result must follow the
    /// rules, or nothing changes and it is a `bad_request`.
    pub async fn set_labels(
        &self,
        e: &str,
        set: &BTreeMap<String, String>,
        remove: &[String],
    ) -> Result<EntryMeta, Error> {
        let entry_key = self.entry_key(e);
        let updated = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            let Some(bytes) = txn.get(&entry_key).await? else {
                return Err(Fail::Api(unknown_entry(e)));
            };
            let mut meta: EntryMeta = serde_json::from_slice(&bytes)?;
            let old = meta.labels.clone();
            for k in remove {
                meta.labels.remove(k);
            }
            meta.labels.extend(set.clone());
            check_labels(&meta.labels)
                .map_err(|m| Fail::Api(Error::new(ErrorKind::BadRequest, m)))?;
            let r = meta.rank();
            for (k, value) in &old {
                if meta.labels.get(k) != Some(value) {
                    txn.delete(self.label_key(k, value, &r, e)).await?;
                }
            }
            for (k, value) in &meta.labels {
                if old.get(k) != Some(value) {
                    txn.put(self.label_key(k, value, &r, e), Vec::new()).await?;
                }
            }
            // Rewriting the record also makes a concurrent delete conflict.
            txn.put(entry_key.clone(), to_record(&meta)?).await?;
            self.log(&mut txn, Some(e), Some(&meta.module)).await?;
            txn.commit().await?;
            Ok(meta)
        })
        .await?;
        Ok(updated)
    }

    /// Removes module `m` if no entry references it and no module imports it.
    pub async fn cleanup(&self, m: &str) -> Result<Option<ModuleMeta>, Error> {
        let removed = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            let removed = self.clean(&mut txn, m).await?;
            if removed.is_some() {
                self.log(&mut txn, None, Some(m)).await?;
            }
            txn.commit().await?;
            Ok::<_, Fail>(removed)
        })
        .await?;
        Ok(removed)
    }

    /// [`cleanup`](Self::cleanup) in `txn`. It locks `mod/m` even when it keeps
    /// the module, so that two transactions that each remove one of the last
    /// two references conflict instead of both keeping it.
    async fn clean(&self, txn: &mut Txn, m: &str) -> Result<Option<ModuleMeta>, Fail> {
        let mod_key = self.mod_key(m);
        txn.lock(mod_key.clone()).await?;
        let Some(bytes) = txn.get(&mod_key).await? else {
            return Ok(None);
        };
        let v = self.version.as_str();
        let referenced = !txn.scan(prefix(&[ROOT, v, "ref", m]), 1).await?.is_empty();
        let imported = !txn
            .scan(prefix(&[ROOT, v, "child", m]), 1)
            .await?
            .is_empty();
        if referenced || imported {
            return Ok(None);
        }
        let meta: ModuleMeta = serde_json::from_slice(&bytes)?;
        txn.delete(mod_key).await?;
        for parent in &meta.imports {
            txn.delete(self.child_key(parent, m)).await?;
        }
        txn.delete(self.decls_key(m)).await?;
        let mut described = prefix(&[ROOT, v, "described", m]);
        loop {
            let page = txn.scan(described.clone(), PRUNE_BATCH).await?;
            for (k, _) in &page {
                txn.delete(k.clone()).await?;
            }
            match page.last() {
                Some((last, _)) if page.len() == PRUNE_BATCH as usize => {
                    described.start = [last.as_slice(), &[0]].concat();
                }
                _ => break,
            }
        }
        Ok(Some(meta))
    }

    /// Deletes the [`Change`]s older than `older_than`, a batch per
    /// transaction; returns how many.
    pub async fn prune_log(&self, older_than: Duration) -> Result<usize, Error> {
        let cutoff = now_micros().saturating_sub(older_than.as_micros() as u64);
        let mut start = key(&[ROOT, &self.version, "log"]);
        let end = key(&[ROOT, &self.version, "log", &format!("{cutoff:016x}")]);
        let mut deleted = 0;
        loop {
            let (batch, last) = kv::retry(|| async {
                let mut txn = self.kv.begin().await?;
                let old = txn.scan(start.clone()..end.clone(), PRUNE_BATCH).await?;
                for (k, _) in &old {
                    txn.delete(k.clone()).await?;
                }
                txn.commit().await?;
                Ok::<_, KvError>((old.len(), old.last().map(|(k, _)| k.clone())))
            })
            .await?;
            deleted += batch;
            if batch < PRUNE_BATCH as usize {
                return Ok(deleted);
            }
            // Scanning from the start again would step over every tombstone.
            if let Some(last) = last {
                start = [last, vec![0]].concat();
            }
        }
    }

    /// Up to `limit` keys of kind `kind` (e.g. `"entry"`) with their values,
    /// in key order, after `after` (a key a previous page returned). Each
    /// key comes decoded, without the root, version and kind.
    pub async fn scan(
        &self,
        kind: &str,
        after: Option<&[u8]>,
        limit: u32,
    ) -> Result<Vec<(Vec<u8>, Vec<String>, Vec<u8>)>, Error> {
        let mut range = prefix(&[ROOT, &self.version, kind]);
        if let Some(after) = after {
            range.start = [after, &[0]].concat();
        }
        let page = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            txn.scan(range.clone(), limit).await
        })
        .await?;
        Ok(page
            .into_iter()
            .map(|(k, v)| {
                let parts = decode(&k).split_off(3);
                (k, parts, v)
            })
            .collect())
    }

    /// The [`Change`]s logged at `since_micros` or later, oldest first, at most
    /// `limit` of them, after the key `after` from a previous page.
    pub async fn changes(
        &self,
        since_micros: u64,
        after: Option<&[u8]>,
        limit: u32,
    ) -> Result<Vec<(Vec<u8>, Change)>, Error> {
        let v = self.version.as_str();
        let mut range = prefix(&[ROOT, v, "log"]);
        range.start = match after {
            Some(after) => [after, &[0]].concat(),
            None => key(&[ROOT, v, "log", &format!("{since_micros:016x}")]),
        };
        let page = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            txn.scan(range.clone(), limit).await
        })
        .await?;
        page.into_iter()
            .map(|(k, v)| Ok((k, serde_json::from_slice(&v).map_err(internal)?)))
            .collect()
    }

    /// Takes or renews lease `name` for `owner` for `ttl`, unless another
    /// owner holds it; returns when it ends if `owner` holds it now.
    pub async fn take_lease(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<Option<u64>, Error> {
        let lease_key = key(&[ROOT, &self.version, "lease", name]);
        let taken = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            let now = now_micros();
            if let Some(bytes) = txn.get(&lease_key).await? {
                let held: Lease = serde_json::from_slice(&bytes)?;
                if held.owner != owner && held.until_micros > now {
                    return Ok(None);
                }
            }
            let lease = Lease {
                owner: owner.to_string(),
                until_micros: now + ttl.as_micros() as u64,
            };
            txn.put(lease_key.clone(), serde_json::to_vec(&lease)?)
                .await?;
            txn.commit().await?;
            Ok::<_, Fail>(Some(lease.until_micros))
        })
        .await?;
        Ok(taken)
    }

    /// Whether any entry is on module `m`.
    pub async fn has_entries(&self, m: &str) -> Result<bool, Error> {
        let refs = prefix(&[ROOT, &self.version, "ref", m]);
        let found = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            Ok::<_, KvError>(!txn.scan(refs.clone(), 1).await?.is_empty())
        })
        .await?;
        Ok(found)
    }

    /// Entry `e`, if it exists.
    pub async fn find_entry(&self, e: &str) -> Result<Option<EntryMeta>, Error> {
        self.read(&self.entry_key(e)).await
    }

    /// Module `m`'s declarations for search, if it is kept and has them.
    pub async fn decls(&self, m: &str) -> Result<Option<SnapshotDecls>, Error> {
        self.read(&self.decls_key(m)).await
    }

    /// The search service's records for module `m`'s declarations, by name.
    pub async fn described(&self, m: &str) -> Result<Vec<(String, Vec<u8>)>, Error> {
        let range = prefix(&[ROOT, &self.version, "described", m]);
        let found = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            let mut range = range.clone();
            let mut found = Vec::new();
            loop {
                let page = txn.scan(range.clone(), PRUNE_BATCH).await?;
                let full = page.len() == PRUNE_BATCH as usize;
                if let Some((last, _)) = page.last() {
                    range.start = [last.as_slice(), &[0]].concat();
                }
                found.extend(page);
                if !full {
                    return Ok::<_, KvError>(found);
                }
            }
        })
        .await?;
        Ok(found
            .into_iter()
            .filter_map(|(k, v)| Some((decode(&k).pop()?, v)))
            .collect())
    }

    /// Stores the search service's records for declarations of module `m`,
    /// by name, unless the module is gone; returns whether it stored them.
    pub async fn put_described(
        &self,
        m: &str,
        records: &[(String, Vec<u8>)],
    ) -> Result<bool, Error> {
        let mod_key = self.mod_key(m);
        let stored = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            txn.lock(mod_key.clone()).await?;
            if txn.get(&mod_key).await?.is_none() {
                return Ok(false);
            }
            for (name, record) in records {
                txn.put(self.described_key(m, name), record.clone()).await?;
            }
            txn.commit().await?;
            Ok::<_, KvError>(true)
        })
        .await?;
        Ok(stored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::verify::Declaration;

    fn meta(id: &str) -> ModuleMeta {
        ModuleMeta {
            imports: Vec::new(),
            modules: vec![Module {
                id: id.into(),
                build: "b".into(),
                files: Vec::new(),
            }],
            header: "import PisaSnap.sx".into(),
            code: String::new(),
            dirty: false,
            verified: false,
            macros: false,
            result: CheckResult::default(),
            created: 0,
        }
    }

    fn decl(name: &str) -> SnapshotDecl {
        SnapshotDecl {
            name: name.into(),
            kind: "theorem".into(),
            signature: format!("{name} : True"),
            type_pp_full: "True".into(),
            type_refs: Vec::new(),
            value_refs: Vec::new(),
            docstring: None,
            deprecated_to: None,
            uses_sorry: false,
        }
    }

    fn no_labels() -> NewEntry {
        NewEntry {
            labels: BTreeMap::new(),
            key: None,
        }
    }

    #[tokio::test]
    async fn a_recheck_refreshes_the_declarations_and_is_logged() {
        let catalog = Catalog::new(Kv::memory(), "v".into());
        let m = meta("x");
        let decls = SnapshotDecls::new(&m, vec![decl("a"), decl("b")]);
        catalog
            .commit("x", &m, Some(&decls), no_labels())
            .await
            .unwrap();
        let before = catalog.changes(0, None, 100).await.unwrap().len();

        catalog
            .update_module("x", "b", |meta| {
                meta.verified = true;
                meta.dirty = true;
                meta.result.declarations = vec![Declaration {
                    name: "b".into(),
                    kind: "theorem".into(),
                    axioms: vec!["sorryAx".into()],
                    type_hash: String::new(),
                    value_hash: None,
                }];
            })
            .await
            .unwrap();
        let decls = catalog.decls("x").await.unwrap().unwrap();
        assert!(decls.verified && decls.dirty);
        assert!(!decls.decls[0].uses_sorry && decls.decls[1].uses_sorry);
        let changes = catalog.changes(0, None, 100).await.unwrap();
        assert_eq!(changes.len(), before + 1);
        let last = &changes.last().unwrap().1;
        assert_eq!(
            (last.entry.as_deref(), last.module.as_deref()),
            (None, Some("x"))
        );
    }

    #[tokio::test]
    async fn changes_page_in_order_and_prune() {
        let catalog = Catalog::new(Kv::memory(), "v".into());
        for id in ["x", "y", "z"] {
            catalog
                .commit(id, &meta(id), None, no_labels())
                .await
                .unwrap();
        }
        let modules = |page: &[(Vec<u8>, Change)]| -> Vec<String> {
            page.iter().filter_map(|(_, c)| c.module.clone()).collect()
        };
        let first = catalog.changes(0, None, 2).await.unwrap();
        assert_eq!(modules(&first), ["x", "y"]);
        let rest = catalog.changes(0, Some(&first[1].0), 2).await.unwrap();
        assert_eq!(modules(&rest), ["z"]);
        // Nothing is logged in the future.
        assert!(
            catalog
                .changes(now_micros() + 1_000_000, None, 10)
                .await
                .unwrap()
                .is_empty()
        );

        assert_eq!(
            catalog.prune_log(Duration::from_secs(3600)).await.unwrap(),
            0
        );
        assert_eq!(catalog.prune_log(Duration::ZERO).await.unwrap(), 3);
        assert!(catalog.changes(0, None, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_lease_is_held_by_one_owner_until_it_ends() {
        let catalog = Catalog::new(Kv::memory(), "v".into());
        let ttl = Duration::from_secs(30);
        assert!(catalog.take_lease("l", "a", ttl).await.unwrap().is_some());
        assert!(catalog.take_lease("l", "b", ttl).await.unwrap().is_none());
        // Renewing is taking it again.
        assert!(catalog.take_lease("l", "a", ttl).await.unwrap().is_some());
        // An ended lease goes to whoever asks.
        catalog.take_lease("l", "a", Duration::ZERO).await.unwrap();
        assert!(catalog.take_lease("l", "b", ttl).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn whether_a_module_has_entries() {
        let catalog = Catalog::new(Kv::memory(), "v".into());
        let (e, _, _) = catalog
            .commit("x", &meta("x"), None, no_labels())
            .await
            .unwrap();
        assert!(catalog.has_entries("x").await.unwrap());
        assert!(!catalog.has_entries("y").await.unwrap());
        catalog.add_entry("x", no_labels()).await.unwrap();
        catalog.remove_entry(&e).await.unwrap();
        assert!(catalog.has_entries("x").await.unwrap());
    }

    #[tokio::test]
    async fn scans_page_through_a_kind() {
        let catalog = Catalog::new(Kv::memory(), "v".into());
        for id in ["x", "y", "z"] {
            let m = meta(id);
            let decls = SnapshotDecls::new(&m, vec![decl("a")]);
            catalog
                .commit(id, &m, Some(&decls), no_labels())
                .await
                .unwrap();
        }
        let first = catalog.scan("decls", None, 2).await.unwrap();
        let ids: Vec<&str> = first
            .iter()
            .map(|(_, parts, _)| parts[0].as_str())
            .collect();
        assert_eq!(ids, ["x", "y"]);
        let rest = catalog.scan("decls", Some(&first[1].0), 2).await.unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].1, ["z"]);
    }
}
