//! Pickled proof states in object storage; the records that make them loadable,
//! and sessions, in the KV store.
//!
//! Object storage, per pool version:
//! - `states/<version>/<hash>.olean.zst`: a pickled proof state, zstd-compressed,
//!   addressed by the SHA-256 of the compressed bytes
//!
//! KV keys, per pool version (every key starts with [`ROOT`]):
//! - `(pisa, <version>, "state", <hash>)`: what is needed to load a state
//!   ([`StateMeta`])
//! - `(pisa, <version>, "session", <id>)`: the session's code and declarations
//!   ([`SessionMeta`])
//! - `(pisa, <version>, "age", <created>, <kind>, <id>)`: empty; orders records
//!   by creation time so that [`Store::sweep`] finds the expired ones. `created`
//!   is 16 hex digits and `kind` is `state` or `session`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use futures::StreamExt;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pisa_protocol::verify::Declaration;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::kv::{self, Kv, KvError, ROOT, decode, key};

/// Records deleted per transaction when sweeping.
const SWEEP_BATCH: u32 = 256;

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
    kv: Kv,
    version: String,
}

impl Store {
    pub fn new(inner: Arc<dyn ObjectStore>, kv: Kv, version: String) -> Self {
        Store { inner, kv, version }
    }

    fn pickle_path(&self, hash: &str) -> Path {
        Path::from(format!("states/{}/{hash}.olean.zst", self.version))
    }

    fn state_key(&self, hash: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "state", hash])
    }

    fn session_key(&self, id: &str) -> Vec<u8> {
        key(&[ROOT, &self.version, "session", id])
    }

    fn age_key(&self, created: u64, kind: &str, id: &str) -> Vec<u8> {
        key(&[
            ROOT,
            &self.version,
            "age",
            &format!("{created:016x}"),
            kind,
            id,
        ])
    }

    /// Stores a pickled state and returns its content hash.
    pub async fn put_state(&self, pickle: &[u8], meta: &StateMeta) -> anyhow::Result<String> {
        let compressed = zstd::encode_all(pickle, 3)?;
        let hash = hex::encode(&Sha256::digest(&compressed)[..16]);
        // Write the record last: a state is loadable once its record exists.
        self.inner
            .put(&self.pickle_path(&hash), PutPayload::from(compressed))
            .await?;
        let record = serde_json::to_vec(meta)?;
        let state_key = self.state_key(&hash);
        let age_key = self.age_key(meta.created, "state", &hash);
        kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            if let Some(old) = txn.get(&state_key).await? {
                // A stored state gets a new age, so the old key must go.
                if let Ok(old) = serde_json::from_slice::<StateMeta>(&old) {
                    txn.delete(self.age_key(old.created, "state", &hash))
                        .await?;
                }
            }
            txn.put(state_key.clone(), record.clone()).await?;
            txn.put(age_key.clone(), Vec::new()).await?;
            txn.commit().await
        })
        .await?;
        Ok(hash)
    }

    pub async fn state_meta(&self, hash: &str) -> anyhow::Result<StateMeta> {
        self.get_record(self.state_key(hash), "state", hash).await
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
        let record = serde_json::to_vec(meta)?;
        let session_key = self.session_key(id);
        let age_key = self.age_key(meta.created, "session", id);
        kv::retry(|| async {
            let mut txn = self.kv.begin().await?;
            txn.put(session_key.clone(), record.clone()).await?;
            txn.put(age_key.clone(), Vec::new()).await?;
            txn.commit().await
        })
        .await?;
        Ok(())
    }

    pub async fn session(&self, id: &str) -> anyhow::Result<SessionMeta> {
        self.get_record(self.session_key(id), "session", id).await
    }

    async fn get_record<T: serde::de::DeserializeOwned>(
        &self,
        key: Vec<u8>,
        kind: &str,
        id: &str,
    ) -> anyhow::Result<T> {
        let value = kv::retry(|| async { self.kv.begin().await?.get(&key).await }).await?;
        let value = value.with_context(|| format!("no {kind} {id}"))?;
        Ok(serde_json::from_slice(&value)?)
    }

    /// Deletes states and sessions of this version older than `ttl`.
    pub async fn sweep(&self, ttl: Duration) -> anyhow::Result<usize> {
        let cutoff = now_secs().saturating_sub(ttl.as_secs());
        let start = key(&[ROOT, &self.version, "age"]);
        let end = key(&[ROOT, &self.version, "age", &format!("{cutoff:016x}")]);
        let mut deleted = 0;
        loop {
            let batch = kv::retry(|| async {
                let mut txn = self.kv.begin().await?;
                let aged = txn.scan(start.clone()..end.clone(), SWEEP_BATCH).await?;
                for (age_key, _) in &aged {
                    if let [.., kind, id] = decode(age_key).as_slice() {
                        let record = if kind == "state" {
                            self.state_key(id)
                        } else {
                            self.session_key(id)
                        };
                        txn.delete(record).await?;
                    }
                    txn.delete(age_key.clone()).await?;
                }
                txn.commit().await?;
                Ok::<_, KvError>(aged.len())
            })
            .await?;
            deleted += batch;
            if batch < SWEEP_BATCH as usize {
                break;
            }
        }

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
    use crate::kv::{decode, prefix};
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
        let store = Store::new(s3.clone(), Kv::memory(), "v1".into());
        let meta = state(now_secs());
        let pickle = b"olean bytes".repeat(100);
        let hash = store.put_state(&pickle, &meta).await.unwrap();
        assert_eq!(store.put_state(&pickle, &meta).await.unwrap(), hash);
        assert_eq!(store.state_pickle(&hash).await.unwrap(), Some(pickle));
        assert_eq!(store.state_meta(&hash).await.unwrap().goals, meta.goals);

        store.put_session("s", &session(now_secs())).await.unwrap();
        assert_eq!(store.session("s").await.unwrap().code, "x");
        assert_eq!(
            object_paths(&s3).await,
            [format!("states/v1/{hash}.olean.zst")]
        );

        assert_eq!(store.sweep(Duration::from_secs(3600)).await.unwrap(), 0);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(store.sweep(Duration::ZERO).await.unwrap(), 3);
        assert!(store.state_meta(&hash).await.is_err());
        assert!(store.session("s").await.is_err());
        assert_eq!(store.state_pickle(&hash).await.unwrap(), None);
        assert!(object_paths(&s3).await.is_empty());
    }

    #[tokio::test]
    async fn storing_a_state_again_keeps_one_age_key() {
        let kv = Kv::memory();
        let store = Store::new(Arc::new(InMemory::new()), kv.clone(), "v1".into());
        let pickle = b"olean bytes".repeat(100);
        let hash = store.put_state(&pickle, &state(1)).await.unwrap();
        store.put_state(&pickle, &state(2)).await.unwrap();

        let mut txn = kv.begin().await.unwrap();
        let ages = txn.scan(prefix(&[ROOT, "v1", "age"]), 10).await.unwrap();
        assert_eq!(ages.len(), 1);
        assert_eq!(
            decode(&ages[0].0),
            [ROOT, "v1", "age", &format!("{:016x}", 2), "state", &hash]
        );
    }

    #[tokio::test]
    async fn sweep_deletes_in_batches() {
        let store = Store::new(Arc::new(InMemory::new()), Kv::memory(), "v1".into());
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
        assert!(store.session("old299").await.is_err());
        assert_eq!(store.session("fresh").await.unwrap().code, "x");
    }
}
