//! Creating a snapshot: replaying a key, joining kept code, or building it.

use std::collections::HashSet;

use pisa_db::catalog::name_taken;
use pisa_protocol::api::{self, Amends, Declaration, Replaces};
use pisa_protocol::verify::normalised;

use super::*;

/// What [`Snapshots::build`] kept: the module's record, the new entry, and
/// what the replacement changed.
pub(super) type Built = Option<(
    ModuleMeta,
    (String, EntryMeta),
    Option<Replacement>,
    Option<VerifyFailure>,
)>;

/// What a create relates its snapshot to, as found before the build.
#[derive(Default)]
pub(super) struct Relations {
    /// The snapshot it replaces.
    pub replacing: Option<Replacing>,
    /// The spec it amends.
    pub amending: Option<Amending>,
    /// The spec target it refutes.
    pub refuting: Option<Refuting>,
}

/// The spec target a create refutes, as found before the build.
pub(super) struct Refuting {
    /// The spec's entry and module.
    pub spec: String,
    pub spec_module: String,
    /// The target, as the spec's module declares it.
    pub target: Declaration,
}

impl Refuting {
    /// The record of what `refuter` found, the theorem that refutes.
    pub fn record(&self, theorem: String) -> crate::catalog::Refutation {
        crate::catalog::Refutation {
            spec: self.spec.clone(),
            target: self.target.name.clone(),
            theorem,
            target_hash: Some(self.target.type_hash.clone()),
        }
    }
}

/// The spec a create amends, as found before the build: compared as a
/// snapshot that a replacement may change nothing of, holes included, so
/// that every change is listed.
pub(super) struct Amending {
    pub amended: Replacing,
    pub reason: String,
    /// The amended spec's holes, which the new spec keeps unless the
    /// request names its own.
    pub holes: Vec<String>,
}

