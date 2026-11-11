//! The records of tactic sessions, their states and open documents, by id,
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
    /// The time limit in seconds its open or last edit took, which a reopen
    /// takes too.
    pub timeout: u64,
}

/// A tactic session's state: one of its `sorry`s, or a tactic applied to
/// another state. A worker replays the chain from the `sorry` on Lean Beam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRow {
    /// The session's document.
    pub session: String,
    /// For a session's first states, the index of their `sorry`.
    pub sorry: Option<u32>,
    /// For the others, the state the tactic was applied to.
    pub parent: Option<String>,
    pub tactic: Option<String>,
}

pub struct Records {
    db: Db,
    version: String,
}

impl Records {
    pub fn new(db: Db, version: String) -> Records {
        Records { db, version }
    }

    /// Stores state `id`, a step of a tactic session, at `created` (seconds
    /// since the Unix epoch). A state's id comes from what it is, so storing
    /// it again only renews it.
    pub async fn put_state(&self, id: &str, state: &StateRow, created: u64) -> Result<(), DbError> {
        dispatch!(&self.db, put_state(&self.version, id, state, created))
    }

    pub async fn state(&self, id: &str) -> Result<Option<StateRow>, DbError> {
        dispatch!(&self.db, state(&self.version, id))
    }

    /// Stores session `id`'s record, created at `created` (seconds since the
    /// Unix epoch), in place of any it had.
    pub async fn put_session(&self, id: &str, created: u64, meta: &Value) -> Result<(), DbError> {
        dispatch!(&self.db, put_session(&self.version, id, created, meta))
    }

    pub async fn session(&self, id: &str) -> Result<Option<Value>, DbError> {
        dispatch!(&self.db, session(&self.version, id))
    }

    /// Stores document `id` at revision 1, opened within `timeout` seconds
    /// on worker `holder` at `now` (seconds since the Unix epoch), in place
    /// of any it had.
    pub async fn put_document(
        &self,
        id: &str,
        code: &str,
        timeout: u64,
        holder: &str,
        now: u64,
    ) -> Result<(), DbError> {
        dispatch!(
            &self.db,
            put_document(&self.version, id, code, timeout, holder, now)
        )
    }

    pub async fn document(&self, id: &str) -> Result<Option<DocumentRow>, DbError> {
        dispatch!(&self.db, document(&self.version, id))
    }

    /// Replaces document `id`'s code if it is at `revision`, as edited
    /// within `timeout` seconds on `holder` at `now`. Returns the new
    /// revision, or `None` if the document is at another or gone.
    pub async fn edit_document(
        &self,
        id: &str,
        code: &str,
        timeout: u64,
        revision: u64,
        holder: &str,
        now: u64,
    ) -> Result<Option<u64>, DbError> {
        dispatch!(
            &self.db,
            edit_document(&self.version, id, code, timeout, revision, holder, now)
        )
    }

    /// Records that `holder` has document `id` open and used it at `now`;
    /// returns whether the document is still there.
    pub async fn hold_document(&self, id: &str, holder: &str, now: u64) -> Result<bool, DbError> {
        dispatch!(&self.db, hold_document(&self.version, id, holder, now))
    }

