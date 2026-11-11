//! Creating a snapshot: replaying a key, joining kept code, or building it.

use super::*;

/// What [`Snapshots::build`] kept: the module's record, the new entry, and
/// what the replacement changed.
pub(super) type Built = Option<(
    ModuleMeta,
    (String, EntryMeta),
    Option<Replacement>,
    Option<VerifyFailure>,
)>;

/// Puts what a create's comparison with `replacing` found into its
/// response, if the entry the response has records that comparison: a create
/// that lost a race on its key gets the other create's entry.
pub(super) fn replaced(
    done: &mut CreateSnapshotResponse,
    replacing: Option<&Replacing>,
    replacement: Option<Replacement>,
) {
    if let (Some(replacing), Some(r)) = (replacing, replacement)
        && done
            .snapshot
            .as_ref()
            .is_some_and(|s| s.replaces.as_ref() == Some(&replacing.replaced))
    {
        done.replacement = Some(r);
    }
}

/// The error for a create that asked to verify an import or existing snapshot
/// and couldn't: of the failure's kind, so that a re-check out of time reads
/// as a timeout and not as the kernel's verdict.
pub(super) fn failure_error(f: &VerifyFailure) -> Error {
    Error::new(
        f.kind,
        format!("{} failed verification: {}", f.module, f.reason),
    )
}

impl Snapshots {
    /// The response to a create that added `entry` to the module `meta`
    /// records, or replayed it. What a replacement changed is for the caller
    /// to add: a key replay doesn't compare again.
    pub(super) fn created(
        &self,
        meta: ModuleMeta,
        (entry, entry_meta): (String, EntryMeta),
    ) -> CreateSnapshotResponse {
        CreateSnapshotResponse {
            snapshot: Some(self.snapshot(entry, entry_meta, &meta)),
            check: meta.result,
            replacement: None,
            heads: Vec::new(),
            unverified: None,
        }
    }

    /// The response to a create of kept code, the module `meta` records,
    /// that added no entry: the module's check, failed, for the caller to
    /// say why.
    pub(super) fn refused(&self, meta: ModuleMeta) -> CreateSnapshotResponse {
        let mut check = meta.result;
        check.ok = false;
        CreateSnapshotResponse {
            snapshot: None,
            check,
            replacement: None,
            heads: Vec::new(),
            unverified: None,
        }
    }

    /// Checks `job.code` by compiling it to a module and keeps the module
    /// unless Lean reported errors, with a new entry on it. Code that is
    /// already kept gets the new entry without being built again. Failures
    /// are reported in `check`. So is a replaced snapshot that another
    /// replaces already, which is refused as a `conflict` before anything is
    /// built, with the run's `heads`, unless the job forks.
    pub async fn create(&self, pool: &Arc<Pool>, job: SnapshotCreateJob) -> CreateSnapshotResponse {
        let mut response = CreateSnapshotResponse {
            snapshot: None,
            check: CheckResult::default(),
            replacement: None,
            heads: Vec::new(),
            unverified: None,
        };
        let valid =
            check_labels(&job.labels).and_then(|()| job.key.as_deref().map_or(Ok(()), check_key));
        if let Err(e) = valid {
            response.check.error = Some(Error::new(ErrorKind::BadRequest, e));
            return response;
        }
        let split = split_header(&job.code);
        if let Err(e) = snapshot::imports(&split.header) {
            response.check.error = Some(Error::new(ErrorKind::BadRequest, e));
            return response;
        }
        // Macros are allowed: `build` re-checks them through the kernel unless
        // the request opts out, which marks the snapshot as not verified.
        let found = pool.policy().findings(&split.body, true);
        if !found.is_empty() {
            response.check.error = Some(policy_error(&found, split.header_lines));
            response.check.messages =
                pisa_protocol::api::messages(policy_messages(&found, split.header_lines));
            response.check.reasons = found.into_iter().map(|f| f.reason).collect();
            return response;
        }
        if let Some(key) = &job.key {
            match self.replay(key, &snapshot::id(&job.code)).await {
                Ok(Some(done)) => return done,
                Ok(None) => {}
                Err(e) => {
                    response.check.error = Some(e);
                    return response;
                }
            }
        }
        let replacing = match &job.replaces {
            Some(r) => match self.reader.replacing(r).await {
                Ok(r) => Some(r),
                Err(e) => {
                    response.check.error = Some(e);
                    return response;
                }
            },
            None => None,
        };
        if let Some(r) = replacing.as_ref().filter(|_| !job.fork) {
            match self.reader.replaced_by(&r.entry).await {
                Ok(Some((error, heads))) => {
                    response.check.error = Some(error);
                    response.heads = heads;
                    return response;
                }
                Ok(None) => {}
                Err(e) => {
                    response.check.error = Some(e);
                    return response;
                }
            }
        }
        // A kept module removed before its new entry was added is built
        // again, once.
        for _ in 0..2 {
            let done = self
                .create_once(pool, &job, &split, replacing.as_ref(), response.clone())
                .await;
            if let Some(mut done) = done {
                // Another create replaced it while this one was building.
                if let Some(r) = &replacing
                    && done
                        .check
                        .error
                        .as_ref()
                        .is_some_and(|e| e.kind == ErrorKind::Conflict)
                    && let Ok(Some((error, heads))) = self.reader.replaced_by(&r.entry).await
                {
                    done.check.error = Some(error);
                    done.heads = heads;
                }
                return done;
            }
        }
        response.check.error = Some(internal(
            "the snapshot was deleted twice while it was being created",
        ));
        response
    }

