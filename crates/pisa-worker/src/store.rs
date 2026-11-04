//! Pickled proof states in object storage; the records that make them loadable,
//! and sessions, in the database.
//!
//! Object storage, per pool version:
//! - `states/<version>/<hash>.olean.zst`: a pickled proof state, zstd-compressed,
//!   addressed by the SHA-256 of the compressed bytes
//!
//! Database [`Records`], per pool version:
//! - state `<hash>`: what is needed to load a state ([`StateMeta`])
//! - session `<id>`: the session's code and declarations ([`SessionMeta`])

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pisa_protocol::verify::Declaration;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::Db;
use crate::records::Records;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMeta {
    pub header: String,
    pub session: String,
    pub parent: Option<String>,
    pub tactic: Option<String>,
    pub goals: Vec<String>,
    /// Seconds since the Unix epoch.
    pub created: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub code: String,
    /// Declarations of the original code, for the statement check.
    pub declarations: Vec<Declaration>,
    /// Seconds since the Unix epoch.
    pub created: u64,
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<dyn ObjectStore>,
    records: Arc<Records>,
    version: String,
    /// This worker, recorded as the holder of the states it stores.
    worker: String,
}

impl Store {
    pub fn new(inner: Arc<dyn ObjectStore>, db: Db, version: String, worker: String) -> Self {
        let records = Arc::new(Records::new(db, version.clone()));
        Store {
            inner,
            records,
            version,
            worker,
        }
    }

    fn pickle_path(&self, hash: &str) -> Path {
        Path::from(format!("states/{}/{hash}.olean.zst", self.version))
    }

    /// Stores a pickled state and returns its content hash.
    pub async fn put_state(&self, pickle: &[u8], meta: &StateMeta) -> anyhow::Result<String> {
        let compressed = zstd::encode_all(pickle, 3)?;
        let hash = hex::encode(&Sha256::digest(&compressed)[..16]);
        // Write the record last: a state is loadable once its record exists.
        self.inner
            .put(&self.pickle_path(&hash), PutPayload::from(compressed))
            .await?;
        let record = serde_json::to_value(meta)?;
        self.records
            .put_state(&hash, meta.created, &record, &self.worker)
            .await?;
        Ok(hash)
    }

    /// The state's record, `None` if there is none. An error is the
    /// database's ([`DbError`](crate::db::DbError)) or an unreadable record.
    pub async fn state_meta(&self, hash: &str) -> anyhow::Result<Option<StateMeta>> {
        let record = self.records.state(hash).await?;
        Ok(record.map(serde_json::from_value).transpose()?)
    }

    /// The state's decompressed pickle, or `None` if it has been deleted.
    pub async fn state_pickle(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let bytes = match self.inner.get(&self.pickle_path(hash)).await {
            Ok(result) => result.bytes().await?,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(zstd::decode_all(&bytes[..])?))
    }

    pub async fn put_session(&self, id: &str, meta: &SessionMeta) -> anyhow::Result<()> {
        let record = serde_json::to_value(meta)?;
        self.records.put_session(id, meta.created, &record).await?;
        Ok(())
    }

    /// The session's record, `None` if there is none; errors as for
    /// [`state_meta`](Self::state_meta).
    pub async fn session(&self, id: &str) -> anyhow::Result<Option<SessionMeta>> {
        let record = self.records.session(id).await?;
        Ok(record.map(serde_json::from_value).transpose()?)
    }

    /// Deletes states and sessions of this version older than `ttl`.
    pub async fn sweep(&self, ttl: Duration) -> anyhow::Result<usize> {
        let cutoff = now_secs().saturating_sub(ttl.as_secs());
        let mut deleted = self.records.sweep(cutoff).await?;

        let prefix = Path::from(format!("states/{}", self.version));
        let mut objects = self.inner.list(Some(&prefix));
        while let Some(meta) = objects.next().await {
            let meta = meta?;
            if meta.last_modified.timestamp() < cutoff as i64 {
                self.inner.delete(&meta.location).await?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    fn state(created: u64) -> StateMeta {
        StateMeta {
            header: "import Mathlib".into(),
            session: "s".into(),
            parent: None,
            tactic: None,
            goals: vec!["⊢ True".into()],
            created,
        }
    }

    fn session(created: u64) -> SessionMeta {
        SessionMeta {
            code: "x".into(),
            declarations: vec![],
            created,
        }
    }

    async fn object_paths(s3: &InMemory) -> Vec<String> {
        s3.list(None)
            .map(|meta| meta.unwrap().location.to_string())
            .collect()
            .await
    }

    #[tokio::test]
    async fn states_round_trip_and_sweep() {
        let s3 = Arc::new(InMemory::new());
        let store = Store::new(s3.clone(), Db::memory().await, "v1".into(), "w".into());
        let meta = state(now_secs());
        let pickle = b"olean bytes".repeat(100);
        let hash = store.put_state(&pickle, &meta).await.unwrap();
        assert_eq!(store.put_state(&pickle, &meta).await.unwrap(), hash);
        assert_eq!(store.state_pickle(&hash).await.unwrap(), Some(pickle));
        let stored = store.state_meta(&hash).await.unwrap().unwrap();
        assert_eq!(stored.goals, meta.goals);

        store.put_session("s", &session(now_secs())).await.unwrap();
        assert_eq!(store.session("s").await.unwrap().unwrap().code, "x");
        assert_eq!(
            object_paths(&s3).await,
            [format!("states/v1/{hash}.olean.zst")]
        );

        assert_eq!(store.sweep(Duration::from_secs(3600)).await.unwrap(), 0);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(store.sweep(Duration::ZERO).await.unwrap(), 3);
        assert!(store.state_meta(&hash).await.unwrap().is_none());
        assert!(store.session("s").await.unwrap().is_none());
        assert_eq!(store.state_pickle(&hash).await.unwrap(), None);
        assert!(object_paths(&s3).await.is_empty());
    }

    #[tokio::test]
    async fn storing_a_state_again_keeps_one_record() {
        let db = Db::memory().await;
        let store = Store::new(
            Arc::new(InMemory::new()),
            db.clone(),
            "v1".into(),
            "w".into(),
        );
        let pickle = b"olean bytes".repeat(100);
        let hash = store.put_state(&pickle, &state(1)).await.unwrap();
        store.put_state(&pickle, &state(2)).await.unwrap();

        assert_eq!(db.rows("v1").await.unwrap(), [["states", hash.as_str()]]);
        assert_eq!(store.state_meta(&hash).await.unwrap().unwrap().created, 2);
    }

    #[tokio::test]
    async fn sweep_deletes_only_old_records() {
        let store = Store::new(
            Arc::new(InMemory::new()),
            Db::memory().await,
            "v1".into(),
            "w".into(),
        );
        for i in 0..300 {
            store
                .put_session(&format!("old{i}"), &session(1))
                .await
                .unwrap();
        }
        store
            .put_session("fresh", &session(now_secs()))
            .await
            .unwrap();
        assert_eq!(store.sweep(Duration::from_secs(3600)).await.unwrap(), 300);
        assert!(store.session("old299").await.unwrap().is_none());
        assert_eq!(store.session("fresh").await.unwrap().unwrap().code, "x");
    }

    /// A missing record is `None`, not an error: errors are the store's.
    #[tokio::test]
    async fn missing_records_are_none() {
        let store = Store::new(
            Arc::new(InMemory::new()),
            Db::memory().await,
            "v1".into(),
            "w".into(),
        );
        assert!(store.state_meta("0f").await.unwrap().is_none());
        assert!(store.session("s").await.unwrap().is_none());
    }
}
