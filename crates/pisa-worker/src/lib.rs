//! Pisa worker agent: takes jobs from the database's job queue and runs them
//! on a pool of Lean REPL processes.

use std::sync::Arc;
use std::time::Duration;

use object_store::ObjectStore;
use pisa_protocol::header::split_header;
use pisa_protocol::wire::{Job, JobResult};
use tokio::sync::watch;

pub mod cgroup;
pub mod check;
pub mod claims;
pub mod config;
pub mod launcher;
pub mod policy;
pub mod pool;
pub mod sessions;
pub mod snapshots;
pub mod store;

pub use config::WorkerConfig;
pub use pisa_db::{catalog, db, queue, records};
use pool::{Pool, PoolConfig};
use sessions::Sessions;

/// How often a worker deletes the snapshot change log's expired rows.
const LOG_PRUNE_EVERY: Duration = Duration::from_secs(600);

/// Runs the worker until SIGTERM or Ctrl-C, then drains and exits.
pub async fn run(cfg: WorkerConfig) -> anyhow::Result<()> {
    let store = cfg.object_store()?;
    let db = cfg.db().await?;
    run_with_stores(cfg, store, db, shutdown_signal()).await
}

async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing a SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

/// Runs the worker on the job queue in `db`, with pickled proof states and
/// snapshot modules kept in `store`, and the records that describe them, the
/// sessions and the snapshot entries in `db`. Every worker of the same
/// version must share both. When `shutdown` completes, the worker drains: it
/// claims no more jobs, finishes the ones it has (for at most
/// `drain_timeout_secs`), and returns.
pub async fn run_with_stores(
    cfg: WorkerConfig,
    store: Arc<dyn ObjectStore>,
    db: db::Db,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let version = cfg.version()?;
    let worker_id = cfg.worker_id();
    let launcher = launcher::Launcher::new(&cfg, &worker_id, &version).await?;
    let snapshots = snapshots::Snapshots::new(
        store.clone(),
        db.clone(),
        version.clone(),
        launcher.snapshot_root().to_path_buf(),
        Duration::from_secs(cfg.snapshot_confirm_secs),
        cfg.policy(),
    );
    let (pool, warm_rx) = Pool::new(
        launcher,
        Arc::new(snapshots),
        PoolConfig {
            slots: cfg.slots(),
            max_uses: cfg.max_uses,
            max_rss_bytes: cfg.max_rss_mb * 1024 * 1024,
            import_timeout: Duration::from_secs(cfg.import_timeout_secs),
            policy: cfg.policy(),
        },
    );
    tracing::info!(%worker_id, %version, slots = pool.slots(), "worker starting");
    prewarm(&pool, &cfg.prewarm);
    let catalog = catalog::Catalog::new(db.clone(), version.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(LOG_PRUNE_EVERY);
        loop {
            tick.tick().await;
            match catalog.prune_log(catalog::LOG_RETENTION).await {
                Ok(n) => tracing::debug!(deleted = n, "pruned the snapshot change log"),
                Err(e) => tracing::warn!("pruning the snapshot change log failed: {e}"),
            }
        }
    });
    let sessions = Arc::new(Sessions {
        store: store::Store::new(store, db.clone(), version.clone(), worker_id.clone()),
        version: version.clone(),
    });
    if cfg.state_ttl_secs > 0 {
        let ttl = Duration::from_secs(cfg.state_ttl_secs);
        let store = sessions.store.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval((ttl / 4).max(Duration::from_secs(60)));
            loop {
                tick.tick().await;
                match store.sweep(ttl).await {
                    Ok(n) => tracing::info!(deleted = n, "swept expired proof states"),
                    Err(e) => tracing::warn!("sweeping proof states failed: {e}"),
                }
            }
        });
    }

    let (drain_tx, drain_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        tracing::info!("shutting down: draining");
        let _ = drain_tx.send(true);
    });
    let claims = claims::Claims {
        queue: queue::Queue::new(db, cfg.queue.clone()),
        me: worker_id,
        version,
        lease: Duration::from_secs(cfg.lease_secs.max(1)),
        pool,
        sessions,
        warm: warm_rx,
    };
    claims
        .run(drain_rx, Duration::from_secs(cfg.drain_timeout_secs))
        .await
}

pub(crate) async fn run_job(pool: &Arc<Pool>, sessions: &Sessions, job: Job) -> JobResult {
    match job {
        Job::Check(c) => JobResult::Check(check::run_check(pool, c).await),
        Job::SessionStart(j) => JobResult::SessionStarted(sessions.start(pool, j).await),
        Job::SessionTactic(j) => JobResult::TacticApplied(sessions.tactic(pool, j).await),
        Job::SessionCheck(j) => JobResult::SessionChecked(sessions.check(pool, j).await),
        Job::SnapshotCreate(j) | Job::SnapshotReplace(j) => {
            JobResult::SnapshotCreated(Box::new(pool.snapshots().create(pool, j).await))
        }
        Job::SnapshotGet(j) => {
            JobResult::Snapshot(pool.snapshots().get(&j.entry).await.map(Box::new))
        }
        Job::SnapshotDelete(j) => {
            JobResult::SnapshotDeleted(pool.snapshots().delete(&j.entry).await)
        }
        Job::SnapshotVerify(j) => {
            JobResult::SnapshotVerified(pool.snapshots().verify(pool, &j.entry).await)
        }
        Job::SnapshotList(j) => JobResult::SnapshotList(
            pool.snapshots()
                .list(&j.labels, j.limit, j.after.as_deref())
                .await,
        ),
        Job::SnapshotLabels(j) => {
            JobResult::SnapshotLabels(pool.snapshots().set_labels(&j.entry, &j.labels).await)
        }
        Job::SnapshotDependents(j) => {
            JobResult::SnapshotDependents(pool.snapshots().dependents(&j.entry, &j.labels).await)
        }
        Job::SnapshotRebuild(j) => {
            JobResult::SnapshotRebuild(pool.snapshots().rebuild_plan(&j).await)
        }
        Job::SnapshotAudit(j) => JobResult::SnapshotAudit(pool.snapshots().audit(&j.labels).await),
        Job::Unsupported => JobResult::Failed {
            error: "this worker does not support the job; it may be older than the server".into(),
        },
    }
}

/// Imports each header into an idle, empty slot, if there is one, unless it
/// is warm or being warmed already.
pub(crate) fn prewarm(pool: &Arc<Pool>, headers: &[String]) {
    for text in headers {
        let split = split_header(text);
        if pool.is_warm(&split.header_key) {
            continue;
        }
        let Some(warming) = pool.prewarming(&split.header_key) else {
            continue;
        };
        let Some(mut lease) = pool.try_acquire_cold() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(e) = lease.warm_up(&split.header, &split.header_key).await {
                tracing::warn!(header = %split.header, "prewarm failed: {e:?}");
            }
            drop(warming);
        });
    }
}