    /// The response to a create whose idempotency key an entry already has:
    /// that entry, unchanged, if it is on module `id`, else an error. `None` if
    /// no entry has the key.
    pub(super) async fn replay(
        &self,
        key: &str,
        id: &str,
    ) -> Result<Option<CreateSnapshotResponse>, Error> {
        let Some((entry, entry_meta)) = self.catalog.keyed(key).await? else {
            return Ok(None);
        };
        if entry_meta.module != id {
            return Err(key_in_use());
        }
        let meta = self.catalog.module(id).await?;
        Ok(Some(self.created(meta, (entry, entry_meta))))
    }

    /// [`create`](Self::create) after validation. `None` if the module was
    /// kept but removed before the entry could be added, or during the
    /// re-check the request asked for.
    pub(super) async fn create_once(
        &self,
        pool: &Arc<Pool>,
        job: &SnapshotCreateJob,
        split: &Split,
        replacing: Option<&Replacing>,
        mut response: CreateSnapshotResponse,
    ) -> Option<CreateSnapshotResponse> {
        let id = snapshot::id(&job.code);
        let lock = self.lock(&id);
        let building = lock.lock().await;
        match self.catalog.module(&id).await {
            Ok(meta) => {
                drop(building);
                return self
                    .add_to_kept(pool, &id, meta, job, replacing, response)
                    .await;
            }
            Err(e) if e.kind == ErrorKind::NotFound => {}
            Err(e) => {
                response.check.error = Some(e);
                return Some(response);
            }
        }
        if job.verify == Some(true) {
            let imports = snapshot::imports(&split.header).unwrap_or_default();
            let per_module = Duration::from_secs(job.timeout_secs);
            let error = match self.verify_chain(pool, &imports, per_module).await {
                Ok((_, Some(failure))) => Some(failure_error(&failure)),
                Ok((_, None)) => None,
                Err(e) => Some(e),
            };
            if let Some(error) = error {
                response.check.error = Some(error);
                return Some(response);
            }
        }
        let header = snapshot::next_header(&split.header, &id);
        let mut slot = pool.acquire(&header_key(&header)).await;
        let built = self
            .build(&mut slot, job, split, replacing, &id, &header)
            .await;
        slot.finish_job();
        Some(match built {
            Ok(Some((meta, entry, replacement, unverified))) => {
                let mut done = self.created(meta, entry);
                replaced(&mut done, replacing, replacement);
                done.unverified = unverified;
                done
            }
            Ok(None) => response,
            Err(outcome) => {
                response.check = (*outcome).into();
                response
            }
        })
    }

