//! Pickled proof states and session records in object storage.
//!
//! Layout, per pool version:
//! - `states/<version>/<hash>.olean.zst`: a pickled proof state, zstd-compressed,
//!   addressed by the SHA-256 of the compressed bytes
//! - `states/<version>/<hash>.json`: what is needed to load it ([`StateMeta`])
//! - `sessions/<version>/<id>.json`: the session's code and declarations

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use futures::StreamExt;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pisa_protocol::verify::Declaration;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMeta {
    pub header: String,
    pub session: String,
    pub parent: Option<String>,
    pub tactic: Option<String>,
    pub goals: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub code: String,
    /// Declarations of the original code, for the statement check.
    pub declarations: Vec<Declaration>,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<dyn ObjectStore>,
    version: String,
}

impl Store {
    pub fn new(inner: Arc<dyn ObjectStore>, version: String) -> Self {
        Store { inner, version }
    }

    fn state_path(&self, hash: &str, ext: &str) -> Path {
        Path::from(format!("states/{}/{hash}.{ext}", self.version))
    }

    /// Stores a pickled state and returns its content hash.
    pub async fn put_state(&self, pickle: &[u8], meta: &StateMeta) -> anyhow::Result<String> {
        let compressed = zstd::encode_all(pickle, 3)?;
        let hash = hex::encode(&Sha256::digest(&compressed)[..16]);
        // Write the metadata last: a state is loadable once its metadata exists.
        self.inner
            .put(
                &self.state_path(&hash, "olean.zst"),
                PutPayload::from(compressed),
            )
            .await?;
        self.put_json(&self.state_path(&hash, "json"), meta).await?;
        Ok(hash)
    }

    pub async fn state_meta(&self, hash: &str) -> anyhow::Result<StateMeta> {
        self.get_json(&self.state_path(hash, "json")).await
    }

    pub async fn state_pickle(&self, hash: &str) -> anyhow::Result<Vec<u8>> {
        let path = self.state_path(hash, "olean.zst");
        let bytes = self.inner.get(&path).await?.bytes().await?;
        Ok(zstd::decode_all(&bytes[..])?)
    }

    pub async fn put_session(&self, id: &str, meta: &SessionMeta) -> anyhow::Result<()> {
        let path = Path::from(format!("sessions/{}/{id}.json", self.version));
        self.put_json(&path, meta).await
    }

    pub async fn session(&self, id: &str) -> anyhow::Result<SessionMeta> {
        self.get_json(&Path::from(format!("sessions/{}/{id}.json", self.version)))
            .await
    }

    async fn put_json<T: Serialize>(&self, path: &Path, value: &T) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        self.inner.put(path, PutPayload::from(bytes)).await?;
        Ok(())
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &Path) -> anyhow::Result<T> {
        let bytes = self
            .inner
            .get(path)
            .await
            .with_context(|| format!("loading {path}"))?
            .bytes()
            .await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Deletes states and sessions of this version older than `ttl`.
    pub async fn sweep(&self, ttl: Duration) -> anyhow::Result<usize> {
        let cutoff = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .saturating_sub(ttl)
            .as_secs() as i64;
        let mut deleted = 0;
        for prefix in ["states", "sessions"] {
            let prefix = Path::from(format!("{prefix}/{}", self.version));
            let mut objects = self.inner.list(Some(&prefix));
            while let Some(meta) = objects.next().await {
                let meta = meta?;
                if meta.last_modified.timestamp() < cutoff {
                    self.inner.delete(&meta.location).await?;
                    deleted += 1;
                }
            }
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    #[tokio::test]
    async fn states_round_trip_and_sweep() {
        let store = Store::new(Arc::new(InMemory::new()), "v1".into());
        let meta = StateMeta {
            header: "import Mathlib".into(),
            session: "s".into(),
            parent: None,
            tactic: None,
            goals: vec!["⊢ True".into()],
        };
        let pickle = b"olean bytes".repeat(100);
        let hash = store.put_state(&pickle, &meta).await.unwrap();
        assert_eq!(store.put_state(&pickle, &meta).await.unwrap(), hash);
        assert_eq!(store.state_pickle(&hash).await.unwrap(), pickle);
        assert_eq!(store.state_meta(&hash).await.unwrap().goals, meta.goals);

        store
            .put_session(
                "s",
                &SessionMeta {
                    code: "x".into(),
                    declarations: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(store.session("s").await.unwrap().code, "x");

        assert_eq!(store.sweep(Duration::from_secs(3600)).await.unwrap(), 0);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(store.sweep(Duration::ZERO).await.unwrap(), 3);
        assert!(store.state_meta(&hash).await.is_err());
    }
}
