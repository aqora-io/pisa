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
    /// Returns how many modules and files went.
    pub async fn sweep(&self, grace: Duration) -> Result<(usize, usize), Error> {
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
    /// `grace`; returns how many. Only in a store whose marker names this
    /// catalog, and not when most of the files would go: a catalog that
    /// isn't the store's has no record of its files.
    async fn sweep_files(&self, grace: Duration) -> Result<usize, Error> {
        let ours = self.catalog.identity().await?;
        match self.store_catalog().await? {
            None => {
                tracing::info!(
                    "the store names no catalog for this version yet, so its files aren't swept"
                );
                return Ok(0);
            }
            Some(theirs) if theirs != ours => {
                tracing::error!(
                    store = %theirs,
                    catalog = %ours,
                    "the store's snapshot files are another catalog's: not sweeping them"
                );
                return Ok(0);
            }
            Some(_) => {}
        }
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
        let unrecorded: Vec<ObjectPath> = old
            .into_iter()
            .filter(|(_, id, build)| builds.get(id) != Some(build))
            .map(|(location, ..)| location)
            .collect();
        if unrecorded.len() >= SWEEP_GUARD && unrecorded.len() * 2 > listed {
            tracing::error!(
                unrecorded = unrecorded.len(),
                listed,
                "most of the store's snapshot files have no record: not sweeping them"
            );
            return Ok(0);
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
        Ok(deleted)
    }
}

/// The modules the sweep looks at per query.
const SWEEP_PAGE: u32 = 500;

/// The fewest files with no record that, as most of the store's, stop a
/// sweep.
pub(super) const SWEEP_GUARD: usize = 16;

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

    /// Makes the store's marker name this catalog, unless it names one
    /// already: once per worker, before its first upload. A store that
    /// another catalog's worker marked first keeps its marker, and this
    /// worker's sweeps leave its files alone.
    pub(super) async fn mark_store(&self) -> Result<(), Error> {
        if self.marked.load(Ordering::Relaxed) {
            return Ok(());
        }
        let ours = self.catalog.identity().await?;
        let payload = PutPayload::from(ours.clone().into_bytes());
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
        let theirs = self.store_catalog().await?;
        if theirs.as_deref() != Some(ours.as_str()) {
            tracing::warn!(
                store = ?theirs,
                catalog = %ours,
                "the store's snapshot files are another catalog's: this worker won't sweep them"
            );
        }
        self.marked.store(true, Ordering::Relaxed);
        Ok(())
    }
}
