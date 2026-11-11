//! Deleting a snapshot and the modules nothing needs any more.

use super::*;

impl Snapshots {
    /// Deletes an entry, or with `with_replacers` it and every entry that
    /// replaces it, directly or through others, then each module that
    /// nothing needs any more: the entries' own, then up the chain of
    /// imports. Once the entries are gone, the rest is best effort: what
    /// fails is logged, and a module whose record went is listed even if its
    /// files couldn't be deleted.
    pub async fn delete(
        &self,
        entry: &str,
        with_replacers: bool,
    ) -> Result<DeleteSnapshotResponse, Error> {
        let entry = self.parse(entry)?;
        let (deleted, cleaned) = if with_replacers {
            self.catalog.remove_branch(&entry).await?
        } else {
            let (entry_meta, cleaned) = self.catalog.remove_entry(&entry).await?;
            let cleaned = cleaned.map(|m| (entry_meta.module, m));
            (vec![entry], cleaned.into_iter().collect())
        };
        let removed = self.reap(cleaned).await;
        let deleted = deleted.iter().map(|entry| self.id(entry)).collect();
        Ok(DeleteSnapshotResponse { removed, deleted })
    }

    /// Removes the files of the modules whose records `cleaned` removed,
    /// then each import that nothing needs any more, the same way; returns
    /// the modules removed. Best effort: what fails is logged, and the
    /// [sweep](Self::sweep) clears what is left.
    async fn reap(&self, cleaned: Vec<(String, ModuleMeta)>) -> Vec<String> {
        let mut queue = VecDeque::from(cleaned);
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
        removed
    }

    /// Clears what deletes that stopped partway left: the modules no
    /// snapshot is on and no module imports, as a delete removes them, then
    /// the files in the store that no module's record names, once they are
    /// older than `grace`, as a build uploads its files before its record.
    /// A module is only cleared once it too is older than `grace`, as a build
    /// whose create was refused is kept for the create that follows.
    /// Returns how many modules went, and what came of the files.
    pub async fn sweep(&self, grace: Duration) -> Result<(usize, FileSweep), Error> {
        let built_by = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .saturating_sub(grace)
            .as_secs();
        let mut modules = 0;
        let mut after: Option<String> = None;
        loop {
            let (orphans, next) = self
                .catalog
                .orphans_after(after.as_deref(), SWEEP_PAGE, built_by)
                .await?;
            for id in orphans {
                if let Some(meta) = self.catalog.cleanup(&id).await? {
                    modules += self.reap(vec![(id, meta)]).await.len();
                }
            }
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        Ok((modules, self.sweep_files(grace).await?))
    }

    /// Deletes the files of module builds that no record names, older than
    /// `grace`. Only in a store whose marker names this catalog, and not
    /// when most of the files would go: a catalog that isn't the store's has
    /// no record of its files. A store that names no catalog has nothing to
    /// sweep yet, unless it holds files this catalog can't account for, as
    /// a claim would find them.
    async fn sweep_files(&self, grace: Duration) -> Result<FileSweep, Error> {
        let ours = self.catalog.identity().await?;
        match self.store_catalog().await? {
            None => {
                let (_, unrecorded) = self.unrecorded(grace.min(CLAIM_GRACE)).await?;
                return Ok(match unrecorded.first() {
                    None => FileSweep::Swept(0),
                    Some(first) => FileSweep::Refused(format!(
                        "the store names no catalog for this version, and {}",
                        unaccounted(unrecorded.len(), first)
                    )),
                });
            }
            Some(theirs) if theirs != ours => {
                return Ok(FileSweep::Refused(format!(
                    "the store's snapshot files are catalog {theirs}'s, not this one's ({ours})"
                )));
            }
            Some(_) => {}
        }
        let (listed, unrecorded) = self.unrecorded(grace).await?;
        if unrecorded.len() >= SWEEP_GUARD && unrecorded.len() * 2 > listed {
            return Ok(FileSweep::Refused(format!(
                "{} of the store's {listed} snapshot files have no record",
                unrecorded.len()
            )));
        }
        let mut deleted = 0;
        for location in unrecorded {
            match self.store.delete(&location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => deleted += 1,
                Err(e) => {
                    tracing::warn!(path = %location, "sweeping a snapshot file failed: {e}")
                }
            }
        }
        Ok(FileSweep::Swept(deleted))
    }

    /// The files of this version in the store older than `grace`, counted,
    /// and those of them whose build no module's record names.
    async fn unrecorded(&self, grace: Duration) -> Result<(usize, Vec<ObjectPath>), Error> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let prefix = ObjectPath::from(format!("snapshots/{}", self.version));
        let listed: Vec<_> = futures::StreamExt::collect(self.store.list(Some(&prefix))).await;
        // `<id>.<build>.<ext>.zst`.
        let mut old = Vec::new();
        for meta in listed {
            let meta = meta.map_err(internal)?;
            if now - meta.last_modified.timestamp() < grace.as_secs() as i64 {
                continue;
            }
            let Some(name) = meta.location.filename() else {
                continue;
            };
            let mut parts = name.split('.');
            if let (Some(id), Some(build)) = (parts.next(), parts.next()) {
                let (id, build) = (id.to_string(), build.to_string());
                old.push((meta.location, id, build));
            }
        }
        // Each module's own build, if it has a record.
        let ids: BTreeSet<&String> = old.iter().map(|(_, id, _)| id).collect();
        let ids: Vec<String> = ids.into_iter().cloned().collect();
        let mut builds = HashMap::new();
        for page in ids.chunks(SWEEP_PAGE as usize) {
            builds.extend(self.catalog.own_builds(page).await?);
        }
        let listed = old.len();
        let unrecorded = old
            .into_iter()
            .filter(|(_, id, build)| builds.get(id) != Some(build))
            .map(|(location, ..)| location)
            .collect();
        Ok((listed, unrecorded))
    }
}

