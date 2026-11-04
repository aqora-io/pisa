//! Environment snapshots: code compiled to a Lean module that later code
//! imports, its files kept in object storage so that any worker of the
//! version can import it. Entries keep modules alive: the same code is one
//! module, however many entries point at it, and a module goes once no entry
//! or kept module needs it. The records, with each module's check result, are
//! in the [`Catalog`], and so are the idempotency keys that make a create safe
//! to retry.
//!
//! Object storage, per pool version:
//! - `snapshots/<version>/<id>.<build>.<ext>.zst`: the module's files,
//!   zstd-compressed: its `.olean`, and for module-system code also
//!   `.olean.server`, `.olean.private`, `.ir` and `.ir.sig`. `build` is random
//!   per build and named in the module's record. The files are uploaded
//!   before the record is written, so a module exists once its record does,
//!   and a delete removes only the files of the record it removed: the same
//!   code built again meanwhile has files of its own.
//!
//! Workers keep the modules they import in a local cache on every Lean
//! process's search path. A cached snapshot is imported only while a lookup
//! in the store vouches for it (`PISA_SNAPSHOT_CONFIRM_SECS`): a module that
//! is still being built can't be imported, and a deleted one stops working
//! everywhere within that time, without a store request on every check.
//!
//! The kernel re-check is separate from building: it runs at creation for code
//! that uses macros or asks for it, and later on request. A module's record
//! is rewritten once it passes.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pisa_protocol::api::{
    CheckResult, Declaration, Deleted, Error, ErrorKind, SnapshotFailure, SnapshotInfo,
    SnapshotList, SnapshotResponse, SnapshotSummary, SnapshotVerified,
};
use pisa_protocol::header::{Split, header_key, split_header};
use pisa_protocol::labels::{
    MAX_LABELS, MAX_LIMIT, check_key, check_labels, entry_id_error, is_cursor, is_entry_id,
};
use pisa_protocol::repl::{CommandResponse, ReplCommand, ReplResponse, Severity};
use pisa_protocol::snapshot;
use pisa_protocol::verify::Verification;
use pisa_protocol::wire::{CheckOutcome, SnapshotCreateJob};
use sha2::{Digest, Sha256};

use crate::catalog::{
    Catalog, EntryMeta, Module, ModuleFile, ModuleMeta, NewEntry, SnapshotDecl, SnapshotDecls,
    record_fits, unknown_module,
};
use crate::check::{header_error, policy_error, run, unexpected};
use crate::db::Db;
use crate::policy::{module_export, module_report, parse_export, parse_report, uses_macros};
use crate::pool::{Lease, Pool};

/// Time allowed for the report on a snapshot's declarations.
const REPORT_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Snapshots {
    store: Arc<dyn ObjectStore>,
    catalog: Catalog,
    version: String,
    /// Holds `PisaSnap/s<id>.<ext>`.
    root: PathBuf,
    confirm_every: Duration,
    /// The build of each module in the local cache.
    cached: Mutex<HashMap<String, Module>>,
    /// When the store last confirmed that each snapshot exists.
    confirmed: Mutex<HashMap<String, Instant>>,
    /// One task at a time loads or builds a given snapshot.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, e.to_string())
}

/// The error for a create whose idempotency key is on an entry of other code.
fn key_in_use() -> Error {
    Error::new(
        ErrorKind::BadRequest,
        "idempotency key already used for different code",
    )
}

/// Checks that a path segment is an entry id.
fn parse_entry(entry: &str) -> Result<&str, Error> {
    if is_entry_id(entry) {
        Ok(entry)
    } else {
        Err(Error::new(ErrorKind::BadRequest, entry_id_error(entry)))
    }
}