    /// A create for code that is already kept: unless the request says
    /// `verify: false`, it verifies the module's chain first if the module
    /// isn't verified, then adds the entry. Its check is what is stored. If
    /// the re-check fails, the check has the error and no entry is added, as
    /// for new code that fails it; as there, a re-check out of time or memory
    /// of plain code with no macros adds the entry, not `verified`, and says
    /// so in `unverified`, unless the request asked to verify. `None` if the
    /// module is gone by the time it is needed, or was built again during the
    /// re-check.
    pub(super) async fn add_to_kept(
        &self,
        pool: &Arc<Pool>,
        id: &str,
        meta: ModuleMeta,
        job: &SnapshotCreateJob,
        replacing: Option<&Replacing>,
        mut response: CreateSnapshotResponse,
    ) -> Option<CreateSnapshotResponse> {
        let module_system = split_header(&job.code).header.lines().next() == Some("module");
        let reported_stands = !module_system && !meta.macros && job.verify.is_none();
        let mut unverified = None;
        let meta = if job.verify != Some(false) && !meta.verified {
            let verified = async {
                let ids = [id.to_string()];
                let per_module = Duration::from_secs(job.timeout_secs);
                let (_, failure) = self.verify_chain(pool, &ids, per_module).await?;
                Ok::<_, Error>((self.catalog.module(id).await?, failure))
            };
            match verified.await {
                // Nothing failed, so the chain stopped at a module built again.
                Ok((meta, None)) if !meta.verified => return None,
                Ok((meta, None)) => meta,
                Ok((meta, Some(failure)))
                    if reported_stands
                        && matches!(failure.kind, ErrorKind::Timeout | ErrorKind::OutOfMemory) =>
                {
                    unverified = Some(failure);
                    meta
                }
                Ok((meta, Some(failure))) => {
                    let mut refused = self.refused(meta);
                    refused.check.error = Some(failure_error(&failure));
                    return Some(refused);
                }
                Err(e) => {
                    let gone = e.kind == ErrorKind::NotFound
                        && matches!(self.catalog.module(id).await, Err(e) if e.kind == ErrorKind::NotFound);
                    if gone {
                        return None;
                    }
                    response.check.error = Some(e);
                    return Some(response);
                }
            }
        } else {
            meta
        };
        let replacement = match replacing {
            Some(r) => {
                let needs: Vec<String> = ids(&meta.modules).cloned().collect();
                let swapped = match self.reader.swapped(r, &needs, id).await {
                    Ok(swapped) => swapped,
                    Err(e) => {
                        response.check.error = Some(e);
                        return Some(response);
                    }
                };
                if let Some(e) = allow_error(r, &swapped) {
                    response.check.error = Some(e);
                    return Some(response);
                }
                match compare(r, &meta.result.declarations, &swapped) {
                    Ok(replacement) => Some(replacement),
                    Err(mut changes) => {
                        let mut new = vec![id.to_string()];
                        new.extend(swapped.gained.iter().map(|(m, _)| m.clone()));
                        let now = self.reader.exports_of(&new).await;
                        self.reader.sign(&mut changes, r, &swapped, &now).await;
                        let mut refused = self.refused(meta);
                        refused
                            .check
                            .reasons
                            .extend(changes.iter().map(ToString::to_string));
                        refused.check.changes = changes;
                        return Some(refused);
                    }
                }
            }
            None => None,
        };
        let closure: Vec<String> = ids(&meta.modules).filter(|m| *m != id).cloned().collect();
        match self
            .reader
            .holes_error(&job.holes, &meta.result.declarations, &closure)
            .await
        {
            Ok(None) => {}
            Ok(Some(e)) | Err(e) => {
                response.check.error = Some(e);
                return Some(response);
            }
        }
        let entry = NewEntry {
            labels: job.labels.clone(),
            key: job.key.clone(),
            replaces: replacing.map(Replacing::link),
            replacement: replacement.clone(),
            fork: job.fork,
            holes: replacing.map_or_else(|| job.holes.clone(), |r| r.holes.clone()),
        };
        let added = match self.catalog.add_entry(id, entry).await {
            Ok(Some(added)) => added,
            Ok(None) => return None,
            Err(e) => {
                response.check.error = Some(e);
                return Some(response);
            }
        };
        // Another create took the key for other code since `create` looked.
        if added.1.module != id {
            response.check.error = Some(key_in_use());
            return Some(response);
        }
        let mut done = self.created(meta, added);
        replaced(&mut done, replacing, replacement);
        done.unverified = unverified;
        Some(done)
    }
}