/// The holes of the snapshot a create makes, unless it replaces one: those
/// the request names, or with `amends` and none named, the amended spec's.
pub(super) fn spec_holes(job: &SnapshotCreateJob, rel: &Relations) -> Vec<String> {
    match (&job.holes, &rel.amending) {
        (Some(holes), _) => holes.clone(),
        (None, Some(a)) => a.holes.clone(),
        (None, None) => Vec::new(),
    }
}

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
            ..Default::default()
        }
    }

    /// The response to a create of kept code, the module `meta` records,
    /// that added no entry: the module's check, failed, for the caller to
    /// say why.
    pub(super) fn refused(&self, meta: ModuleMeta) -> CreateSnapshotResponse {
        let mut check = meta.result;
        check.ok = false;
        CreateSnapshotResponse {
            check,
            ..Default::default()
        }
    }

    /// Checks `job.code` by compiling it to a module and keeps the module
    /// unless Lean reported errors, with a new entry on it. Code that is
    /// already kept gets the new entry without being built again. Failures
    /// are reported in `check`. So is a replaced snapshot that another
    /// replaces already, which is refused as a `conflict` before anything is
    /// built, with the run's `heads`, unless the job forks. A spec that
    /// amends another, or a refutation, answers what its entry recorded of
    /// that, a key replay too.
    pub async fn create(&self, pool: &Arc<Pool>, job: SnapshotCreateJob) -> CreateSnapshotResponse {
        let (amends, refutes) = (job.amends.is_some(), job.refutes.is_some());
        let replaces = job.replaces.is_some();
        let runs = job.runs.clone();
        let sent = job.source.clone().map(|source| (job.code.clone(), source));
        let mut done = self.create_checked(pool, job).await;
        done.runs = runs;
        // The build's messages are in the code as resolved: placed on the
        // header lines as sent.
        if let Some((code, source)) = &sent {
            let place = header::resolved_to_submitted(code, source);
            for m in &mut done.check.messages {
                m.pos.column = place(m.pos.line, m.pos.column);
                if let Some(end) = &mut m.end_pos {
                    end.column = place(end.line, end.column);
                }
            }
        }
        if let Some(s) = done.snapshot.as_mut()
            && let Err(e) = self.reader.name_runs(std::slice::from_mut(s)).await
        {
            tracing::warn!(snapshot = %s.id, "reading its run's name failed: {e}");
        }
        if refutes && let Some(s) = &done.snapshot {
            let entry = self.reader.parse(&s.id).unwrap_or_default();
            match self
                .catalog
                .refutations_of(std::slice::from_ref(&entry))
                .await
            {
                Ok(found) => {
                    done.refutation = (found.into_iter())
                        .find(|(e, _)| *e == entry)
                        .map(|(e, r)| self.reader.refutation(&e, r));
                }
                Err(e) => tracing::warn!(entry, "reading the refutation back failed: {e}"),
            }
        }
        // A create into a run hears of its refutations.
        if replaces && let Some(s) = &done.snapshot {
            let entry = self.reader.parse(&s.id).unwrap_or_default();
            match self.reader.refutations_of_run(&entry).await {
                Ok(found) => done.refutations = found,
                Err(e) => tracing::warn!(entry, "reading the run's refutations failed: {e}"),
            }
        }
        if amends && let Some(s) = &done.snapshot {
            let entry = self.reader.parse(&s.id).unwrap_or_default();
            match self
                .catalog
                .amendments_of(std::slice::from_ref(&entry))
                .await
            {
                Ok(found) => {
                    done.amendment = (found.into_iter())
                        .find(|(e, _)| *e == entry)
                        .map(|(e, a)| self.reader.amendment(&e, a));
                }
                Err(e) => tracing::warn!(entry, "reading the amendment back failed: {e}"),
            }
        }
        done
    }

    /// [`create`](Self::create), but for reading the amendment back.
    async fn create_checked(
        &self,
        pool: &Arc<Pool>,
        job: SnapshotCreateJob,
    ) -> CreateSnapshotResponse {
        let mut response = CreateSnapshotResponse::default();
        let valid =
            check_labels(&job.labels).and_then(|()| job.key.as_deref().map_or(Ok(()), check_key));
        if let Err(e) = valid {
            response.check.error = Some(Error::new(ErrorKind::BadRequest, e));
            return response;
        }
        let split = split_header(&job.code);
        let imports = match snapshot::imports(&split.header) {
            Ok(imports) => imports,
            Err(e) => {
                response.check.error = Some(Error::new(ErrorKind::BadRequest, e));
                return response;
            }
        };
        match self.reader.clashes(&imports).await {
            Ok(None) => {}
            Ok(Some(e)) | Err(e) => {
                response.check.error = Some(e);
                return response;
            }
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
            let id = snapshot::id(&job.code);
            match self.replay(key, &id, job.source.as_deref()).await {
                Ok(Some(done)) => return done,
                Ok(None) => {}
                Err(e) => {
                    response.check.error = Some(e);
                    return response;
                }
            }
        }
        if let Some(name) = &job.name
            && let Err(e) = self.name_free(name, job.amends.as_ref()).await
        {
            response.check.error = Some(e);
            return response;
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
        let amending = match &job.amends {
            Some(a) => {
                let amended = Replaces {
                    snapshot_id: a.snapshot_id.clone(),
                    allow: None,
                };
                match self.reader.replacing(&amended).await {
                    Ok(mut amended) => {
                        // A spec is amended once: the amendment is the spec
                        // now, and what comes next amends that.
                        match self.reader.superseding(&amended.entry).await {
                            Ok(None) => {}
                            Ok(Some(current)) => {
                                response.check.error = Some(Error::new(
                                    ErrorKind::Conflict,
                                    format!(
                                        "{} is amended already; the spec now current is {}: \
                                         amend that one",
                                        a.snapshot_id,
                                        self.id(&current)
                                    ),
                                ));
                                return response;
                            }
                            Err(e) => {
                                response.check.error = Some(e);
                                return response;
                            }
                        }
                        let holes = std::mem::take(&mut amended.holes);
                        Some(Amending {
                            amended,
                            reason: a.reason.clone(),
                            holes,
                        })
                    }
                    Err(e) => {
                        response.check.error = Some(e);
                        return response;
                    }
                }
            }
            None => None,
        };
        let refuting = match &job.refutes {
            Some(r) => match self.refuting(r).await {
                Ok(r) => Some(r),
                Err(e) => {
                    response.check.error = Some(e);
                    return response;
                }
            },
            None => None,
        };
        let rel = Relations {
            replacing,
            amending,
            refuting,
        };
        if let Some(r) = rel.replacing.as_ref() {
            match self.reader.superseded(&r.entry).await {
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
        if let Some(r) = rel.replacing.as_ref().filter(|_| !job.fork) {
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
                .create_once(pool, &job, &split, &rel, response.clone())
                .await;
            if let Some(mut done) = done {
                // Another create replaced it, or amended the run's spec, while
                // this one was building.
                if let Some(r) = &rel.replacing
                    && done
                        .check
                        .error
                        .as_ref()
                        .is_some_and(|e| e.kind == ErrorKind::Conflict)
                {
                    let found = match self.reader.superseded(&r.entry).await {
                        Ok(None) => self.reader.replaced_by(&r.entry).await,
                        found => found,
                    };
                    if let Ok(Some((mut error, heads))) = found {
                        if done
                            .check
                            .error
                            .as_ref()
                            .is_some_and(|e| e.message.ends_with(build::KEPT))
                        {
                            error.message.push_str(build::KEPT);
                        }
                        done.check.error = Some(error);
                        done.heads = heads;
                    }
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
    /// that entry, unchanged, if it is on module `id`, or if it was sent as
    /// `source` too, its imports by run name resolving to other heads since;
    /// else an error. `None` if no entry has the key.
    pub(super) async fn replay(
        &self,
        key: &str,
        id: &str,
        source: Option<&str>,
    ) -> Result<Option<CreateSnapshotResponse>, Error> {
        let Some((entry, entry_meta)) = self.catalog.keyed(key).await? else {
            return Ok(None);
        };
        let sent_again = source.is_some() && entry_meta.source.as_deref() == source;
        if entry_meta.module != id && !sent_again {
            return Err(key_in_use());
        }
        let meta = self.catalog.module(&entry_meta.module).await?;
        Ok(Some(self.created(meta, (entry, entry_meta))))
    }

    /// Refuses `name` for the run a create starts while another run that has
    /// snapshots has it, unless the create amends that run's spec, as `amends`
    /// says: the name then moves.
    async fn name_free(&self, name: &str, amends: Option<&Amends>) -> Result<(), Error> {
        // A spec that amends a named run's takes that name, and no other.
        if let Some(a) = amends {
            let amended = self.reader.parse(&a.snapshot_id)?;
            let names = self
                .catalog
                .run_names_of(std::slice::from_ref(&amended))
                .await?;
            match names.get(&amended) {
                Some(held) if held == name => return Ok(()),
                Some(held) => {
                    return Err(Error::new(
                        ErrorKind::BadRequest,
                        format!(
                            "the amended spec's run is named {held}, which the amending spec's \
                             takes: leave out name {name}, or give that one"
                        ),
                    ));
                }
                None => {}
            }
        }
        match self.catalog.run_named(name).await? {
            Some((root, true)) => Err(name_taken(&self.version, name, &root)),
            _ => Ok(()),
        }
    }

    /// [`create`](Self::create) after validation. `None` if the module was
    /// kept but removed before the entry could be added, or during the
    /// re-check the request asked for.
    pub(super) async fn create_once(
        &self,
        pool: &Arc<Pool>,
        job: &SnapshotCreateJob,
        split: &Split,
        rel: &Relations,
        mut response: CreateSnapshotResponse,
    ) -> Option<CreateSnapshotResponse> {
        let id = snapshot::id(&job.code);
        let lock = self.lock(&id);
        let building = lock.lock().await;
        match self.catalog.module(&id).await {
            Ok(meta) => {
                drop(building);
                return self.add_to_kept(pool, &id, meta, job, rel, response).await;
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
            let error = match self.verify_chain(pool, &imports, per_module, false).await {
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
        let built = self.build(&mut slot, job, split, rel, &id, &header).await;
        slot.finish_job();
        Some(match built {
            Ok(Some((meta, entry, replacement, unverified))) => {
                let mut done = self.created(meta, entry);
                replaced(&mut done, rel.replacing.as_ref(), replacement);
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
        rel: &Relations,
        mut response: CreateSnapshotResponse,
    ) -> Option<CreateSnapshotResponse> {
        let replacing = rel.replacing.as_ref();
        let module_system = split_header(&job.code).header.lines().next() == Some("module");
        let reported_stands = !module_system && !meta.macros && job.verify.is_none();
        let mut unverified = None;
        let meta = if job.verify != Some(false) && !meta.verified {
            let verified = async {
                let ids = [id.to_string()];
                let per_module = Duration::from_secs(job.timeout_secs);
                let (_, failure) = self.verify_chain(pool, &ids, per_module, false).await?;
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
                let swapped = match self.swap(r, &needs, id).await {
                    Ok(swapped) => swapped,
                    Err(e) => {
                        response.check.error = Some(e);
                        return Some(response);
                    }
                };
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
            .holes_error(&spec_holes(job, rel), &meta.result.declarations, &closure)
            .await
        {
            Ok(None) => {}
            Ok(Some(e)) | Err(e) => {
                response.check.error = Some(e);
                return Some(response);
            }
        }
        let refutes = match &rel.refuting {
            Some(r) => {
                let needs: Vec<String> = ids(&meta.modules).cloned().collect();
                let decls = &meta.result.declarations;
                match refuter(&r.target, &r.spec_module, &needs, meta.verified, decls) {
                    Ok(theorem) => Some(r.record(theorem)),
                    Err(reasons) => {
                        let mut refused = self.refused(meta);
                        refused.check.reasons.extend(reasons);
                        return Some(refused);
                    }
                }
            }
            None => None,
        };
        let amends = match &rel.amending {
            Some(a) => {
                let needs: Vec<String> = ids(&meta.modules).cloned().collect();
                let own = self.reader.exports_of(&[id.to_string()]).await;
                let decls = &meta.result.declarations;
                match self
                    .amendment(a, &spec_holes(job, rel), &needs, id, decls, own)
                    .await
                {
                    Ok(a) => Some(a),
                    Err(e) => {
                        response.check.error = Some(e);
                        return Some(response);
                    }
                }
            }
            None => None,
        };
        let entry = NewEntry {
            labels: job.labels.clone(),
            key: job.key.clone(),
            replaces: replacing.map(Replacing::link),
            replacement: replacement.clone(),
            fork: job.fork,
            holes: replacing.map_or_else(|| spec_holes(job, rel), |r| r.holes.clone()),
            amends,
            refutes,
            name: job.name.clone(),
            source: job.source.clone(),
            runs: job.runs.clone(),
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

impl Snapshots {
    /// The target that `r` names, if it can be refuted (see [`refutable`]).
    pub(super) async fn refuting(&self, r: &api::Refutes) -> Result<Refuting, Error> {
        let spec = self.reader.parse(&r.snapshot_id)?;
        let entry = self.catalog.entry(&spec).await?;
        let meta = self.catalog.module(&entry.module).await?;
        // A hole may be a definition of a module the spec imports, or be
        // reached through one.
        let imported: Vec<String> = ids(&meta.modules)
            .filter(|m| **m != entry.module)
            .cloned()
            .collect();
        let records = self.catalog.records(&imported).await?;
        let closure: Vec<Declaration> = (records.into_values())
            .flat_map(|r| r.result.declarations)
            .collect();
        let decls = &meta.result.declarations;
        let target = refutable(decls, &closure, &entry.holes, &r.target)?.clone();
        Ok(Refuting {
            spec,
            spec_module: entry.module,
            target,
        })
    }

    /// What a create for module `id`, which needs the snapshot modules
    /// `needs` (its own last) and has declarations `decls`, finds of the
    /// spec it amends: what changed, each type with its signature before and
    /// now (`own` has the module's own), and the declarations it adds.
    pub(super) async fn amendment(
        &self,
        a: &Amending,
        holes: &[String],
        needs: &[String],
        id: &str,
        decls: &[Declaration],
        own: impl IntoIterator<Item = (String, String)>,
    ) -> Result<crate::catalog::Amendment, Error> {
        let swapped = self.reader.swapped(&a.amended, needs, id).await?;
        let changes = match compare(&a.amended, decls, &swapped) {
            Ok(_) => Vec::new(),
            Err(mut changes) => {
                let gained: Vec<String> = swapped.gained.iter().map(|(m, _)| m.clone()).collect();
                let mut now = self.reader.exports_of(&gained).await;
                now.extend(own);
                self.reader
                    .sign(&mut changes, &a.amended, &swapped, &now)
                    .await;
                changes
            }
        };
        let had: HashSet<String> = (a.amended.declarations.iter())
            .map(|d| normalised(&d.name).into_owned())
            .collect();
        let added = decls
            .iter()
            .filter(|d| !d.generated && !had.contains(normalised(&d.name).as_ref()))
            .map(|d| d.name.clone())
            .collect();
        let holes = (a.holes != holes).then(|| AmendedHoles {
            before: a.holes.clone(),
            after: holes.to_vec(),
        });
        Ok(crate::catalog::Amendment {
            amends: a.amended.entry.clone(),
            reason: a.reason.clone(),
            changes,
            added,
            holes,
        })
    }
}
