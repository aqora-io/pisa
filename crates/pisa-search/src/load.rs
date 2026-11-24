//! Loading a bundle into Qdrant: the declarations collection of its `env_id`
//! gets a point per declaration, and the manifest last, which is when
//! servers start answering from it. Snapshot points are left alone.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use futures::{StreamExt, TryStreamExt};
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::index::Bundle;
use crate::points::{self, PointPayload, collection, entries_collection};
use crate::qdrant::{self, MANIFEST_ID, Qdrant};

/// Points per upsert, and upserts in flight.
const BATCH: usize = 512;
const IN_FLIGHT: usize = 4;

#[derive(Debug)]
pub struct Loaded {
    pub env_id: String,
    pub points: usize,
    pub removed: usize,
}

/// Creates `collection` with `config` and `indexes` unless it exists. One
/// that exists without a sparse vector `config` names is refused: a sparse
/// vector can't be added to it.
pub async fn ensure_collection(
    q: &Qdrant,
    collection: &str,
    config: &Value,
    indexes: &[(&str, &str)],
) -> anyhow::Result<()> {
    if !q.exists(collection).await? {
        q.create(collection, config).await?;
    } else if let Some(wanted) = config["sparse_vectors"].as_object() {
        let has = q.sparse_vectors(collection).await?;
        let lacking: Vec<&String> = wanted.keys().filter(|k| !has.contains(k)).collect();
        anyhow::ensure!(
            lacking.is_empty(),
            "collection {collection} has no {lacking:?} sparse vector: drop it and load again"
        );
    }
    for (field, schema) in indexes {
        q.index(collection, field, schema).await?;
    }
    Ok(())
}

/// Loads the bundle in `dir`, which must have vectors for at least
/// `min_coverage` of its declarations, into `q`.
pub async fn load(
    dir: &Path,
    q: &Qdrant,
    replication: u32,
    min_coverage: f64,
) -> anyhow::Result<Loaded> {
    let bundle = Arc::new(Bundle::open(dir)?);
    let n = bundle.decls.len();
    let covered = bundle.dense.as_ref().map_or(0, |d| d.len());
    anyhow::ensure!(
        n == 0 || covered as f64 / n as f64 >= min_coverage,
        "only {covered} of {n} declarations have vectors; at least {:.0}% must",
        min_coverage * 100.0
    );
    anyhow::ensure!(
        !bundle.manifest.field_stats.is_empty(),
        "the bundle has no field statistics; build it again"
    );
    // Qdrant may still be starting, as in Compose.
    for _ in 0..60 {
        if q.healthy().await {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let env_id = bundle.manifest.env_id.clone();
    let c = collection(&env_id);
    ensure_collection(
        q,
        &c,
        &points::collection_config(replication),
        points::INDEXES,
    )
    .await?;
    let entries = entries_collection(&env_id);
    let config = points::entries_config(replication);
    ensure_collection(q, &entries, &config, points::ENTRY_INDEXES).await?;

    let used_by = Arc::new(points::used_by(&bundle.decls));
    let started = std::time::Instant::now();
    let ranges: Vec<std::ops::Range<usize>> = (0..n)
        .step_by(BATCH)
        .map(|i| i..(i + BATCH).min(n))
        .collect();
    let done = std::sync::atomic::AtomicUsize::new(0);
    futures::stream::iter(ranges)
        .map(|range| {
            let (bundle, used_by) = (Arc::clone(&bundle), Arc::clone(&used_by));
            let (q, c, done) = (q, &c, &done);
            async move {
                let len = range.len();
                let batch =
                    tokio::task::spawn_blocking(move || base_points(&bundle, &used_by, range))
                        .await?;
                q.upsert(c, batch).await?;
                let done = done.fetch_add(len, std::sync::atomic::Ordering::Relaxed) + len;
                if done % 50_000 < len {
                    let elapsed = started.elapsed().as_secs_f64();
                    tracing::info!(
                        done,
                        total = n,
                        elapsed = format!("{elapsed:.0}s"),
                        "loaded"
                    );
                }
                Ok::<_, anyhow::Error>(())
            }
        })
        .buffer_unordered(IN_FLIGHT)
        .try_collect::<Vec<()>>()
        .await?;

    let wanted: HashSet<String> = bundle
        .decls
        .iter()
        .map(|d| points::base_id(&d.name))
        .collect();
    let mut stale = Vec::new();
    let mut offset = None;
    loop {
        let filter = json!({"must": [qdrant::is("source", "base")]});
        let (page, next) = q.scroll(&c, filter, json!(false), 10_000, offset).await?;
        stale.extend(
            page.iter()
                .map(|p| p.id())
                .filter(|id| !wanted.contains(id)),
        );
        match next {
            Some(next) => offset = Some(next),
            None => break,
        }
    }
    for ids in stale.chunks(1000) {
        q.delete_ids(&c, ids).await?;
    }
    let manifest = json!({
        "id": MANIFEST_ID,
        "vector": {},
        "payload": {"source": "manifest", "manifest": bundle.manifest},
    });
    q.upsert(&c, vec![manifest]).await?;
    Ok(Loaded {
        env_id,
        points: n,
        removed: stale.len(),
    })
}

/// The points of the bundle's declarations in `range`.
fn base_points(bundle: &Bundle, used_by: &[Vec<u32>], range: std::ops::Range<usize>) -> Vec<Value> {
    let decls = &bundle.decls;
    range
        .into_par_iter()
        .map(|i| {
            let s = &decls[i];
            let mut payload = PointPayload::new("base", s);
            let names = |ids: &[u32]| -> Vec<String> {
                ids.iter()
                    .map(|&j| &decls[j as usize])
                    .filter(|other| !points::is_plumbing(s, other))
                    .map(|other| other.name.clone())
                    .collect()
            };
            payload.deps = names(&s.deps);
            payload.used_by = names(&used_by[i]);
            let dense: Option<Vec<f32>> = bundle
                .dense
                .as_ref()
                .and_then(|d| d.get(i as u32))
                .map(|v| v.iter().map(|&x| f32::from(x) / 127.0).collect());
            let stats = &bundle.manifest.field_stats;
            points::point(
                points::base_id(&s.name),
                &payload,
                s,
                dense.as_deref(),
                stats,
            )
        })
        .collect()
}
