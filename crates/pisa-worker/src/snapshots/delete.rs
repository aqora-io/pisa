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
    /// Returns how many modules and files went.
    pub async fn sweep(&self, grace: Duration) -> Result<(usize, usize), Error> {
        let mut modules = 0;
        let mut after: Option<String> = None;
        loop {
            let (orphans, next) = self
                .catalog
                .orphans_after(after.as_deref(), SWEEP_PAGE)
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
    /// `grace`; returns how many.
    async fn sweep_files(&self, grace: Duration) -> Result<usize, Error> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let prefix = ObjectPath::from(format!("snapshots/{}", self.version));
        let listed: Vec<_> = futures::StreamExt::collect(self.store.list(Some(&prefix))).await;
        // Each module's own build, if it has a record.
        let mut builds: HashMap<String, Option<String>> = HashMap::new();
        let mut deleted = 0;
        for meta in listed {
            let meta = meta.map_err(internal)?;
            if now - meta.last_modified.timestamp() < grace.as_secs() as i64 {
                continue;
            }
            // `<id>.<build>.<ext>.zst`.
            let Some(name) = meta.location.filename() else {
                continue;
            };
            let mut parts = name.split('.');
            let (Some(id), Some(build)) = (parts.next(), parts.next()) else {
                continue;
            };
            if !builds.contains_key(id) {
                let own = match self.catalog.module(id).await {
                    Ok(m) => m.modules.last().map(|own| own.build.clone()),
                    Err(e) if e.kind == ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                };
                builds.insert(id.to_string(), own);
            }
            if builds[id].as_deref() == Some(build) {
                continue;
            }
            match self.store.delete(&meta.location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => deleted += 1,
                Err(e) => {
                    tracing::warn!(path = %meta.location, "sweeping a snapshot file failed: {e}")
                }
            }
        }
        Ok(deleted)
    }
}

/// The modules the sweep looks at per query.
const SWEEP_PAGE: u32 = 500;