    /// Records that `holder` no longer has document `id` open, unless
    /// another worker holds it by now, so that any worker may open it.
    pub async fn release_document(&self, id: &str, holder: &str) -> Result<(), DbError> {
        dispatch!(&self.db, release_document(&self.version, id, holder))
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
            r.put_document("d", "a", 30, "w1", 10).await.unwrap();
            let row = |code: &str, revision, holder: &str, timeout| DocumentRow {
                code: code.into(),
                revision,
                holder: Some(holder.into()),
                timeout,
            };
            assert_eq!(r.document("d").await.unwrap(), Some(row("a", 1, "w1", 30)));
            assert_eq!(
                r.edit_document("d", "b", 90, 1, "w2", 11).await.unwrap(),
                Some(2)
            );
            // A stale revision changes nothing.
            assert_eq!(
                r.edit_document("d", "c", 5, 1, "w3", 12).await.unwrap(),
                None
            );
            assert_eq!(r.document("d").await.unwrap(), Some(row("b", 2, "w2", 90)));
            assert!(r.hold_document("d", "w3", 13).await.unwrap());
            assert_eq!(r.document("d").await.unwrap(), Some(row("b", 2, "w3", 90)));
            // Only its holder lets it go.
            r.release_document("d", "w2").await.unwrap();
            assert_eq!(r.document("d").await.unwrap(), Some(row("b", 2, "w3", 90)));
            r.release_document("d", "w3").await.unwrap();
            assert_eq!(r.document("d").await.unwrap().unwrap().holder, None);

            assert!(r.delete_document("d").await.unwrap());
            assert!(!r.delete_document("d").await.unwrap());
            assert_eq!(r.document("d").await.unwrap(), None);
            assert!(!r.hold_document("d", "w3", 14).await.unwrap());
            assert_eq!(
                r.edit_document("d", "c", 5, 2, "w3", 14).await.unwrap(),
                None
            );
        }
    }

    #[tokio::test]
    async fn documents_sessions_and_states_keep_to_their_scope() {
        use crate::scope::{Scope, within};
        for (_, r, _) in records().await {
            let a = Scope::parse("user=a").unwrap();
            let state = StateRow {
                session: "s".into(),
                sorry: Some(0),
                parent: None,
                tactic: None,
            };
            within(a.clone(), async {
                r.put_document("d", "code", 10, "w", 1).await.unwrap();
                r.put_session("s", 1, &serde_json::json!({})).await.unwrap();
                r.put_state("t", &state, 1).await.unwrap();
                assert!(r.document("d").await.unwrap().is_some());
            })
            .await;
            within(Scope::parse("user=b").unwrap(), async {
                assert!(r.document("d").await.unwrap().is_none());
                assert!(r.session("s").await.unwrap().is_none());
                assert!(r.state("t").await.unwrap().is_none());
                assert!(!r.delete_document("d").await.unwrap());
            })
            .await;
            within(a, async {
                assert!(r.session("s").await.unwrap().is_some());
                assert_eq!(r.state("t").await.unwrap(), Some(state.clone()));
            })
            .await;
            assert!(r.document("d").await.unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn a_state_is_a_sorry_or_a_tactic_on_another_state() {
        for (db, r, version) in records().await {
            let root = StateRow {
                session: "s".into(),
                sorry: Some(0),
                parent: None,
                tactic: None,
            };
            let step = StateRow {
                session: "s".into(),
                sorry: None,
                parent: Some("a".into()),
                tactic: Some("simp".into()),
            };
            r.put_state("a", &root, 10).await.unwrap();
            r.put_state("b", &step, 10).await.unwrap();
            // Storing it again only renews it.
            r.put_state("b", &step, 20).await.unwrap();
            assert_eq!(r.state("a").await.unwrap(), Some(root));
            assert_eq!(r.state("b").await.unwrap(), Some(step));
            assert_eq!(r.state("c").await.unwrap(), None);
            assert_eq!(r.sweep(15).await.unwrap(), 1);
            assert_eq!(db.rows(&version).await.unwrap(), [["states", "b"]]);
        }
    }

    #[tokio::test]
    async fn the_sweep_takes_documents_unused_since_the_cutoff() {
        for (db, r, version) in records().await {
            r.put_document("old", "a", 60, "w", 100).await.unwrap();
            r.put_document("used", "a", 60, "w", 100).await.unwrap();
            r.hold_document("used", "w", 200).await.unwrap();
            assert_eq!(r.sweep(150).await.unwrap(), 1);
            assert_eq!(db.rows(&version).await.unwrap(), [["documents", "used"]]);
        }
    }
}
