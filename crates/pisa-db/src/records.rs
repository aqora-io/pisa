//! Proof states' and sessions' records, by hash and by id, each with the
//! time it was stored, so that old ones can be swept.

use serde_json::Value;

use crate::backend::dispatch;
use crate::db::{Db, DbError};

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

    /// Deletes the states and sessions created before `cutoff` (seconds since
    /// the Unix epoch); returns how many.
    pub async fn sweep(&self, cutoff: u64) -> Result<usize, DbError> {
        dispatch!(&self.db, sweep(&self.version, cutoff))
    }
}
