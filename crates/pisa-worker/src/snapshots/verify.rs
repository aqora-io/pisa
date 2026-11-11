//! The kernel re-check of snapshot modules (`POST /snapshots/{id}/verify`,
//! and at creation).

use super::*;

/// What re-checking one module of a chain came to.
#[derive(Debug, PartialEq)]
pub(super) enum Recheck {
    Passed,
    AlreadyVerified,
    /// The module was deleted and built again since the chain was read, or
    /// a module it needs lost `verified`, so this run doesn't verify it.
    Changed,
}

/// Why a module's re-check gave no declarations.
pub(super) enum Unchecked {
    /// The kernel rejected it, or its verdict is unreadable.
    Rejected(String),
    /// The re-check ran out of time or memory, or failed.
    Unfinished(Unfinished),
}

/// Re-checks the module through the kernel in a clean process, with the
/// slot's import time and then `per_module`, and returns its declarations
/// with their axioms from there, so that they don't depend on whatever ran
/// during the build.
pub(super) async fn recheck_module(
    slot: &Lease,
    id: &str,
    per_module: Duration,
) -> Result<Vec<ReportedDeclaration>, Unchecked> {
    let modules = [snapshot::module(id)];
    let mut run = slot
        .verify(&modules, slot.import_timeout(), per_module)
        .map_err(|e| Unchecked::Unfinished(Unfinished::Crash(e.to_string())))?;
    match run.next().await {
        Some(Ok(verdict)) => verified_declarations(verdict).map_err(Unchecked::Rejected),
        Some(Err(unfinished)) => Err(Unchecked::Unfinished(unfinished)),
        None => Err(Unchecked::Unfinished(Unfinished::Crash(
            "it gave no verdict".into(),
        ))),
    }
}

/// The kind of error a re-check that didn't finish is.
pub(super) fn unfinished_kind(unfinished: &Unfinished) -> ErrorKind {
    match unfinished {
        Unfinished::Timeout(_) => ErrorKind::Timeout,
        Unfinished::OutOfMemory => ErrorKind::OutOfMemory,
        Unfinished::Crash(_) => ErrorKind::Crash,
    }
}

/// Whether a re-check that didn't finish ran out of time or memory, which
/// says nothing about the code, rather than failed.
pub(super) fn outgrown(unfinished: &Unfinished) -> bool {
    matches!(unfinished, Unfinished::Timeout(_) | Unfinished::OutOfMemory)
}

/// The error of a create whose re-check didn't finish.
pub(super) fn unfinished_error(unfinished: &Unfinished) -> Error {
    Error::new(unfinished_kind(unfinished), unfinished.to_string())
}

/// A verdict's declarations, or why there are none: the kernel rejected the
/// module, or the verdict is unreadable.
pub(super) fn verified_declarations(verdict: Verdict) -> Result<Vec<ReportedDeclaration>, String> {
    if !verdict.ok {
        return Err(verdict.reason.unwrap_or_else(|| "snapshot rejected".into()));
    }
    parse_declarations(verdict.declarations)
        .ok_or_else(|| "unreadable verifier declarations".to_string())
}

impl Snapshots {
    /// Re-checks an entry's module and everything it imports through the
    /// kernel, with `per_module` for each module once their imports are
    /// loaded, and records each module that passes; `again` re-checks those
    /// that are verified already too, which describes them again.
    pub async fn verify(
        &self,
        pool: &Arc<Pool>,
        entry: &str,
        per_module: Duration,
        again: bool,
    ) -> Result<VerifySnapshotResponse, Error> {
        let entry = self.parse(entry)?;
        let id = self.catalog.entry(&entry).await?.module;
        let (checked, failure) = self.verify_chain(pool, &[id], per_module, again).await?;
        // As the entry and its module are now.
        let entry_meta = self.catalog.entry(&entry).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        Ok(VerifySnapshotResponse {
            snapshot: self.snapshot(entry, entry_meta, &meta),
            checked,
            failure,
        })
    }

