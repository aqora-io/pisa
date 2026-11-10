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
        let deleted = deleted.iter().map(|entry| self.id(entry)).collect();
        Ok(DeleteSnapshotResponse { removed, deleted })
    }
}
