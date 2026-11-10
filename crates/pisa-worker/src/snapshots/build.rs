//! Building a snapshot module: `pisa build`, the re-check, and committing
//! its files and record.

use super::*;

/// The files the build wrote for module `s<id>` in `dir`, by extension, the
/// `.olean` last.
pub(super) fn outputs(dir: &Path, id: &str) -> std::io::Result<Vec<(String, PathBuf)>> {
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

/// The snapshots among the modules the build says the module imports, by id.
pub(super) fn snapshot_imports(modules: &[String]) -> Result<BTreeSet<String>, Error> {
    let mut ids = BTreeSet::new();
    for module in modules {
        match snapshot::parse_module(module) {
            Ok(Some(id)) => {
                ids.insert(id);
            }
            Ok(None) => {}
            Err(e) => return Err(Error::new(ErrorKind::BadRequest, e)),
        }
    }
    Ok(ids)
}

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Every module a snapshot needs: its imports' modules, then its own.
pub(super) fn modules(parents: &[ModuleMeta], own: Module) -> Vec<Module> {
    let mut modules: Vec<Module> = Vec::new();
    for m in parents.iter().flat_map(|p| &p.modules) {
        if !modules.contains(m) {
            modules.push(m.clone());
        }
    }
    modules.push(own);
    modules
}

impl Snapshots {
    /// Compiles, judges and stores the snapshot with a new entry, once it
    /// compares well with the entry it replaces, if any. Returns its record,
    /// the entry and what the replacement changed, `None` if nothing could be
    /// checked (the cause is in the result), or the outcome of a check that
    /// failed.
    pub(super) async fn build(
        &self,
        slot: &mut Lease,
        job: &SnapshotCreateJob,
        split: &Split,
        replacing: Option<&Replacing>,
        id: &str,
        header: &str,
    ) -> Result<Built, Box<CheckOutcome>> {
        let mut outcome = CheckOutcome {
            time: 0.0,
            error: None,
            response: None,
            verification: None,
        };
        let fail = |mut outcome: CheckOutcome, error: Error| {
            outcome.error = Some(error);
            Err(Box::new(outcome))
        };
        if let Err(e) = self.ensure(&split.header, None).await {
            return fail(outcome, e);
        }
        let parents = match self.reader.parents(&split.header).await {
            Ok(parents) => parents,
            Err(e) => return fail(outcome, e),
        };
        let macros = uses_macros(&split.body) || parents.iter().any(|p| p.macros);
        // Module-system code exports its theorems as axioms to what it
        // imports, so only the verifier, which loads those whole, can
        // describe it.
        let module_system = split.header.lines().next() == Some("module");
        let recheck = module_system || job.verify != Some(false);
        // The snapshots a replacement swaps follow from its imports, so a
        // misnamed `allow` costs no build.
        let swapped = match replacing {
            Some(r) => {
                let mut needs: Vec<String> = Vec::new();
                for m in parents.iter().flat_map(|p| ids(&p.modules)) {
                    if !needs.contains(m) {
                        needs.push(m.clone());
                    }
                }
                needs.push(id.to_string());
                let swapped = match self.reader.swapped(r, &needs, id).await {
                    Ok(swapped) => swapped,
                    Err(e) => return fail(outcome, e),
                };
                if let Some(e) = allow_error(r, &swapped) {
                    return fail(outcome, e);
                }
                Some(swapped)
            }
            None => None,
        };
        // The build imports everything again, so free the slot's memory first.
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
        let mut compiled = match compiled {
            Ok(c) => c,
            Err(e) => {
                remove_module(&dir, id);
                return fail(outcome, internal(format!("failed to start Lean: {e}")));
            }
        };
        // The file is the submitted code, so positions need no shifting.
        let mut messages = std::mem::take(&mut compiled.messages);
        messages.extend(auto_implicit_hint(&split.body, &messages));
        let response = CommandResponse {
            env: 0,
            messages,
            sorries: Vec::new(),
            tactics: Vec::new(),
            at: Vec::new(),
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
        // What the module imports is what Lean's own header parser read,
        // which can differ from the line-by-line split the record holds.
        let read = match &compiled.imports {
            Some(modules) => snapshot_imports(modules),
            None => Err(internal("the build didn't say what the module imports")),
        };
        let listed = snapshot::imports(&split.header).unwrap_or_default();
        if let Err(e) = read.and_then(|read| {
            let listed: BTreeSet<String> = listed.into_iter().collect();
            (read == listed).then_some(()).ok_or_else(|| {
                Error::new(
                    ErrorKind::BadRequest,
                    "Lean reads other snapshot imports than the header's lines say: \
                     put each import on its own line",
                )
            })
        }) {
            remove_module(&dir, id);
            return fail(outcome, e);
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
        // Without the re-check, the build's report stands only for code whose
        // report can't say more than the code does: outside the module
        // system, which exports theorems as axioms, and with no macros, which
        // could expand into what the scan forbids.
        let reported_stands = !module_system && !macros && job.verify != Some(true);
        let per_module = Duration::from_secs(job.timeout_secs);
        let (rechecked, unverified) = if recheck {
            match recheck_module(slot, id, per_module).await {
                Ok(decls) => (Some(decls), None),
                Err(Unchecked::Unfinished(u)) if reported_stands => {
                    let unverified = VerifyFailure {
                        module: snapshot::module(id),
                        reason: u.to_string(),
                    };
                    (None, Some(unverified))
                }
                Err(Unchecked::Unfinished(u)) => {
                    self.unbuild(slot, id);
                    return fail(outcome, unfinished_error(&u));
                }
                Err(Unchecked::Rejected(reason)) => {
                    self.unbuild(slot, id);
                    return fail(outcome, Error::new(ErrorKind::Lean, reason));
                }
            }
        } else {
            (None, None)
        };

        // Keep the builder warm on the snapshot so the next checks are hits.
        if let Err(f) = slot
            .warm_up_with(header, &header_key(header), Some(id))
            .await
        {
            self.unbuild(slot, id);
            return fail(outcome, header_error(f, slot.import_timeout()));
        }
        // The verifier's declarations, which don't depend on what ran in the
        // build, or else the build's report.
        let kernel = rechecked.is_some();
        let decls = match rechecked {
            Some(decls) => decls,
            None => match compiled.report.take().and_then(parse_declarations) {
                Some(decls) => decls,
                None => {
                    self.unbuild(slot, id);
                    return fail(outcome, internal("the build's axiom report is missing"));
                }
            },
        };
        let mut verification = slot.policy().judge_decls(false, Some(decls));
        // For search only, so a failure just leaves the module out of it.
        let exported = compiled.export.take().and_then(parse_export);
        if exported.is_none() {
            tracing::warn!(module = %id, "the build's declaration export is missing");
        }
        let status = worst_status(&verification.declarations);
        let replacement = match replacing.zip(swapped.as_ref()) {
            Some((r, swapped)) => match compare(r, &verification.declarations, swapped) {
                Ok(replacement) => Some(replacement),
                Err(mut changes) => {
                    self.unbuild(slot, id);
                    let gained: Vec<String> =
                        swapped.gained.iter().map(|(m, _)| m.clone()).collect();
                    let mut now = self.reader.exports_of(&gained).await;
                    let own = exported.iter().flatten();
                    now.extend(own.map(|d| (d.name.clone(), d.signature.clone())));
                    self.reader.sign(&mut changes, r, swapped, &now).await;
                    verification.ok = false;
                    verification
                        .reasons
                        .extend(changes.iter().map(ToString::to_string));
                    verification.changes = changes;
                    outcome.verification = Some(verification);
                    return Err(Box::new(outcome));
                }
            },
            None => None,
        };
        // A spec's holes, now that its own declarations are known.
        let closure: Vec<String> = parents
            .iter()
            .flat_map(|p| ids(&p.modules))
            .cloned()
            .collect();
        match self
            .reader
            .holes_error(&job.holes, &verification.declarations, &closure)
            .await
        {
            Ok(None) => {}
            Ok(Some(e)) | Err(e) => {
                self.unbuild(slot, id);
                outcome.verification = Some(verification);
                return fail(outcome, e);
            }
        }
        outcome.verification = Some(verification);

        let meta = ModuleMeta {
            imports: snapshot::imports(&split.header).unwrap_or_default(),
            modules: modules(&parents, own.clone()),
            header: header.to_string(),
            code: job.code.clone(),
            status,
            verified: kernel && parents.iter().all(|p| p.verified),
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
            replaces: replacing.map(Replacing::link),
            replacement: replacement.clone(),
            fork: job.fork,
            holes: replacing.map_or_else(|| job.holes.clone(), |r| r.holes.clone()),
        };
        match self
            .commit(&own, &contents, &meta, search.as_ref(), entry)
            .await
        {
            Ok((meta, entry)) => Ok(Some((meta, entry, replacement, unverified))),
            Err(e) => {
                self.unbuild(slot, id);
                fail(outcome, e)
            }
        }
    }

    /// Moves a freshly built module's files from `dir` into the local cache,
    /// not yet confirmed, and returns the build with the files' contents.
    pub(super) async fn install(
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
    pub(super) fn unbuild(&self, slot: &mut Lease, id: &str) {
        slot.retire("snapshot_failed");
        self.discard(id);
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
    pub(super) async fn commit(
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
}
