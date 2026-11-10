//! Proof states', sessions' and open documents' records, by hash and by id,
//! each with the time it was stored (a document's, when it was last used), so
//! that old ones can be swept.

use serde_json::Value;

use crate::backend::dispatch;
use crate::db::{Db, DbError};

/// An open document's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentRow {
    /// Its whole text.
    pub code: String,
    /// 1 when opened, then one more for each edit.
    pub revision: u64,
    /// The worker that has it open.
    pub holder: Option<String>,
}

pub struct Records {
    db: Db,
    version: String,
}

impl Records {
    pub fn new(db: Db, version: String) -> Records {
        Records { db, version }
    }

    /// Stores state `hash`'s record, created at `created` (seconds since the
    /// Unix epoch) by worker `holder`, in place of any it had. Jobs that
    /// continue the state prefer the holder, which may still have it loaded.
    pub async fn put_state(
        &self,
        hash: &str,
        created: u64,
        meta: &Value,
        holder: &str,
    ) -> Result<(), DbError> {
        dispatch!(
            &self.db,
            put_state(&self.version, hash, created, meta, holder)
        )
    }

    pub async fn state(&self, hash: &str) -> Result<Option<Value>, DbError> {
        dispatch!(&self.db, state(&self.version, hash))
    }

    /// Stores session `id`'s record, created at `created` (seconds since the
    /// Unix epoch), in place of any it had.
    pub async fn put_session(&self, id: &str, created: u64, meta: &Value) -> Result<(), DbError> {
        dispatch!(&self.db, put_session(&self.version, id, created, meta))
    }

    pub async fn session(&self, id: &str) -> Result<Option<Value>, DbError> {
        dispatch!(&self.db, session(&self.version, id))
    }

    /// Stores document `id` at revision 1, open on worker `holder` at `now`
    /// (seconds since the Unix epoch), in place of any it had.
    pub async fn put_document(
        &self,
        id: &str,
        code: &str,
        holder: &str,
        now: u64,
    ) -> Result<(), DbError> {
        dispatch!(&self.db, put_document(&self.version, id, code, holder, now))
    }

    pub async fn document(&self, id: &str) -> Result<Option<DocumentRow>, DbError> {
        dispatch!(&self.db, document(&self.version, id))
    }

    /// Replaces document `id`'s code if it is at `revision`, as edited on
    /// `holder` at `now`. Returns the new revision, or `None` if the document
    /// is at another or gone.
    pub async fn edit_document(
        &self,
        id: &str,
        code: &str,
        revision: u64,
        holder: &str,
        now: u64,
    ) -> Result<Option<u64>, DbError> {
        dispatch!(
            &self.db,
            edit_document(&self.version, id, code, revision, holder, now)
        )
    }

    /// Records that `holder` has document `id` open and used it at `now`;
    /// returns whether the document is still there.
    pub async fn hold_document(&self, id: &str, holder: &str, now: u64) -> Result<bool, DbError> {
        dispatch!(&self.db, hold_document(&self.version, id, holder, now))
    }

    /// Deletes document `id`; returns whether it was there.
    pub async fn delete_document(&self, id: &str) -> Result<bool, DbError> {
        dispatch!(&self.db, delete_document(&self.version, id))
    }

    /// Deletes the states and sessions created, and the documents last used,
    /// before `cutoff` (seconds since the Unix epoch); returns how many.
    pub async fn sweep(&self, cutoff: u64) -> Result<usize, DbError> {
        dispatch!(&self.db, sweep(&self.version, cutoff))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records of a version of their own on in-memory SQLite, and on the
    /// PostgreSQL at `PISA_TEST_DATABASE_URL` if that is set.
    async fn records() -> Vec<(Db, Records, String)> {
        let mut dbs = vec![Db::memory().await];
        if let Ok(url) = std::env::var("PISA_TEST_DATABASE_URL") {
            dbs.push(Db::connect(Some(&url)).await.unwrap());
        }
        dbs.into_iter()
            .map(|db| {
                let version = uuid::Uuid::new_v4().simple().to_string();
                (db.clone(), Records::new(db, version.clone()), version)
            })
            .collect()
    }

    #[tokio::test]
    async fn a_document_is_edited_only_at_its_revision() {
        for (_, r, _) in records().await {
            r.put_document("d", "a", "w1", 10).await.unwrap();
            let row = |code: &str, revision, holder: &str| DocumentRow {
                code: code.into(),
                revision,
                holder: Some(holder.into()),
            };
            assert_eq!(r.document("d").await.unwrap(), Some(row("a", 1, "w1")));
            assert_eq!(
                r.edit_document("d", "b", 1, "w2", 11).await.unwrap(),
                Some(2)
            );
            // A stale revision changes nothing.
            assert_eq!(r.edit_document("d", "c", 1, "w3", 12).await.unwrap(), None);
            assert_eq!(r.document("d").await.unwrap(), Some(row("b", 2, "w2")));
            assert!(r.hold_document("d", "w3", 13).await.unwrap());
            assert_eq!(r.document("d").await.unwrap(), Some(row("b", 2, "w3")));

            assert!(r.delete_document("d").await.unwrap());
            assert!(!r.delete_document("d").await.unwrap());
            assert_eq!(r.document("d").await.unwrap(), None);
            assert!(!r.hold_document("d", "w3", 14).await.unwrap());
            assert_eq!(r.edit_document("d", "c", 2, "w3", 14).await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn the_sweep_takes_documents_unused_since_the_cutoff() {
        for (db, r, version) in records().await {
            r.put_document("old", "a", "w", 100).await.unwrap();
            r.put_document("used", "a", "w", 100).await.unwrap();
            r.hold_document("used", "w", 200).await.unwrap();
            assert_eq!(r.sweep(150).await.unwrap(), 1);
            assert_eq!(db.rows(&version).await.unwrap(), [["documents", "used"]]);
        }
    }
}
