//! Informal descriptions, keyed by declaration content hash, in object
//! storage (a local directory or S3).
//!
//! Each producer run (LeanSearch v2 import, an informalization run) writes one
//! batch object `informal/<batch>.ndjson.zst`. Loading reads every batch;
//! later batches win. A description stays valid while its declaration's
//! content hash is unchanged.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::Arc;

use futures::StreamExt;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pisa_protocol::search::Informal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub content_hash: String,
    pub name: String,
    pub informal: Informal,
    /// LLM-generated only: synthetic queries for extra embedding views.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synthetic_queries: Vec<String>,
    /// Model, tokens and cost of generating it, when generated here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

/// A description's hash, from its title, text and LaTeX: readers' flags are
/// on the description they read, so one written again starts with none.
pub fn description_hash(title: &str, text: &str, latex: Option<&str>) -> String {
    let mut h = Sha256::new();
    for part in [title, text, latex.unwrap_or_default()] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    hex::encode(&h.finalize()[..16])
}

pub struct Store {
    inner: Arc<dyn ObjectStore>,
    prefix: Path,
}

impl Store {
    /// `url` is `file:///abs/dir`, a plain local path, or `s3://bucket/prefix`
    /// (credentials from the AWS_* environment).
    pub fn open(url: &str) -> anyhow::Result<Store> {
        let url = if url.contains("://") {
            url.to_string()
        } else {
            let abs = std::path::absolute(url)?;
            std::fs::create_dir_all(&abs)?;
            format!("file://{}", abs.display())
        };
        let parsed = url::Url::parse(&url)?;
        let (inner, prefix) = object_store::parse_url_opts(&parsed, std::env::vars())?;
        Ok(Store {
            inner: Arc::from(inner),
            prefix,
        })
    }

    fn batch_path(&self, batch: &str) -> Path {
        self.prefix
            .clone()
            .join("informal")
            .join(format!("{batch}.ndjson.zst"))
    }

    /// Writes one object under `<kind>/` (e.g. `vectors/<model>/<batch>.bin.zst`).
    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        let mut path = self.prefix.clone();
        for part in key.split('/') {
            path = path.join(part);
        }
        self.inner.put(&path, PutPayload::from(bytes)).await?;
        Ok(())
    }

    /// Every object under `dir`, in name order.
    pub async fn get_all(&self, dir: &str) -> anyhow::Result<Vec<bytes::Bytes>> {
        let mut prefix = self.prefix.clone();
        for part in dir.split('/') {
            prefix = prefix.join(part);
        }
        let mut names: Vec<Path> = Vec::new();
        let mut list = self.inner.list(Some(&prefix));
        while let Some(meta) = list.next().await {
            names.push(meta?.location);
        }
        names.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
        let mut out = Vec::new();
        for path in names {
            out.push(self.inner.get(&path).await?.bytes().await?);
        }
        Ok(out)
    }

    pub async fn write_batch(&self, batch: &str, entries: &[Entry]) -> anyhow::Result<()> {
        let mut enc = zstd::Encoder::new(Vec::new(), 9)?;
        for e in entries {
            serde_json::to_writer(&mut enc, e)?;
            enc.write_all(b"\n")?;
        }
        let bytes = enc.finish()?;
        self.inner
            .put(&self.batch_path(batch), PutPayload::from(bytes))
            .await?;
        Ok(())
    }

    /// Every description, by content hash. Batches load in name order, so
    /// name them with a sortable timestamp to let newer ones win.
    pub async fn load_all(&self) -> anyhow::Result<HashMap<String, Entry>> {
        let dir = self.prefix.clone().join("informal");
        let mut names: Vec<Path> = Vec::new();
        let mut list = self.inner.list(Some(&dir));
        while let Some(meta) = list.next().await {
            let meta = meta?;
            if meta.location.as_ref().ends_with(".ndjson.zst") {
                names.push(meta.location);
            }
        }
        names.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
        let mut out = HashMap::new();
        for path in names {
            let bytes = self.inner.get(&path).await?.bytes().await?;
            let reader = std::io::BufReader::new(zstd::Decoder::new(&bytes[..])?);
            for line in reader.lines() {
                let entry: Entry = serde_json::from_str(&line?)?;
                out.insert(entry.content_hash.clone(), entry);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hash: &str, name: &str, text: &str) -> Entry {
        Entry {
            content_hash: hash.into(),
            name: name.into(),
            informal: Informal {
                informal_name: name.into(),
                statement_nl: text.into(),
                source: "test".into(),
                ..Default::default()
            },
            synthetic_queries: Vec::new(),
            meta: None,
        }
    }

    #[test]
    fn a_description_hash_follows_its_text() {
        let hash = |title, text, latex| description_hash(title, text, latex);
        let h = hash("Name", "Words.", Some("x = x"));
        assert_eq!(h, hash("Name", "Words.", Some("x = x")));
        assert_eq!(h.len(), 32);
        for other in [
            hash("Name", "Other words.", Some("x = x")),
            hash("Other", "Words.", Some("x = x")),
            hash("Name", "Words.", None),
            // Fields don't run into each other.
            hash("NameW", "ords.", Some("x = x")),
        ] {
            assert_ne!(h, other);
        }
    }

    #[tokio::test]
    async fn later_batches_win() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().to_str().unwrap()).unwrap();
        store
            .write_batch(
                "2026-01-01-a",
                &[entry("h1", "a", "old"), entry("h2", "b", "b")],
            )
            .await
            .unwrap();
        store
            .write_batch("2026-02-01-a", &[entry("h1", "a", "new")])
            .await
            .unwrap();
        let all = store.load_all().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all["h1"].informal.statement_nl, "new");
    }
}
