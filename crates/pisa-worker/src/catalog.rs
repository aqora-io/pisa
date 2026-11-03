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
//!
//! A module is kept while an entry points at it or a kept module imports it.
//! Each transaction that relies on a `mod` record it only reads locks it, so
//! that it conflicts with one that removes the module.

use std::collections::BTreeMap;
use std::time::SystemTime;

use pisa_protocol::api::{CheckResult, Error, ErrorKind};
use pisa_protocol::snapshot;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::kv::{self, IsConflict, Kv, KvError, ROOT, Txn, key, prefix};

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
    /// Microseconds since the Unix epoch.
    pub created_micros: u64,
}

/// What a new entry is given.
pub struct NewEntry {
    pub labels: BTreeMap<String, String>,
}

pub struct Catalog {
    kv: Kv,
    version: String,
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
            Fail::Kv(e) => internal(e),
            Fail::Api(e) => e,
        }
    }
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, e.to_string())
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
        Catalog { kv, version }
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

    /// Reads one JSON record.
    async fn read<T: DeserializeOwned>(&self, key: &[u8]) -> Result<Option<T>, Error> {
        let value = kv::retry(|| async { self.kv.begin().await?.get(key).await })
            .await
            .map_err(internal)?;
        value
            .map(|v| serde_json::from_slice(&v).map_err(internal))
            .transpose()
    }

    pub async fn module(&self, m: &str) -> Result<ModuleMeta, Error> {
        self.read(&self.mod_key(m))
            .await?
            .ok_or_else(|| unknown_module(m))
    }

    /// Rewrites module `m`'s record with `f`.
    pub async fn update_module(
        &self,
        m: &str,
        f: impl Fn(&mut ModuleMeta),
    ) -> Result<ModuleMeta, Error> {
        let mod_key = self.mod_key(m);
        let updated = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            let Some(bytes) = txn.get(&mod_key).await? else {
                return Err(Fail::Api(unknown_module(m)));
            };
            let mut meta: ModuleMeta = serde_json::from_slice(&bytes)?;
            f(&mut meta);
            txn.put(mod_key.clone(), serde_json::to_vec(&meta)?).await?;
            txn.commit().await?;
            Ok(meta)
        })
        .await?;
        Ok(updated)
    }

    /// Stores a freshly built module (unless it exists) with its child links,
    /// plus a new entry on it. Returns the entry and the module's stored
    /// record, which is another worker's if it stored the module first. A
    /// missing parent is `not_found`, and so is one deleted and built again
    /// since `meta` was made from it, whose files `meta` would name.
    pub async fn commit(
        &self,
        m: &str,
        meta: &ModuleMeta,
        entry: NewEntry,
    ) -> Result<(String, EntryMeta, ModuleMeta), Error> {
        let record = serde_json::to_vec(meta).map_err(internal)?;
        let mod_key = self.mod_key(m);
        let added = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
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
                    meta.clone()
                }
            };
            let (e, entry_meta) = self.add(&mut txn, m, &entry).await?;
            txn.commit().await?;
            Ok((e, entry_meta, stored))
        })
        .await?;
        Ok(added)
    }

    /// A new entry on an existing module; `None` if the module is gone.
    pub async fn add_entry(
        &self,
        m: &str,
        entry: NewEntry,
    ) -> Result<Option<(String, EntryMeta)>, Error> {
        let mod_key = self.mod_key(m);
        let added = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            txn.lock(mod_key.clone()).await?;
            if txn.get(&mod_key).await?.is_none() {
                return Ok(None);
            }
            let added = self.add(&mut txn, m, &entry).await?;
            txn.commit().await?;
            Ok::<_, Fail>(Some(added))
        })
        .await?;
        Ok(added)
    }

    /// Writes a new entry on module `m`, which `txn` has locked and found.
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
            created_micros: now_micros(),
        };
        txn.put(self.entry_key(&e), serde_json::to_vec(&meta)?)
            .await?;
        txn.put(self.ref_key(m, &e), Vec::new()).await?;
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
        Ok(())
    }

    /// Removes module `m` if no entry references it and no module imports it.
    pub async fn cleanup(&self, m: &str) -> Result<Option<ModuleMeta>, Error> {
        let removed = kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            let removed = self.clean(&mut txn, m).await?;
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
        Ok(Some(meta))
    }
}
