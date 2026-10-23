//! Environment snapshots: code compiled to a Lean module that later code
//! imports, kept in object storage with its check result so that any worker
//! of the version can import it.
//!
//! Layout, per pool version:
//! - `snapshots/<version>/<id>.<ext>.zst`: the module's files, zstd-compressed:
//!   its `.olean`, and for module-system code also `.olean.server`,
//!   `.olean.private`, `.ir` and `.ir.sig`
//! - `snapshots/<version>/<id>.json`: [`SnapshotMeta`]. Written last, so a
//!   snapshot exists once its metadata does.
//! - `snapshots/<version>/<id>.children/<child id>`: an empty marker for each
//!   snapshot that imports it, so deletes can cascade
//!
//! Workers keep the modules they import in a local cache on every Lean
//! process's search path. A cached snapshot is imported only while a lookup
//! in the store vouches for it (`PISA_SNAPSHOT_CONFIRM_SECS`): a module that
//! is still being built can't be imported, and a deleted one stops working
//! everywhere within that time, without a store request on every check.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use futures::StreamExt;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pisa_protocol::api::{
    CheckResult, Declaration, Deleted, Error, ErrorKind, SnapshotInfo, SnapshotResponse,
};
use pisa_protocol::header::{Split, header_key, split_header};
use pisa_protocol::repl::CommandResponse;
use pisa_protocol::snapshot;
use pisa_protocol::wire::{CheckOutcome, SnapshotCreateJob};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::check::{header_error, policy_error};

use crate::pool::{Lease, Pool};

/// Time allowed for the report on a snapshot's declarations.
const REPORT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    /// Ids of the snapshots it imports.
    pub imports: Vec<String>,
    /// Every module it needs, its own last.
    pub modules: Vec<Module>,
    /// See [`SnapshotResponse::header`].
    pub header: String,
    pub code: String,
    pub dirty: bool,
    pub result: CheckResult,
    pub created: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub id: String,
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

pub struct Snapshots {
    store: Arc<dyn ObjectStore>,
    version: String,
    /// Holds `PisaSnap/s<id>.<ext>`.
    root: PathBuf,
    confirm_every: Duration,
    /// The files of each module in the local cache.
    cached: Mutex<HashMap<String, Vec<ModuleFile>>>,
    /// When the store last confirmed that each snapshot exists.
    confirmed: Mutex<HashMap<String, Instant>>,
    /// One task at a time loads or builds a given snapshot.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, e.to_string())
}

fn not_found(id: &str) -> Error {
    Error::new(
        ErrorKind::NotFound,
        format!("unknown snapshot {}", snapshot::module(id)),
    )
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The files `lean -o` wrote for module `s<id>` in `dir`, by extension, the
/// `.olean` last.
fn outputs(dir: &Path, id: &str) -> std::io::Result<Vec<(String, PathBuf)>> {
    let prefix = format!("s{id}.");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(ext) = name.strip_prefix(&prefix)
            && ext != "lean"
        {
            files.push((ext.to_string(), path.clone()));
        }
    }
    files.sort_by_key(|(ext, _)| (ext == "olean", ext.clone()));
    Ok(files)
}

