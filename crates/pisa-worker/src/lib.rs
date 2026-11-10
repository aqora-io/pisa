//! Pisa worker agent: takes jobs from the database's job queue and runs them
//! on a pool of Lean REPL processes.

use std::sync::Arc;
use std::time::Duration;

use object_store::ObjectStore;
use pisa_protocol::api::Detail;
use pisa_protocol::header::split_header;
use pisa_protocol::verify::Declaration;
use pisa_protocol::wire::{Job, JobResult};
use tokio::sync::watch;

pub mod cgroup;
pub mod check;
pub mod claims;
pub mod config;
pub mod documents;
pub mod launcher;
pub mod policy;
pub mod pool;
pub mod runs;
pub mod sessions;
pub mod snapshots;

pub use config::WorkerConfig;
use documents::Documents;
pub use pisa_db::{catalog, db, queue, records};
use pool::{Pool, PoolConfig};
use sessions::Sessions;

/// How often a worker deletes the snapshot change log's expired rows.
const LOG_PRUNE_EVERY: Duration = Duration::from_secs(600);
/// How often one worker of a version sweeps what deletes left behind, under
/// a lease of this name; and how long a module file without a record is
/// kept, as a build uploads its files before its record.
const SWEEP_EVERY: Duration = Duration::from_secs(3600);
const SWEEP_LEASE: &str = "snapshot-sweep";
const SWEEP_GRACE: Duration = Duration::from_secs(24 * 3600);

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

/// Runs the worker on the job queue in `db`, with snapshot modules kept in
/// `store`, and the records that describe them, the sessions, their states,
/// the open documents and the snapshot entries in `db`. Every worker of the
/// same version must share both. When `shutdown` completes, the worker drains: it
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
    let documents = if cfg.beam_slots > 0 {
        let documents = Documents::new(
            pool.clone(),
            db.clone(),
            version.clone(),
            worker_id.clone(),
            &cfg,
        )
        .await?;
        tracing::info!(beam_slots = documents.beam_slots(), "serving documents");
        Some(Arc::new(documents))
    } else {
        None
    };
    prewarm(&pool, &cfg.prewarm);
    {
        let (pool, owner) = (pool.clone(), worker_id.clone());
        let catalog = catalog::Catalog::new(db.clone(), version.clone());
        tokio::spawn(async move {
            // From the start, so that workers that live less than an
            // interval sweep too; the lease keeps it to one an interval.
            let mut tick = tokio::time::interval(SWEEP_EVERY);
            loop {
                tick.tick().await;
                match catalog.take_lease(SWEEP_LEASE, &owner, SWEEP_EVERY).await {
                    Ok(Some(_)) => match pool.snapshots().sweep(SWEEP_GRACE).await {
                        Ok((modules, files)) => {
                            tracing::info!(modules, files, "swept what deletes left")
                        }
                        Err(e) => tracing::warn!("sweeping snapshots failed: {e}"),
                    },
                    Ok(None) => {}
                    Err(e) => tracing::warn!("taking the sweep's lease failed: {e}"),
                }
            }
        });
    }
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
        records: records::Records::new(db.clone(), version.clone()),
        version: version.clone(),
    });
    if cfg.state_ttl_secs > 0 {
        let ttl = Duration::from_secs(cfg.state_ttl_secs);
        let records = records::Records::new(db.clone(), version.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval((ttl / 4).max(Duration::from_secs(60)));
            loop {
                tick.tick().await;
                let cutoff = documents::now_secs().saturating_sub(ttl.as_secs());
                match records.sweep(cutoff).await {
                    Ok(n) => tracing::info!(deleted = n, "swept expired sessions and documents"),
                    Err(e) => tracing::warn!("sweeping sessions and documents failed: {e}"),
                }
            }
        });
    }
    tokio::spawn(remove_pickles(store, version.clone()));

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
        documents,
        warm: warm_rx,
    };
    claims
        .run(drain_rx, Duration::from_secs(cfg.drain_timeout_secs))
        .await
}

