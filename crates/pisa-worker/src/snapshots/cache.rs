//! The worker's local cache of snapshot modules, fetched from the object
//! store and confirmed against the catalog.

use super::*;

/// How many of a closure's modules the cache fetches at once.
const FETCHES: usize = 16;

pub(super) fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Removes every file of module `s<id>` from `dir`.
pub(super) fn remove_module(dir: &Path, id: &str) {
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

impl Snapshots {
    /// Where the store keeps file `ext` of a module build.
    pub(super) fn blob(&self, module: &Module, ext: &str) -> ObjectPath {
        ObjectPath::from(format!(
            "snapshots/{}/{}.{}.{ext}.zst",
            self.version, module.id, module.build
        ))
    }

    pub(super) fn dir(&self) -> PathBuf {
        self.root.join(snapshot::ROOT)
    }

    pub(super) fn file(&self, id: &str, ext: &str) -> PathBuf {
        self.dir().join(format!("s{id}.{ext}"))
    }

    pub(crate) fn lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
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

    /// Why code on `header` can't be checked, if its snapshot imports
    /// declare one name twice (see [`Reader::clashes`]): Lean would take the
    /// first import's, and the code would mean one or the other by the order
    /// of its imports. A header that names no snapshot module, or one that
    /// isn't kept, is left to the import to report.
    pub async fn clash(&self, header: &str) -> Result<Option<Error>, Error> {
        if self.clean_headers.lock().unwrap().contains(header) {
            return Ok(None);
        }
        let Ok(imports) = snapshot::imports(header) else {
            return Ok(None);
        };
        let clash = self.reader.clashes(&imports).await?;
        if clash.is_none() {
            self.clean_headers
                .lock()
                .unwrap()
                .insert(header.to_string());
        }
        Ok(clash)
    }

    pub(super) async fn ensure_one(&self, id: &str) -> Result<(), Error> {
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
        // A few at a time: a library's closure can hold a thousand modules.
        for some in meta.modules.chunks(FETCHES) {
            futures::future::try_join_all(some.iter().map(|module| self.fetch(module))).await?;
        }
        self.confirmed
            .lock()
            .unwrap()
            .insert(id.to_string(), Instant::now());
        Ok(())
    }

    /// Downloads a module build into the local cache unless it is already
    /// there.
    pub(super) async fn fetch(&self, module: &Module) -> Result<(), Error> {
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
    pub(super) fn discard(&self, id: &str) {
        remove_module(&self.dir(), id);
        self.cached.lock().unwrap().remove(id);
        self.confirmed.lock().unwrap().remove(id);
    }

    /// Forgets a module that a delete removed, unless the same code has been
    /// stored again since, maybe by a build on this worker. Then the local
    /// copy may be the removed build, so the next import looks the module up
    /// again.
    pub(super) async fn forget(&self, id: &str) {
        let lock = self.lock(id);
        let _held = lock.lock().await;
        if self.catalog.module(id).await.is_err() {
            self.discard(id);
        } else {
            self.confirmed.lock().unwrap().remove(id);
        }
    }

    /// Deletes a module build's files from the store, logging any it can't.
    pub(super) async fn delete_blobs(&self, module: &Module) {
        for file in &module.files {
            let path = self.blob(module, &file.ext);
            match self.store.delete(&path).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => tracing::warn!(%path, "deleting a snapshot file failed: {e}"),
            }
        }
    }
}