/// Removes every file of module `s<id>` from `dir`.
fn remove_module(dir: &Path, id: &str) {
    let prefix = format!("s{id}.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Snapshots {
    /// `root` is the search-path root of snapshot modules; it must exist and
    /// contain an empty `PisaSnap/`. A lookup in the store vouches for a
    /// cached snapshot for `confirm_every`.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        version: String,
        root: PathBuf,
        confirm_every: Duration,
    ) -> Self {
        Snapshots {
            store,
            version,
            root,
            confirm_every,
            cached: Mutex::default(),
            confirmed: Mutex::default(),
            locks: Mutex::default(),
        }
    }

    fn object(&self, name: &str) -> ObjectPath {
        ObjectPath::from(format!("snapshots/{}/{name}", self.version))
    }

    fn dir(&self) -> PathBuf {
        self.root.join(snapshot::ROOT)
    }

    fn file(&self, id: &str, ext: &str) -> PathBuf {
        self.dir().join(format!("s{id}.{ext}"))
    }

    fn lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    async fn meta(&self, id: &str) -> Result<SnapshotMeta, Error> {
        let bytes = match self.store.get(&self.object(&format!("{id}.json"))).await {
            Ok(r) => r.bytes().await.map_err(internal)?,
            Err(object_store::Error::NotFound { .. }) => return Err(not_found(id)),
            Err(e) => return Err(internal(e)),
        };
        serde_json::from_slice(&bytes).map_err(internal)
    }

    /// Makes every snapshot `header` imports importable by this worker's Lean
    /// processes, except `pending`, which the caller is building.
    pub async fn ensure(&self, header: &str, pending: Option<&str>) -> Result<(), Error> {
        let ids = snapshot::imports(header).map_err(|e| Error::new(ErrorKind::BadRequest, e))?;
        for id in ids.iter().filter(|id| Some(id.as_str()) != pending) {
            self.ensure_one(id).await?;
        }
        Ok(())
    }

    async fn ensure_one(&self, id: &str) -> Result<(), Error> {
        let lock = self.lock(id);
        let _loading = lock.lock().await;
        let fresh = self
            .confirmed
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|t| t.elapsed() < self.confirm_every);
        if fresh {
            return Ok(());
        }
        let meta = match self.meta(id).await {
            Ok(meta) => meta,
            Err(e) => {
                if e.kind == ErrorKind::NotFound {
                    self.discard(id);
                }
                return Err(e);
            }
        };
        for module in &meta.modules {
            self.fetch(module).await?;
        }
        self.confirmed
            .lock()
            .unwrap()
            .insert(id.to_string(), Instant::now());
        Ok(())
    }

    /// Downloads a module into the local cache unless it is already there.
    async fn fetch(&self, module: &Module) -> Result<(), Error> {
        let cached = self.cached.lock().unwrap().get(&module.id).cloned();
        if cached.as_ref() == Some(&module.files) {
            return Ok(());
        }
        for file in &module.files {
            let path = self.object(&format!("{}.{}.zst", module.id, file.ext));
            let compressed = match self.store.get(&path).await {
                Ok(r) => r.bytes().await.map_err(internal)?,
                Err(object_store::Error::NotFound { .. }) => return Err(not_found(&module.id)),
                Err(e) => return Err(internal(e)),
            };
            let bytes = zstd::decode_all(&compressed[..]).map_err(internal)?;
            if sha256(&bytes) != file.sha256 {
                return Err(internal(format!(
                    "snapshot {} is corrupt in storage",
                    snapshot::module(&module.id)
                )));
            }
            // Lean processes may have the old file mapped: replace it, don't
            // overwrite it.
            let dest = self.file(&module.id, &file.ext);
            let tmp = self.file(
                &module.id,
                &format!("tmp.{}", uuid::Uuid::new_v4().simple()),
            );
            tokio::fs::write(&tmp, &bytes).await.map_err(internal)?;
            tokio::fs::rename(&tmp, &dest).await.map_err(internal)?;
        }
        self.cached
            .lock()
            .unwrap()
            .insert(module.id.clone(), module.files.clone());
        Ok(())
    }

    /// Forgets a snapshot on this worker. Processes that imported it keep it.
    fn discard(&self, id: &str) {
        remove_module(&self.dir(), id);
        self.cached.lock().unwrap().remove(id);
        self.confirmed.lock().unwrap().remove(id);
    }

    fn response(&self, id: &str, meta: SnapshotMeta) -> SnapshotResponse {
        SnapshotResponse {
            snapshot: Some(snapshot::module(id)),
            header: Some(meta.header),
            version: self.version.clone(),
            dirty: meta.dirty,
            result: meta.result,
        }
    }

    /// Checks `job.code` by compiling it to a module and keeps the module
    /// unless Lean reported errors. Failures are reported in `result`.
    pub async fn create(&self, pool: &Arc<Pool>, job: SnapshotCreateJob) -> SnapshotResponse {
        let mut response = SnapshotResponse {
            snapshot: None,
            header: None,
            version: self.version.clone(),
            dirty: false,
            result: CheckResult::default(),
        };
        let split = split_header(&job.code);
        if let Err(e) = snapshot::imports(&split.header) {
            response.result.error = Some(Error::new(ErrorKind::BadRequest, e));
            return response;
        }
        // Macros are allowed: the build is re-checked through the kernel.
        let rejected = pool.policy().scan(&split.body, true);
        if !rejected.is_empty() {
            response.result.error = Some(policy_error(&rejected));
            response.result.reasons = rejected;
            return response;
        }
        let id = snapshot::id(&job.code);
        let lock = self.lock(&id);
        let _building = lock.lock().await;
        match self.meta(&id).await {
            Ok(meta) => return self.response(&id, meta),
            Err(e) if e.kind == ErrorKind::NotFound => {}
            Err(e) => {
                response.result.error = Some(e);
                return response;
            }
        }
        let header = snapshot::next_header(&split.header, &id);
        let mut slot = pool.acquire(&header_key(&header), None).await;
        let built = self.build(&mut slot, &job, &split, &id, &header).await;
        slot.finish_job();
        match built {
            Ok(Some(meta)) => self.response(&id, meta),
            Ok(None) => response,
            Err(outcome) => {
                response.result = (*outcome).into();
                response
            }
        }
    }

    /// Compiles, judges and stores the snapshot. Returns its metadata, `None`
    /// if nothing could be checked (the cause is in the result), or the
    /// outcome of a check that failed.
    async fn build(
        &self,
        slot: &mut Lease,
        job: &SnapshotCreateJob,
        split: &Split,
        id: &str,
        header: &str,
    ) -> Result<Option<SnapshotMeta>, Box<CheckOutcome>> {
        let mut outcome = CheckOutcome {
            time: 0.0,
            error: None,
            response: None,
            verification: None,
            warm: false,
        };
        let fail = |mut outcome: CheckOutcome, error: Error| {
            outcome.error = Some(error);
            Err(Box::new(outcome))
        };
        if let Err(e) = self.ensure(&split.header, None).await {
            return fail(outcome, e);
        }
        // `lean -o` imports everything again, so free the slot's memory first.
        slot.retire_and_wait("evict").await;

        let root = slot.scratch_dir().join("build");
        let dir = root.join(snapshot::ROOT);
        let file = dir.join(format!("s{id}.lean"));
        let olean = file.with_extension("olean");
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            return fail(outcome, internal(e));
        }
        if let Err(e) = tokio::fs::write(&file, &job.code).await {
            return fail(outcome, internal(e));
        }
        let limit = slot.import_timeout() + Duration::from_secs(job.timeout_secs);
        let start = Instant::now();
        let compiled = slot.compile(&root, &file, &olean, limit).await;
        outcome.time = (start.elapsed().as_secs_f64() * 1e6).round() / 1e6;
        let compiled = match compiled {
            Ok(c) => c,
            Err(e) => {
                remove_module(&dir, id);
                return fail(outcome, internal(format!("failed to start Lean: {e}")));
            }
        };
        // The file is the submitted code, so positions need no shifting.
        let response = CommandResponse {
            env: 0,
            messages: compiled.messages,
            sorries: Vec::new(),
            tactics: Vec::new(),
            infotree: None,
        };
        let errors = response.has_errors();
        outcome.response = Some(response);
        let built = match compiled.status {
            None => Err(Error::new(
                ErrorKind::Timeout,
                format!("building took more than {}s", limit.as_secs()),
            )),
            Some(_) if compiled.oom_kills > 0 => Err(Error::new(
                ErrorKind::OutOfMemory,
                "Lean exceeded its memory limit",
            )),
            Some(status) if status.success() || errors => Ok(()),
            Some(status) => Err(Error::new(
                ErrorKind::Crash,
                format!("Lean failed ({status}): {}", compiled.stderr.trim()),
            )),
        };
        if let Err(e) = built {
            remove_module(&dir, id);
            return fail(outcome, e);
        }
        if errors {
            remove_module(&dir, id);
            let judged = slot
                .policy()
                .judge(outcome.response.as_ref().unwrap(), None);
            outcome.verification = Some(judged);
            return Err(Box::new(outcome));
        }

        // Judge it in the slot that will serve code on top of it.
        let installed = self.install(id, &dir).await;
        remove_module(&dir, id);
        let files = match installed {
            Ok(files) => files,
            Err(e) => {
                self.discard(id);
                return fail(outcome, e);
            }
        };
        // Re-check the module through the kernel in a clean process, and take
        // its axioms from there: the verdict then doesn't depend on whatever
        // ran during the build.
        let verified = slot.verify(&snapshot::module(id), REPORT_TIMEOUT).await;
        if !verified.ok {
            self.unbuild(slot, id);
            let reason = verified
                .reason
                .unwrap_or_else(|| "snapshot rejected".into());
            return fail(outcome, Error::new(ErrorKind::Lean, reason));
        }
        let Ok(decls) = serde_json::from_value::<Vec<Declaration>>(verified.declarations) else {
            self.unbuild(slot, id);
            return fail(outcome, internal("unreadable verifier declarations"));
        };
        let verification = slot.policy().judge_decls(false, Some(decls));
        let dirty = !verification.ok;
        outcome.verification = Some(verification);

        // Keep the builder warm on the snapshot so the next checks are hits.
        if let Err(f) = slot
            .warm_up_with(header, &header_key(header), Some(id))
            .await
        {
            self.unbuild(slot, id);
            return fail(outcome, header_error(f, slot.import_timeout()));
        }

        let own = files.iter().map(|(f, _)| f.clone()).collect();
        let modules = match self.modules(&split.header, id, own).await {
            Ok(m) => m,
            Err(e) => {
                self.unbuild(slot, id);
                return fail(outcome, e);
            }
        };
        let meta = SnapshotMeta {
            imports: snapshot::imports(&split.header).unwrap_or_default(),
            modules,
            header: header.to_string(),
            code: job.code.clone(),
            dirty,
            result: outcome.clone().into(),
            created: now(),
        };
        if let Err(e) = self.commit(id, &files, &meta).await {
            self.unbuild(slot, id);
            return fail(outcome, e);
        }
        Ok(Some(meta))
    }

    /// Moves a freshly built module's files from `dir` into the local cache,
    /// not yet confirmed, and returns them with their contents.
    async fn install(&self, id: &str, dir: &Path) -> Result<Vec<(ModuleFile, Vec<u8>)>, Error> {
        let outputs = outputs(dir, id).map_err(internal)?;
        if !outputs.iter().any(|(ext, _)| ext == "olean") {
            return Err(internal("Lean wrote no .olean"));
        }
        let mut files = Vec::new();
        for (ext, path) in outputs {
            let bytes = tokio::fs::read(&path).await.map_err(internal)?;
            tokio::fs::rename(&path, self.file(id, &ext))
                .await
                .map_err(internal)?;
            let sha256 = sha256(&bytes);
            files.push((ModuleFile { ext, sha256 }, bytes));
        }
        let cached = files.iter().map(|(f, _)| f.clone()).collect();
        self.cached.lock().unwrap().insert(id.to_string(), cached);
        Ok(files)
    }

    /// Undoes [`install`](Self::install) after a failure, including the slot
    /// that imported the module.
    fn unbuild(&self, slot: &mut Lease, id: &str) {
        slot.retire("snapshot_failed");
        self.discard(id);
    }

    /// Every module a snapshot needs: its imports' modules, then its own.
    async fn modules(
        &self,
        header: &str,
        id: &str,
        files: Vec<ModuleFile>,
    ) -> Result<Vec<Module>, Error> {
        let mut modules: Vec<Module> = Vec::new();
        for import in snapshot::imports(header).map_err(internal)? {
            for m in self.meta(&import).await?.modules {
                if !modules.contains(&m) {
                    modules.push(m);
                }
            }
        }
        modules.push(Module {
            id: id.to_string(),
            files,
        });
        Ok(modules)
    }

    async fn commit(
        &self,
        id: &str,
        files: &[(ModuleFile, Vec<u8>)],
        meta: &SnapshotMeta,
    ) -> Result<(), Error> {
        let put = |name: String, bytes: Vec<u8>| async move {
            self.store
                .put(&self.object(&name), PutPayload::from(bytes))
                .await
                .map_err(internal)
        };
        for (file, bytes) in files {
            let compressed = zstd::encode_all(&bytes[..], 3).map_err(internal)?;
            put(format!("{id}.{}.zst", file.ext), compressed).await?;
        }
        put(
            format!("{id}.json"),
            serde_json::to_vec(meta).map_err(internal)?,
        )
        .await?;
        for parent in &meta.imports {
            put(format!("{parent}.children/{id}"), Vec::new()).await?;
        }
        self.confirmed
            .lock()
            .unwrap()
            .insert(id.to_string(), Instant::now());
        Ok(())
    }

    fn parse(&self, name: &str) -> Result<String, Error> {
        match snapshot::parse_module(name) {
            Ok(Some(id)) => Ok(id),
            Ok(None) | Err(_) => Err(Error::new(
                ErrorKind::BadRequest,
                format!("{name} is not a snapshot module"),
            )),
        }
    }

    pub async fn get(&self, name: &str) -> Result<SnapshotInfo, Error> {
        let id = self.parse(name)?;
        let meta = self.meta(&id).await?;
        Ok(SnapshotInfo {
            snapshot: snapshot::module(&id),
            imports: meta.imports.iter().map(|i| snapshot::module(i)).collect(),
            header: meta.header,
            code: meta.code,
            dirty: meta.dirty,
            result: meta.result,
            created: meta.created,
        })
    }

    /// Deletes a snapshot and every snapshot that imports it.
    pub async fn delete(&self, name: &str) -> Result<Deleted, Error> {
        let root = self.parse(name)?;
        self.meta(&root).await?;
        let mut deleted = Vec::new();
        let mut queue = vec![root];
        while let Some(id) = queue.pop() {
            let markers = self.object(&format!("{id}.children"));
            let mut children = self.store.list(Some(&markers));
            while let Some(child) = children.next().await {
                let child = child.map_err(internal)?;
                if let Some(child_id) = child.location.filename() {
                    queue.push(child_id.to_string());
                }
                self.delete_object(&child.location).await?;
            }
            let Ok(meta) = self.meta(&id).await else {
                continue;
            };
            // Metadata first: the snapshot stops existing before its parts go.
            self.delete_object(&self.object(&format!("{id}.json")))
                .await?;
            let own = meta.modules.iter().filter(|m| m.id == id);
            for file in own.flat_map(|m| &m.files) {
                self.delete_object(&self.object(&format!("{id}.{}.zst", file.ext)))
                    .await?;
            }
            for parent in &meta.imports {
                self.delete_object(&self.object(&format!("{parent}.children/{id}")))
                    .await?;
            }
            self.discard(&id);
            deleted.push(snapshot::module(&id));
        }
        Ok(Deleted { deleted })
    }

    async fn delete_object(&self, path: &ObjectPath) -> Result<(), Error> {
        match self.store.delete(path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(internal(e)),
        }
    }
}