    /// Re-checks the modules that the modules `ids` need, theirs included,
    /// in one run of the verifier. Each module's record lists imports before
    /// what imports them, so a module is only marked verified once its imports
    /// are. Stops at the first module that fails, or that was deleted and
    /// built again since: that one isn't verified this run, which isn't an
    /// error. Returns the modules that passed, in order, and the one that
    /// failed. Each module has `per_module` once their imports are loaded.
    ///
    /// This doesn't take the snapshots' locks, which every check on them
    /// needs: they shouldn't wait for the kernel, and two re-checks of a
    /// module agree.
    pub(super) async fn verify_chain(
        &self,
        pool: &Arc<Pool>,
        ids: &[String],
        per_module: Duration,
        again: bool,
    ) -> Result<(Vec<String>, Option<VerifyFailure>), Error> {
        let mut modules: Vec<Module> = Vec::new();
        for id in ids {
            self.ensure_one(id).await?;
            for module in self.catalog.module(id).await?.modules {
                if !modules.contains(&module) {
                    modules.push(module);
                }
            }
        }
        // Those this run re-checks, as far as the first built again.
        let mut todo = Vec::new();
        for module in modules {
            match self.before_recheck(&module, again).await? {
                // Whatever imports it was built again too, or is gone.
                Some(Recheck::Changed) => break,
                Some(_) => {}
                None => todo.push(module),
            }
        }
        // A module whose imports declare one name twice fails before the
        // kernel sees it, and the chain stops there: the re-check would
        // describe its names from whichever import came first. Creates refuse
        // such a header; this catches modules built before they did.
        let mut clashed = None;
        for (i, module) in todo.iter().enumerate() {
            let imports = self.catalog.module(&module.id).await?.imports;
            if let Some(e) = self.reader.clashes(&imports).await? {
                let module = snapshot::module(&module.id);
                clashed = Some((
                    i,
                    VerifyFailure {
                        module,
                        kind: e.kind,
                        reason: e.message,
                    },
                ));
                break;
            }
        }
        if let Some((i, _)) = &clashed {
            todo.truncate(*i);
        }
        let clashed = clashed.map(|(_, failure)| failure);
        let mut checked = Vec::new();
        if todo.is_empty() {
            return Ok((checked, clashed));
        }
        // No header to prefer: this takes a slot with nothing warm if it can.
        let mut slot = pool.acquire("").await;
        // The verifier imports everything again, so free the slot's memory first.
        slot.retire_and_wait("evict").await;
        let names: Vec<String> = todo.iter().map(|m| snapshot::module(&m.id)).collect();
        let mut run = match slot.verify(&names, slot.import_timeout(), per_module) {
            Ok(run) => run,
            Err(e) => {
                let failure = VerifyFailure {
                    module: names[0].clone(),
                    kind: ErrorKind::Crash,
                    reason: format!("the re-check failed to start: {e}"),
                };
                return Ok((checked, Some(failure)));
            }
        };
        let mut failure = None;
        for (module, name) in todo.iter().zip(names) {
            let verdict = match run.next().await {
                Some(Ok(verdict)) => {
                    verified_declarations(verdict).map_err(|reason| (ErrorKind::Lean, reason))
                }
                Some(Err(unfinished)) => {
                    Err((unfinished_kind(&unfinished), unfinished.to_string()))
                }
                None => Err((ErrorKind::Crash, "the re-check gave no verdict".into())),
            };
            let decls = match verdict {
                Ok(decls) => decls,
                Err((kind, reason)) => {
                    // The kernel rejects a module verified already, as
                    // `again` asks: it loses `verified`, and so does what
                    // builds on it.
                    if kind == ErrorKind::Lean {
                        let why = format!("the kernel re-check rejected it: {reason}");
                        self.catalog
                            .unverify(&module.id, &module.build, &why)
                            .await?;
                    }
                    failure = Some(VerifyFailure {
                        module: name,
                        kind,
                        reason,
                    });
                    break;
                }
            };
            let verification = slot.policy().judge_decls(false, Some(decls));
            match self.record_recheck(module, &verification).await? {
                Recheck::Passed => checked.push(name),
                _ => return Ok((checked, None)),
            }
        }
        Ok((checked, failure.or(clashed)))
    }

    /// Gets a re-check of a module build ready: `Changed` if the module's
    /// record names another build now, `AlreadyVerified` if this build is
    /// and it isn't to be checked `again`, else `None` once the local files,
    /// which the verifier reads, are this build's. They are fetched for a
    /// verified build too, as the next modules of the chain import it.
    pub(super) async fn before_recheck(
        &self,
        module: &Module,
        again: bool,
    ) -> Result<Option<Recheck>, Error> {
        let meta = self.catalog.module(&module.id).await?;
        if meta.modules.last().map(|own| &own.build) != Some(&module.build) {
            return Ok(Some(Recheck::Changed));
        }
        self.fetch(module).await?;
        Ok((meta.verified && !again).then_some(Recheck::AlreadyVerified))
    }

    /// Records that a module build passed the re-check, with what it found.
    /// A module removed meanwhile stays removed, and one built again keeps
    /// its record and is `Changed`.
    pub(super) async fn record_recheck(
        &self,
        module: &Module,
        verification: &Verification,
    ) -> Result<Recheck, Error> {
        let updated = self
            .catalog
            .update_module(&module.id, &module.build, |meta| {
                meta.verified = true;
                meta.status = worst_status(&verification.declarations);
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
}