/// An entry with its module's state.
fn summary(entry: String, entry_meta: EntryMeta, meta: &ModuleMeta) -> SnapshotSummary {
    SnapshotSummary {
        entry,
        snapshot: snapshot::module(&entry_meta.module),
        imports: meta.imports.iter().map(|i| snapshot::module(i)).collect(),
        header: meta.header.clone(),
        labels: entry_meta.labels,
        dirty: meta.dirty,
        verified: meta.verified,
        created: entry_meta.created_micros / 1_000_000,
    }
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

/// What re-checking one module of a chain came to.
#[derive(Debug, PartialEq)]
enum Recheck {
    Passed,
    AlreadyVerified,
    /// The module was deleted and built again since the chain was read, so
    /// this run doesn't verify it.
    Changed,
    Failed(String),
}

/// Every module a snapshot needs: its imports' modules, then its own.
fn modules(parents: &[ModuleMeta], own: Module) -> Vec<Module> {
    let mut modules: Vec<Module> = Vec::new();
    for m in parents.iter().flat_map(|p| &p.modules) {
        if !modules.contains(m) {
            modules.push(m.clone());
        }
    }
    modules.push(own);
    modules
}

/// The error for a create that asked to verify an import or existing snapshot
/// and couldn't.
fn failure_error(f: &SnapshotFailure) -> Error {
    Error::new(
        ErrorKind::Lean,
        format!("{} failed verification: {}", f.snapshot, f.reason),
    )
}

/// Re-checks the module through the kernel in a clean process, and returns its
/// declarations with their axioms from there, so that they don't depend on
/// whatever ran during the build. The error is why the re-check failed.
async fn recheck_module(slot: &Lease, id: &str) -> Result<Vec<Declaration>, String> {
    let verified = slot.verify(&snapshot::module(id), REPORT_TIMEOUT).await;
    if !verified.ok {
        return Err(verified
            .reason
            .unwrap_or_else(|| "snapshot rejected".into()));
    }
    serde_json::from_value(verified.declarations)
        .map_err(|_| "unreadable verifier declarations".to_string())
}

/// Runs one of the worker's own reports in environment `env` and parses it;
/// `what` names it in the error.
async fn run_report<T>(
    slot: &mut Lease,
    text: String,
    env: u64,
    what: &str,
    parse: impl Fn(&CommandResponse) -> Option<T>,
) -> Result<T, Error> {
    let cmd = ReplCommand::cmd(text, Some(env));
    let report = match run(slot, &cmd, REPORT_TIMEOUT).await? {
        ReplResponse::Command(r) => r,
        ReplResponse::Error(e) => return Err(Error::new(ErrorKind::Lean, e.message)),
        other => return Err(unexpected(other)),
    };
    parse(&report).ok_or_else(|| {
        let why = report
            .messages
            .iter()
            .find(|m| m.severity == Severity::Error)
            .map_or("no output".to_string(), |m| m.data.clone());
        internal(format!("the {what} failed: {why}"))
    })
}

/// The declarations of a module in a process that has it imported, with their
/// axioms as that process reports them.
async fn report_module(slot: &mut Lease, id: &str, env: u64) -> Result<Vec<Declaration>, Error> {
    let text = module_report(&snapshot::module(id));
    run_report(slot, text, env, "axiom report", parse_report).await
}

/// What the search service indexes of a module's declarations, from a
/// process that has it imported.
async fn export_module(slot: &mut Lease, id: &str, env: u64) -> Result<Vec<SnapshotDecl>, Error> {
    let text = module_export(&snapshot::module(id));
    run_report(slot, text, env, "declaration export", parse_export).await
}

impl Snapshots {
    /// `root` is the search-path root of snapshot modules; it must exist and
    /// contain an empty `PisaSnap/`. A lookup in the store vouches for a
    /// cached snapshot for `confirm_every`.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        db: Db,
        version: String,
        root: PathBuf,
        confirm_every: Duration,
    ) -> Self {
        Snapshots {
            store,
            catalog: Catalog::new(db, version.clone()),
            version,
            root,
            confirm_every,
            cached: Mutex::default(),
            confirmed: Mutex::default(),
            locks: Mutex::default(),
        }
    }

    /// Where the store keeps file `ext` of a module build.
    fn blob(&self, module: &Module, ext: &str) -> ObjectPath {
        ObjectPath::from(format!(
            "snapshots/{}/{}.{}.{ext}.zst",
            self.version, module.id, module.build
        ))
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
        let meta = match self.catalog.module(id).await {
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

    /// Downloads a module build into the local cache unless it is already
    /// there.
    async fn fetch(&self, module: &Module) -> Result<(), Error> {
        let cached = self.cached.lock().unwrap().get(&module.id).cloned();
        if cached.as_ref() == Some(module) {
            return Ok(());
        }
        for file in &module.files {
            let path = self.blob(module, &file.ext);
            let compressed = match self.store.get(&path).await {
                Ok(r) => r.bytes().await.map_err(internal)?,
                Err(object_store::Error::NotFound { .. }) => {
                    return Err(unknown_module(&module.id));
                }
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
            .insert(module.id.clone(), module.clone());
        Ok(())
    }

    /// Forgets a snapshot on this worker. Processes that imported it keep it.
    fn discard(&self, id: &str) {
        remove_module(&self.dir(), id);
        self.cached.lock().unwrap().remove(id);
        self.confirmed.lock().unwrap().remove(id);
    }

    /// Forgets a module that a delete removed, unless the same code has been
    /// stored again since, maybe by a build on this worker. Then the local
    /// copy may be the removed build, so the next import looks the module up
    /// again.
    async fn forget(&self, id: &str) {
        let lock = self.lock(id);
        let _held = lock.lock().await;
        if self.catalog.module(id).await.is_err() {
            self.discard(id);
        } else {
            self.confirmed.lock().unwrap().remove(id);
        }
    }

    /// Deletes a module build's files from the store, logging any it can't.
    async fn delete_blobs(&self, module: &Module) {
        for file in &module.files {
            let path = self.blob(module, &file.ext);
            match self.store.delete(&path).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => tracing::warn!(%path, "deleting a snapshot file failed: {e}"),
            }
        }
    }

    /// A kept module, with the entry that this create added to it, if any.
    fn response(
        &self,
        id: &str,
        meta: ModuleMeta,
        entry: Option<(String, EntryMeta)>,
    ) -> SnapshotResponse {
        let (entry, labels) =
            entry.map_or((None, Default::default()), |(e, m)| (Some(e), m.labels));
        SnapshotResponse {
            entry,
            snapshot: Some(snapshot::module(id)),
            header: Some(meta.header),
            version: self.version.clone(),
            dirty: meta.dirty,
            verified: meta.verified,
            labels,
            result: meta.result,
        }
    }

    /// Checks `job.code` by compiling it to a module and keeps the module
    /// unless Lean reported errors, with a new entry on it. Code that is
    /// already kept gets the new entry without being built again. Failures
    /// are reported in `result`.
    pub async fn create(&self, pool: &Arc<Pool>, job: SnapshotCreateJob) -> SnapshotResponse {
        let mut response = SnapshotResponse {
            entry: None,
            snapshot: None,
            header: None,
            version: self.version.clone(),
            dirty: false,
            verified: false,
            labels: Default::default(),
            result: CheckResult::default(),
        };
        let valid =
            check_labels(&job.labels).and_then(|()| job.key.as_deref().map_or(Ok(()), check_key));
        if let Err(e) = valid {
            response.result.error = Some(Error::new(ErrorKind::BadRequest, e));
            return response;
        }
        let split = split_header(&job.code);
        if let Err(e) = snapshot::imports(&split.header) {
            response.result.error = Some(Error::new(ErrorKind::BadRequest, e));
            return response;
        }
        // Macros are allowed: `build` re-checks them through the kernel unless
        // the request opts out, which marks the snapshot as not verified.
        let rejected = pool.policy().scan(&split.body, true);
        if !rejected.is_empty() {
            response.result.error = Some(policy_error(&rejected));
            response.result.reasons = rejected;
            return response;
        }
        if let Some(key) = &job.key {
            match self.replay(key, &snapshot::id(&job.code)).await {
                Ok(Some(done)) => return done,
                Ok(None) => {}
                Err(e) => {
                    response.result.error = Some(e);
                    return response;
                }
            }
        }
        // A kept module removed before its new entry was added is built
        // again, once.
        for _ in 0..2 {
            if let Some(done) = self.create_once(pool, &job, &split, response.clone()).await {
                return done;
            }
        }
        response.result.error = Some(internal(
            "the snapshot was deleted twice while it was being created",
        ));
        response
    }

    /// The response to a create whose idempotency key an entry already has:
    /// that entry, unchanged, if it is on module `id`, else an error. `None` if
    /// no entry has the key.
    async fn replay(&self, key: &str, id: &str) -> Result<Option<SnapshotResponse>, Error> {
        let Some((entry, entry_meta)) = self.catalog.keyed(key).await? else {
            return Ok(None);
        };
        if entry_meta.module != id {
            return Err(key_in_use());
        }
        let meta = self.catalog.module(id).await?;
        Ok(Some(self.response(id, meta, Some((entry, entry_meta)))))
    }

    /// [`create`](Self::create) after validation. `None` if the module was
    /// kept but removed before the entry could be added, or during the
    /// re-check the request asked for.
    async fn create_once(
        &self,
        pool: &Arc<Pool>,
        job: &SnapshotCreateJob,
        split: &Split,
        mut response: SnapshotResponse,
    ) -> Option<SnapshotResponse> {
        let id = snapshot::id(&job.code);
        let lock = self.lock(&id);
        let building = lock.lock().await;
        match self.catalog.module(&id).await {
            Ok(meta) => {
                drop(building);
                return self.add_to_kept(pool, &id, meta, job, response).await;
            }
            Err(e) if e.kind == ErrorKind::NotFound => {}
            Err(e) => {
                response.result.error = Some(e);
                return Some(response);
            }
        }
        if job.verify == Some(true) {
            for import in snapshot::imports(&split.header).unwrap_or_default() {
                let error = match self.verify_chain(pool, &import).await {
                    Ok((_, Some(failure))) => failure_error(&failure),
                    Ok((_, None)) => continue,
                    Err(e) => e,
                };
                response.result.error = Some(error);
                return Some(response);
            }
        }
        let header = snapshot::next_header(&split.header, &id);
        let mut slot = pool.acquire(&header_key(&header), None).await;
        let built = self.build(&mut slot, job, split, &id, &header).await;
        slot.finish_job();
        Some(match built {
            Ok(Some((meta, entry))) => self.response(&id, meta, Some(entry)),
            Ok(None) => response,
            Err(outcome) => {
                response.result = (*outcome).into();
                response
            }
        })
    }

    /// A create for code that is already kept: it verifies the module's chain
    /// first if the request asks for that and the module isn't verified, then
    /// adds the entry. Its result is what is stored. If the re-check fails,
    /// it has the error and no entry is added, as for new code that fails it.
    /// `None` if the module is gone by the time it is needed, or was built
    /// again during the re-check.
    async fn add_to_kept(
        &self,
        pool: &Arc<Pool>,
        id: &str,
        meta: ModuleMeta,
        job: &SnapshotCreateJob,
        mut response: SnapshotResponse,
    ) -> Option<SnapshotResponse> {
        let meta = if job.verify == Some(true) && !meta.verified {
            let verified = async {
                let (_, failure) = self.verify_chain(pool, id).await?;
                Ok::<_, Error>((self.catalog.module(id).await?, failure))
            };
            match verified.await {
                // Nothing failed, so the chain stopped at a module built again.
                Ok((meta, None)) if !meta.verified => return None,
                Ok((meta, None)) => meta,
                Ok((meta, Some(failure))) => {
                    let mut response = self.response(id, meta, None);
                    response.result.ok = false;
                    response.result.error = Some(failure_error(&failure));
                    return Some(response);
                }
                Err(e) => {
                    let gone = e.kind == ErrorKind::NotFound
                        && matches!(self.catalog.module(id).await, Err(e) if e.kind == ErrorKind::NotFound);
                    if gone {
                        return None;
                    }
                    response.result.error = Some(e);
                    return Some(response);
                }
            }
        } else {
            meta
        };
        let entry = NewEntry {
            labels: job.labels.clone(),
            key: job.key.clone(),
        };
        let added = match self.catalog.add_entry(id, entry).await {
            Ok(Some(added)) => added,
            Ok(None) => return None,
            Err(e) => {
                response.result.error = Some(e);
                return Some(response);
            }
        };
        // Another create took the key for other code since `create` looked.
        if added.1.module != id {
            response.result.error = Some(key_in_use());
            return Some(response);
        }
        Some(self.response(id, meta, Some(added)))
    }

    /// Compiles, judges and stores the snapshot with a new entry. Returns its
    /// record and the entry, `None` if nothing could be checked (the cause is
    /// in the result), or the outcome of a check that failed.
    async fn build(
        &self,
        slot: &mut Lease,
        job: &SnapshotCreateJob,
        split: &Split,
        id: &str,
        header: &str,
    ) -> Result<Option<(ModuleMeta, (String, EntryMeta))>, Box<CheckOutcome>> {
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
        let parents = match self.parents(&split.header).await {
            Ok(parents) => parents,
            Err(e) => return fail(outcome, e),
        };
        let macros = uses_macros(&split.body) || parents.iter().any(|p| p.macros);
        let recheck = job.verify.unwrap_or(macros);
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
        let build = uuid::Uuid::new_v4().simple().to_string();
        let installed = self.install(id, build, &dir).await;
        remove_module(&dir, id);
        let (own, contents) = match installed {
            Ok(installed) => installed,
            Err(e) => {
                self.discard(id);
                return fail(outcome, e);
            }
        };
        let rechecked = if recheck {
            match recheck_module(slot, id).await {
                Ok(decls) => Some(decls),
                Err(reason) => {
                    self.unbuild(slot, id);
                    return fail(outcome, Error::new(ErrorKind::Lean, reason));
                }
            }
        } else {
            None
        };

        // Keep the builder warm on the snapshot so the next checks are hits.
        let env = match slot
            .warm_up_with(header, &header_key(header), Some(id))
            .await
        {
            Ok((env, _)) => env,
            Err(f) => {
                self.unbuild(slot, id);
                return fail(outcome, header_error(f, slot.import_timeout()));
            }
        };
        let decls = match rechecked {
            Some(decls) => decls,
            None => match report_module(slot, id, env).await {
                Ok(decls) => decls,
                Err(e) => {
                    self.unbuild(slot, id);
                    return fail(outcome, e);
                }
            },
        };
        // For search only, so a failure just leaves the module out of it.
        let exported = export_module(slot, id, env)
            .await
            .inspect_err(|e| tracing::warn!(module = %id, "exporting declarations failed: {e}"))
            .ok();
        let verification = slot.policy().judge_decls(false, Some(decls));
        let dirty = !verification.ok;
        outcome.verification = Some(verification);

        let meta = ModuleMeta {
            imports: snapshot::imports(&split.header).unwrap_or_default(),
            modules: modules(&parents, own.clone()),
            header: header.to_string(),
            code: job.code.clone(),
            dirty,
            verified: recheck && parents.iter().all(|p| p.verified),
            macros,
            result: outcome.clone().into(),
            created: now(),
        };
        let search = exported
            .map(|decls| SnapshotDecls::new(&meta, decls))
            .filter(|d| {
                record_fits(d) || {
                    tracing::warn!(
                        module = %id,
                        "declarations too large, or with a NUL character, to index for search"
                    );
                    false
                }
            });
        let entry = NewEntry {
            labels: job.labels.clone(),
            key: job.key.clone(),
        };
        match self
            .commit(&own, &contents, &meta, search.as_ref(), entry)
            .await
        {
            Ok(stored) => Ok(Some(stored)),
            Err(e) => {
                self.unbuild(slot, id);
                fail(outcome, e)
            }
        }
    }

    /// Moves a freshly built module's files from `dir` into the local cache,
    /// not yet confirmed, and returns the build with the files' contents.
    async fn install(
        &self,
        id: &str,
        build: String,
        dir: &Path,
    ) -> Result<(Module, Vec<Vec<u8>>), Error> {
        let outputs = outputs(dir, id).map_err(internal)?;
        if !outputs.iter().any(|(ext, _)| ext == "olean") {
            return Err(internal("Lean wrote no .olean"));
        }
        let mut files = Vec::new();
        let mut contents = Vec::new();
        for (ext, path) in outputs {
            let bytes = tokio::fs::read(&path).await.map_err(internal)?;
            tokio::fs::rename(&path, self.file(id, &ext))
                .await
                .map_err(internal)?;
            files.push(ModuleFile {
                ext,
                sha256: sha256(&bytes),
            });
            contents.push(bytes);
        }
        let module = Module {
            id: id.to_string(),
            build,
            files,
        };
        self.cached
            .lock()
            .unwrap()
            .insert(id.to_string(), module.clone());
        Ok((module, contents))
    }

    /// Undoes [`install`](Self::install) after a failure, including the slot
    /// that imported the module.
    fn unbuild(&self, slot: &mut Lease, id: &str) {
        slot.retire("snapshot_failed");
        self.discard(id);
    }

    /// The records of the modules `header` imports.
    async fn parents(&self, header: &str) -> Result<Vec<ModuleMeta>, Error> {
        let mut parents = Vec::new();
        for import in snapshot::imports(header).map_err(internal)? {
            parents.push(self.catalog.module(&import).await?);
        }
        Ok(parents)
    }

    /// Re-checks an entry's module and everything it imports through the
    /// kernel, and records each module that passes.
    pub async fn verify(&self, pool: &Arc<Pool>, entry: &str) -> Result<SnapshotVerified, Error> {
        let entry = parse_entry(entry)?;
        let id = self.catalog.entry(entry).await?.module;
        let (checked, failure) = self.verify_chain(pool, &id).await?;
        let meta = self.catalog.module(&id).await?;
        Ok(SnapshotVerified {
            entry: entry.to_string(),
            snapshot: snapshot::module(&id),
            verified: meta.verified,
            dirty: meta.dirty,
            checked,
            failure,
        })
    }

    /// Walks `meta.modules`, which lists imports before what imports them, so
    /// a module is only marked verified once its imports are. Stops at the
    /// first module that fails, or that was deleted and built again since:
    /// that one isn't verified this run, which isn't an error. Returns the
    /// modules that passed, in order, and the one that failed.
    async fn verify_chain(
        &self,
        pool: &Arc<Pool>,
        id: &str,
    ) -> Result<(Vec<String>, Option<SnapshotFailure>), Error> {
        self.ensure_one(id).await?;
        let modules = self.catalog.module(id).await?.modules;
        let mut checked = Vec::new();
        let mut failure = None;
        for module in &modules {
            match self.verify_module(pool, module).await? {
                Recheck::Passed => checked.push(snapshot::module(&module.id)),
                Recheck::AlreadyVerified => {}
                // Whatever imports it was built again too, or is gone.
                Recheck::Changed => break,
                Recheck::Failed(reason) => {
                    failure = Some(SnapshotFailure {
                        snapshot: snapshot::module(&module.id),
                        reason,
                    });
                    break;
                }
            }
        }
        Ok((checked, failure))
    }

    /// Re-checks one module build, whose imports must be verified and in the
    /// local cache, unless it is already verified. Its axioms then come from
    /// the re-check, replacing what the build's report said.
    ///
    /// This doesn't take the snapshot's lock, which every check on it needs:
    /// they shouldn't wait for the kernel, and two re-checks of a module agree.
    async fn verify_module(&self, pool: &Arc<Pool>, module: &Module) -> Result<Recheck, Error> {
        if let Some(done) = self.before_recheck(module).await? {
            return Ok(done);
        }
        // No header to prefer: this takes a slot with nothing warm if it can.
        let mut slot = pool.acquire("", None).await;
        // The verifier imports everything again, so free the slot's memory first.
        slot.retire_and_wait("evict").await;
        let verification = match recheck_module(&slot, &module.id).await {
            Ok(decls) => slot.policy().judge_decls(false, Some(decls)),
            Err(reason) => return Ok(Recheck::Failed(reason)),
        };
        drop(slot);
        self.record_recheck(module, &verification).await
    }

    /// Gets a re-check of a module build ready: `Changed` if the module's
    /// record names another build now, `AlreadyVerified` if this build is,
    /// else `None` once the local files, which the verifier reads, are this
    /// build's. They are fetched for a verified build too, as the next
    /// modules of the chain import it.
    async fn before_recheck(&self, module: &Module) -> Result<Option<Recheck>, Error> {
        let meta = self.catalog.module(&module.id).await?;
        if meta.modules.last().map(|own| &own.build) != Some(&module.build) {
            return Ok(Some(Recheck::Changed));
        }
        self.fetch(module).await?;
        Ok(meta.verified.then_some(Recheck::AlreadyVerified))
    }

    /// Records that a module build passed the re-check, with what it found.
    /// A module removed meanwhile stays removed, and one built again keeps
    /// its record and is `Changed`.
    async fn record_recheck(
        &self,
        module: &Module,
        verification: &Verification,
    ) -> Result<Recheck, Error> {
        let updated = self
            .catalog
            .update_module(&module.id, &module.build, |meta| {
                meta.verified = true;
                meta.dirty = !verification.ok;
                meta.result.ok = verification.ok;
                meta.result.reasons = verification.reasons.clone();
                meta.result.declarations = verification.declarations.clone();
            })
            .await?;
        Ok(match updated {
            Some(_) => Recheck::Passed,
            None => Recheck::Changed,
        })
    }

    /// Uploads a freshly built module's files (`own`, whose files have
    /// `contents`), then stores its record, and `search` with it, with the new
    /// entry. Returns the
    /// stored record and the entry. If another worker stored the module
    /// first, its record and build are kept, and this build's files go. So do
    /// they if the entry's key is on an entry of other code, which is
    /// `bad_request`, with nothing stored, and if the catalog refuses the
    /// record. Only after an `unavailable` store, which may have stored it,
    /// do they stay.
    async fn commit(
        &self,
        own: &Module,
        contents: &[Vec<u8>],
        meta: &ModuleMeta,
        search: Option<&SnapshotDecls>,
        entry: NewEntry,
    ) -> Result<(ModuleMeta, (String, EntryMeta)), Error> {
        for (file, bytes) in own.files.iter().zip(contents) {
            let compressed = zstd::encode_all(&bytes[..], 3).map_err(internal)?;
            self.store
                .put(&self.blob(own, &file.ext), PutPayload::from(compressed))
                .await
                .map_err(internal)?;
        }
        let (entry, entry_meta, stored) =
            match self.catalog.commit(&own.id, meta, search, entry).await {
                Ok(committed) => committed,
                Err(e) => {
                    if e.kind != ErrorKind::Unavailable {
                        self.delete_blobs(own).await;
                    }
                    return Err(e);
                }
            };
        if entry_meta.module != own.id {
            self.delete_blobs(own).await;
            return Err(key_in_use());
        }
        if stored.modules.last() != Some(own) {
            self.delete_blobs(own).await;
            if let Some(kept) = stored.modules.last()
                && let Err(e) = self.fetch(kept).await
            {
                // Not confirmed, so the next import fetches it.
                tracing::warn!(module = %kept.id, "fetching the stored build failed: {e}");
                self.discard(&own.id);
                return Ok((stored, (entry, entry_meta)));
            }
        }
        self.confirmed
            .lock()
            .unwrap()
            .insert(own.id.clone(), Instant::now());
        Ok((stored, (entry, entry_meta)))
    }

    pub async fn get(&self, entry: &str) -> Result<SnapshotInfo, Error> {
        let entry = parse_entry(entry)?;
        let entry_meta = self.catalog.entry(entry).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        Ok(SnapshotInfo {
            entry: entry.to_string(),
            snapshot: snapshot::module(&entry_meta.module),
            imports: meta.imports.iter().map(|i| snapshot::module(i)).collect(),
            header: meta.header,
            code: meta.code,
            labels: entry_meta.labels,
            dirty: meta.dirty,
            verified: meta.verified,
            result: meta.result,
            created: entry_meta.created_micros / 1_000_000,
        })
    }

    /// The entries, newest first, that have every label in `labels`, at most
    /// `limit`, after the cursor `after`; see [`Catalog::list`].
    pub async fn list(
        &self,
        labels: &[(String, String)],
        limit: usize,
        after: Option<&str>,
    ) -> Result<SnapshotList, Error> {
        if !(1..=MAX_LIMIT).contains(&limit) || after.is_some_and(|a| !is_cursor(a)) {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("a listing takes a limit from 1 to {MAX_LIMIT} and a cursor from `next`"),
            ));
        }
        if labels.len() > MAX_LABELS {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!(
                    "{} label filters; a listing takes at most {MAX_LABELS}",
                    labels.len()
                ),
            ));
        }
        let (listed, next) = self.catalog.list(labels, limit, after).await?;
        let mut modules: HashMap<String, Option<ModuleMeta>> = HashMap::new();
        let mut entries = Vec::new();
        for (entry, entry_meta) in listed {
            let id = entry_meta.module.clone();
            if !modules.contains_key(&id) {
                let meta = match self.catalog.module(&id).await {
                    Ok(meta) => Some(meta),
                    // Its entries were deleted since they were listed.
                    Err(e) if e.kind == ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                };
                modules.insert(id.clone(), meta);
            }
            if let Some(meta) = &modules[&id] {
                entries.push(summary(entry, entry_meta, meta));
            }
        }
        Ok(SnapshotList { entries, next })
    }

    /// Edits an entry's labels; see [`Catalog::set_labels`].
    pub async fn set_labels(
        &self,
        entry: &str,
        set: &BTreeMap<String, String>,
        remove: &[String],
    ) -> Result<SnapshotSummary, Error> {
        let entry = parse_entry(entry)?;
        let entry_meta = self.catalog.set_labels(entry, set, remove).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        Ok(summary(entry.to_string(), entry_meta, &meta))
    }

    /// Deletes an entry, then each module that nothing needs any more: the
    /// entry's own, then up the chain of imports. Once the entry is gone,
    /// the rest is best effort: what fails is logged, and a module whose
    /// record went is listed even if its files couldn't be deleted.
    pub async fn delete(&self, entry: &str) -> Result<Deleted, Error> {
        let entry = parse_entry(entry)?;
        let (entry_meta, cleaned) = self.catalog.remove_entry(entry).await?;
        let mut queue = VecDeque::from_iter(cleaned.map(|m| (entry_meta.module, m)));
        let mut removed = Vec::new();
        while let Some((id, meta)) = queue.pop_front() {
            // The record is gone, so the files of its build can go.
            if let Some(own) = meta.modules.last() {
                self.delete_blobs(own).await;
            }
            self.forget(&id).await;
            removed.push(snapshot::module(&id));
            for parent in meta.imports {
                match self.catalog.cleanup(&parent).await {
                    Ok(Some(parent_meta)) => queue.push_back((parent, parent_meta)),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(module = %parent, "cleaning up a snapshot failed: {e}")
                    }
                }
            }
        }
        Ok(Deleted {
            entry: entry.to_string(),
            removed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    /// A worker's snapshots over `db`, with its local cache in `dir`.
    fn snapshots(db: Db, dir: &Path) -> Arc<Snapshots> {
        std::fs::create_dir_all(dir.join(snapshot::ROOT)).unwrap();
        let store = Arc::new(InMemory::new());
        let s = Snapshots::new(store, db, "v".into(), dir.into(), Duration::from_secs(60));
        Arc::new(s)
    }

    /// A new build of module `id`: one `.olean` that holds `content`.
    fn built(id: &str, content: &[u8]) -> ModuleMeta {
        let own = Module {
            id: id.into(),
            build: uuid::Uuid::new_v4().simple().to_string(),
            files: vec![ModuleFile {
                ext: "olean".into(),
                sha256: sha256(content),
            }],
        };
        ModuleMeta {
            imports: Vec::new(),
            modules: vec![own],
            header: String::new(),
            code: String::new(),
            dirty: false,
            verified: false,
            macros: false,
            result: CheckResult::default(),
            created: 0,
        }
    }

    fn no_labels() -> NewEntry {
        NewEntry {
            labels: Default::default(),
            key: None,
        }
    }

    fn keyed(key: &str) -> NewEntry {
        NewEntry {
            key: Some(key.into()),
            ..no_labels()
        }
    }

    /// Every row of version `v`, except the change log's, which outlive what
    /// they describe.
    async fn rows(db: &Db) -> Vec<Vec<String>> {
        db.rows("v").await.unwrap()
    }

    async fn upload(s: &Snapshots, meta: &ModuleMeta, content: &[u8]) {
        let compressed = zstd::encode_all(content, 3).unwrap();
        let path = s.blob(&meta.modules[0], "olean");
        s.store.put(&path, compressed.into()).await.unwrap();
    }

    async fn has_files(s: &Snapshots, meta: &ModuleMeta) -> bool {
        s.store
            .get(&s.blob(&meta.modules[0], "olean"))
            .await
            .is_ok()
    }

    /// The race a delete has to survive: while it removes a module, a create
    /// of the same code on this worker builds the module again, uploads it and
    /// stores it. The delete removes only the files of the build it removed,
    /// and leaves the new build's local files alone.
    #[tokio::test]
    async fn a_delete_leaves_a_new_build_of_its_module_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let catalog = Catalog::new(db, "v".into());
        let id = snapshot::id("code");
        let old = built(&id, b"old");
        upload(&s, &old, b"old").await;
        let (entry, _, _) = catalog.commit(&id, &old, None, no_labels()).await.unwrap();

        // The new build is uploaded and installed here, but not stored yet.
        let new = built(&id, b"new");
        upload(&s, &new, b"new").await;
        std::fs::write(s.file(&id, "olean"), b"new").unwrap();
        s.cached
            .lock()
            .unwrap()
            .insert(id.clone(), new.modules[0].clone());
        // A lookup vouched for the old build before.
        s.confirmed
            .lock()
            .unwrap()
            .insert(id.clone(), Instant::now());

        // The build holds the module's lock until it has stored its record,
        // so the delete removes the old record and files, then waits there.
        let lock = s.lock(&id);
        let building = lock.lock().await;
        let deleting = tokio::spawn({
            let s = s.clone();
            async move { s.delete(&entry).await }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while has_files(&s, &old).await {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        catalog.commit(&id, &new, None, no_labels()).await.unwrap();
        drop(building);

        let deleted = deleting.await.unwrap().unwrap();
        assert_eq!(deleted.removed, [snapshot::module(&id)]);
        assert!(has_files(&s, &new).await, "the new build's files stay");
        assert_eq!(std::fs::read(s.file(&id, "olean")).unwrap(), b"new");
        assert_eq!(s.cached.lock().unwrap().get(&id), Some(&new.modules[0]));
        assert_eq!(s.catalog.module(&id).await.unwrap().modules, new.modules);
        // The local copy might have been the old build: the next import
        // looks the module up again.
        assert!(!s.confirmed.lock().unwrap().contains_key(&id));
    }

    /// A re-check that passes on a module's old build. Meanwhile the module
    /// was deleted and built again: the new build stays unverified, and so
    /// does the old one's record, which no longer exists.
    #[tokio::test]
    async fn a_verify_of_an_old_build_leaves_the_new_one_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let catalog = Catalog::new(db, "v".into());
        let id = snapshot::id("code");
        let old = built(&id, b"old");
        let (entry, _, _) = catalog.commit(&id, &old, None, no_labels()).await.unwrap();
        catalog.remove_entry(&entry).await.unwrap();
        let new = built(&id, b"new");
        catalog.commit(&id, &new, None, no_labels()).await.unwrap();

        let passed = Verification {
            ok: true,
            reasons: Vec::new(),
            declarations: Vec::new(),
        };
        let old = &old.modules[0];
        assert_eq!(s.before_recheck(old).await.unwrap(), Some(Recheck::Changed));
        let recorded = s.record_recheck(old, &passed).await.unwrap();
        assert_eq!(recorded, Recheck::Changed);
        assert!(!catalog.module(&id).await.unwrap().verified);

        let new = &new.modules[0];
        let recorded = s.record_recheck(new, &passed).await.unwrap();
        assert_eq!(recorded, Recheck::Passed);
        assert!(catalog.module(&id).await.unwrap().verified);
    }

    /// The local copy is an old build that a lookup still vouches for, as on
    /// a worker that didn't see the module deleted and built again. The
    /// re-check of the stored build fetches that build first, since the
    /// verifier reads the local files.
    #[tokio::test]
    async fn a_verify_fetches_the_build_it_checks() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let id = snapshot::id("code");
        let old = built(&id, b"old");
        std::fs::write(s.file(&id, "olean"), b"old").unwrap();
        s.cached
            .lock()
            .unwrap()
            .insert(id.clone(), old.modules[0].clone());
        s.confirmed
            .lock()
            .unwrap()
            .insert(id.clone(), Instant::now());
        let new = built(&id, b"new");
        upload(&s, &new, b"new").await;
        let catalog = Catalog::new(db, "v".into());
        catalog.commit(&id, &new, None, no_labels()).await.unwrap();

        assert_eq!(s.before_recheck(&new.modules[0]).await.unwrap(), None);
        assert_eq!(std::fs::read(s.file(&id, "olean")).unwrap(), b"new");
        assert_eq!(s.cached.lock().unwrap().get(&id), Some(&new.modules[0]));
    }

    /// A record the store couldn't read back is refused before anything is
    /// written, and the files uploaded for it go.
    #[tokio::test]
    async fn a_record_too_large_is_refused_and_its_files_go() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let id = snapshot::id("code");
        let mut ours = built(&id, b"ours");
        ours.code = "x".repeat(7 * 1024 * 1024);
        let own = ours.modules[0].clone();
        let contents = [b"ours".to_vec()];
        let error = s
            .commit(&own, &contents, &ours, None, keyed("k"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest);
        assert!(
            error.message.contains("snapshot record too large"),
            "{error:?}"
        );
        assert!(rows(&db).await.is_empty());
        assert!(!has_files(&s, &ours).await, "this build's files go");
    }

    #[tokio::test]
    async fn a_listing_takes_at_most_64_filters() {
        let dir = tempfile::tempdir().unwrap();
        let s = snapshots(Db::memory().await, dir.path());
        let filters = |n: usize| -> Vec<(String, String)> {
            (0..n).map(|i| (i.to_string(), String::new())).collect()
        };
        assert!(s.list(&filters(64), 10, None).await.is_ok());
        let error = s.list(&filters(65), 10, None).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
    }

    /// Two workers build the same code at once and the other one stores it
    /// first: this one adds its entry to their record, deletes the files it
    /// uploaded and installs their build instead.
    #[tokio::test]
    async fn a_commit_that_finds_the_module_stored_keeps_that_build() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let id = snapshot::id("code");
        let theirs = built(&id, b"theirs");
        upload(&s, &theirs, b"theirs").await;
        let catalog = Catalog::new(db, "v".into());
        catalog
            .commit(&id, &theirs, None, no_labels())
            .await
            .unwrap();

        // As `install` leaves this worker's build.
        let ours = built(&id, b"ours");
        std::fs::write(s.file(&id, "olean"), b"ours").unwrap();
        let own = ours.modules[0].clone();
        s.cached.lock().unwrap().insert(id.clone(), own.clone());

        let contents = [b"ours".to_vec()];
        let (stored, (entry, _)) = s
            .commit(&own, &contents, &ours, None, no_labels())
            .await
            .unwrap();
        assert_eq!(stored.modules, theirs.modules);
        assert_eq!(catalog.entry(&entry).await.unwrap().module, id);
        assert!(!has_files(&s, &ours).await, "this build's files go");
        assert!(has_files(&s, &theirs).await);
        assert_eq!(std::fs::read(s.file(&id, "olean")).unwrap(), b"theirs");
        assert_eq!(s.cached.lock().unwrap().get(&id), Some(&theirs.modules[0]));
    }

    /// Another create took the key for other code while this one was
    /// building: its transaction writes nothing, not even the module and its
    /// child link, and the blobs uploaded for it go. An entry added to a kept
    /// module under that key gets the other code's entry back too.
    #[tokio::test]
    async fn a_commit_whose_key_is_used_by_other_code_keeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let catalog = Catalog::new(db.clone(), "v".into());
        let (parent_id, other_id, id) = (
            snapshot::id("parent"),
            snapshot::id("other"),
            snapshot::id("code"),
        );
        let parent = built(&parent_id, b"parent");
        let other = built(&other_id, b"other");
        catalog
            .commit(&parent_id, &parent, None, no_labels())
            .await
            .unwrap();
        let (taken, _, _) = catalog
            .commit(&other_id, &other, None, keyed("k"))
            .await
            .unwrap();
        upload(&s, &other, b"other").await;
        let before = rows(&db).await;

        let ours = built(&id, b"ours");
        let own = ours.modules[0].clone();
        upload(&s, &ours, b"ours").await;
        let mut on_parent = ours.clone();
        on_parent.imports = vec![parent_id.clone()];
        on_parent.modules.insert(0, parent.modules[0].clone());
        let (entry, entry_meta, _) = catalog
            .commit(&id, &on_parent, None, keyed("k"))
            .await
            .unwrap();
        assert_eq!((entry, entry_meta.module), (taken.clone(), other_id));
        assert_eq!(rows(&db).await, before);

        let contents = [b"ours".to_vec()];
        let error = s
            .commit(&own, &contents, &on_parent, None, keyed("k"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest);
        assert!(error.message.contains("different code"), "{error:?}");
        assert_eq!(rows(&db).await, before);
        assert!(catalog.module(&id).await.is_err());
        assert!(!has_files(&s, &ours).await, "this build's files go");
        assert!(has_files(&s, &other).await);

        let added = catalog.add_entry(&parent_id, keyed("k")).await.unwrap();
        assert_eq!(added.map(|(e, _)| e), Some(taken));
        assert_eq!(rows(&db).await, before);
    }

    /// The key is already on an entry of this very module: that entry is
    /// returned unchanged with the stored record, and this build's files go.
    #[tokio::test]
    async fn a_commit_whose_key_is_on_the_same_module_returns_that_entry() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let catalog = Catalog::new(db.clone(), "v".into());
        let id = snapshot::id("code");
        let theirs = built(&id, b"theirs");
        upload(&s, &theirs, b"theirs").await;
        let mut labelled = keyed("k");
        labelled.labels.insert("a".into(), "1".into());
        let (taken, _, _) = catalog.commit(&id, &theirs, None, labelled).await.unwrap();
        let before = rows(&db).await;

        let ours = built(&id, b"ours");
        let own = ours.modules[0].clone();
        upload(&s, &ours, b"ours").await;
        std::fs::write(s.file(&id, "olean"), b"ours").unwrap();
        let contents = [b"ours".to_vec()];
        let (stored, (entry, entry_meta)) = s
            .commit(&own, &contents, &ours, None, keyed("k"))
            .await
            .unwrap();
        assert_eq!(entry, taken);
        assert_eq!(entry_meta.labels["a"], "1");
        assert_eq!(stored.modules, theirs.modules);
        assert_eq!(rows(&db).await, before);
        assert!(!has_files(&s, &ours).await, "this build's files go");
        assert!(has_files(&s, &theirs).await);
    }
}