/// Why a catalog doesn't claim a store that holds `n` files of builds it has
/// no record of, `first` among them.
fn unaccounted(n: usize, first: &ObjectPath) -> String {
    format!(
        "the store holds {n} files of builds this catalog has no record of, such as {first}; \
         if this catalog should sweep them anyway, start a worker with PISA_CLAIM_STORE=1"
    )
}

/// What a sweep of the store's files came to.
#[derive(Debug, PartialEq, Eq)]
pub enum FileSweep {
    /// How many files went.
    Swept(usize),
    /// Why none were looked at.
    Refused(String),
}

/// What a worker's claim on the store came to.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Claim {
    /// The marker names this catalog.
    Ours,
    /// It names this other one.
    Theirs(String),
    /// It names none, and this catalog didn't claim it, for this reason.
    Refused(String),
}

/// The modules the sweep looks at per query.
const SWEEP_PAGE: u32 = 500;

/// The fewest files with no record that, as most of the store's, stop a
/// sweep.
pub(super) const SWEEP_GUARD: usize = 16;

/// How old a file of a build with no record has to be to stop a claim on
/// the store: newer ones are other workers' uploads, whose records come
/// after.
const CLAIM_GRACE: Duration = Duration::from_secs(3600);

impl Snapshots {
    /// Where the store names the catalog whose modules it holds.
    fn marker(&self) -> ObjectPath {
        ObjectPath::from(format!("catalogs/{}", self.version))
    }

    /// The catalog the store's marker names, if it has one.
    pub(super) async fn store_catalog(&self) -> Result<Option<String>, Error> {
        match self.store.get(&self.marker()).await {
            Ok(got) => {
                let bytes = got.bytes().await.map_err(internal)?;
                Ok(Some(String::from_utf8_lossy(&bytes).trim().to_string()))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(internal(e)),
        }
    }

    /// Has the store's marker name this catalog, unless it names one
    /// already ([`claim`](Self::claim)): once per worker, before its first
    /// upload. A store that names another catalog, or that this one doesn't
    /// claim, keeps its marker, and this worker's sweeps leave its files
    /// alone.
    pub(crate) async fn mark_store(&self) -> Result<(), Error> {
        if self.marked.load(Ordering::Relaxed) {
            return Ok(());
        }
        match self.claim(CLAIM_GRACE).await? {
            Claim::Ours => {}
            Claim::Theirs(theirs) => tracing::warn!(
                store = %theirs,
                "the store's snapshot files are another catalog's: this worker won't sweep them"
            ),
            Claim::Refused(why) => {
                tracing::warn!("this worker didn't claim the store, and won't sweep it: {why}")
            }
        }
        self.marked.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Reads the store's marker, and if it names no catalog, makes it name
    /// this one, unless the store holds files older than `grace` of builds
    /// that no module's record names: those are another catalog's, or this
    /// one has lost them. With [`claiming_store`](Self::claiming_store), the
    /// marker is made to name this catalog whatever it names.
    pub(super) async fn claim(&self, grace: Duration) -> Result<Claim, Error> {
        let ours = self.catalog.identity().await?;
        let payload = PutPayload::from(ours.clone().into_bytes());
        let theirs = self.store_catalog().await?;
        if self.claim_store {
            if theirs.as_deref() != Some(ours.as_str()) {
                self.store
                    .put(&self.marker(), payload)
                    .await
                    .map_err(internal)?;
                tracing::warn!(
                    store = ?theirs,
                    catalog = %ours,
                    "PISA_CLAIM_STORE: the store's marker now names this catalog"
                );
            }
            return Ok(Claim::Ours);
        }
        if let Some(theirs) = theirs {
            return Ok(match theirs == ours {
                true => Claim::Ours,
                false => Claim::Theirs(theirs),
            });
        }
        let (_, unrecorded) = self.unrecorded(grace).await?;
        if let Some(first) = unrecorded.first() {
            return Ok(Claim::Refused(unaccounted(unrecorded.len(), first)));
        }
        let put = self
            .store
            .put_opts(&self.marker(), payload.clone(), PutMode::Create.into())
            .await;
        match put {
            Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
            // A store without conditional writes: read, then write.
            Err(object_store::Error::NotImplemented { .. }) => {
                if self.store_catalog().await?.is_none() {
                    self.store
                        .put(&self.marker(), payload)
                        .await
                        .map_err(internal)?;
                }
            }
            Err(e) => return Err(internal(e)),
        }
        Ok(match self.store_catalog().await? {
            Some(theirs) if theirs == ours => Claim::Ours,
            Some(theirs) => Claim::Theirs(theirs),
            None => Claim::Refused("the store's marker didn't stay written".into()),
        })
    }
}