/// `declarations` as a response gives them: summarised unless `detail` asks
/// for them in full. Records keep them in full; this is applied last.
fn answered(declarations: &mut Vec<Declaration>, detail: &[Detail], pool: &Pool) {
    if detail.contains(&Detail::Declarations) {
        return;
    }
    let allowed = &pool.policy().allowed_axioms;
    let full = std::mem::take(declarations);
    *declarations = full.into_iter().map(|d| d.summarised(allowed)).collect();
}

pub(crate) async fn run_job(
    pool: &Arc<Pool>,
    sessions: &Sessions,
    documents: Option<&Documents>,
    job: Job,
) -> JobResult {
    match job {
        Job::Check(c) => {
            let detail = c.detail.clone();
            let mut outcome = check::run_check(pool, c).await;
            if let Some(v) = &mut outcome.verification {
                answered(&mut v.declarations, &detail, pool);
            }
            JobResult::Check(outcome)
        }
        job @ (Job::SessionStart(_) | Job::SessionTactic(_)) => {
            documents::run(documents, job).await
        }
        Job::SessionCheck(j) => {
            let detail = j.detail.clone();
            let mut checked = sessions.check(pool, j).await;
            if let Some(v) = &mut checked.outcome.verification {
                answered(&mut v.declarations, &detail, pool);
            }
            JobResult::SessionChecked(checked)
        }
        Job::SnapshotCreate(j) | Job::SnapshotReplace(j) => {
            let detail = j.detail.clone();
            let mut created = pool.snapshots().create(pool, j).await;
            answered(&mut created.check.declarations, &detail, pool);
            JobResult::SnapshotCreated(Box::new(created))
        }
        Job::SnapshotDelete(j) => {
            JobResult::SnapshotDeleted(pool.snapshots().delete(&j.entry, j.with_replacers).await)
        }
        Job::SnapshotVerify(j) => {
            let per_module = Duration::from_secs(j.timeout_secs);
            JobResult::SnapshotVerified(pool.snapshots().verify(pool, &j.entry, per_module).await)
        }
        Job::SnapshotRebuild(_) => JobResult::Failed {
            error: "a rebuild is planned and run by a server".into(),
        },
        Job::SnapshotComparator(j) => {
            JobResult::SnapshotComparator(runs::comparator(pool, j).await)
        }
        Job::SnapshotExport(j) => {
            let project = pool.launcher().project();
            JobResult::SnapshotExport(
                runs::export(pool.snapshots(), project, &j.against, &j.allow).await,
            )
        }
        job @ (Job::DocumentOpen(_)
        | Job::DocumentEdit(_)
        | Job::DocumentGoals(_)
        | Job::DocumentProbe(_)
        | Job::DocumentClose(_)) => documents::run(documents, job).await,
        Job::Unsupported => JobResult::Failed {
            error: "this worker does not support the job; it may be older than the server".into(),
        },
    }
}

/// Deletes the pickled proof states an earlier release stored under
/// `states/<version>/`: states are replayed on Lean Beam now.
async fn remove_pickles(store: Arc<dyn ObjectStore>, version: String) {
    use futures::StreamExt;
    use object_store::ObjectStoreExt;
    let prefix = object_store::path::Path::from(format!("states/{version}"));
    let mut objects = store.list(Some(&prefix));
    let mut removed = 0;
    while let Some(meta) = objects.next().await {
        let removing = match meta {
            Ok(meta) => store.delete(&meta.location).await,
            Err(e) => Err(e),
        };
        match removing {
            Ok(()) => removed += 1,
            Err(e) => {
                tracing::warn!("removing the pickled proof states failed: {e}");
                return;
            }
        }
    }
    if removed > 0 {
        tracing::info!(
            removed,
            "removed the pickled proof states of an earlier release"
        );
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
