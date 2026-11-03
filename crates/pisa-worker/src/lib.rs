//! Pisa worker agent: dials the router, receives jobs, and runs them on a pool
//! of Lean REPL processes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use object_store::ObjectStore;
use pisa_protocol::header::split_header;
use pisa_protocol::wire::{self, Job, JobResult, RouterMessage, WorkerMessage};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

pub mod catalog;
pub mod cgroup;
pub mod check;
pub mod config;
pub mod kv;
pub mod launcher;
pub mod policy;
pub mod pool;
pub mod sessions;
pub mod snapshots;
pub mod store;

pub use config::WorkerConfig;
use pool::{Pool, PoolConfig};
use sessions::Sessions;

/// Runs the worker until SIGTERM or Ctrl-C, then drains and exits.
pub async fn run(cfg: WorkerConfig) -> anyhow::Result<()> {
    let store = cfg.object_store()?;
    let kv = cfg.kv().await?;
    run_with_stores(cfg, store, kv, shutdown_signal()).await
}

async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing a SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

/// Runs the worker with pickled proof states and snapshot modules kept in
/// `store`, and the records that describe them, the sessions and the snapshot
/// entries in `kv`. Every worker of the same version must share both. When
/// `shutdown` completes, the worker drains: it tells the router to send no
/// more jobs, finishes the ones it has (for at most `drain_timeout_secs`), and
/// returns.
pub async fn run_with_stores(
    cfg: WorkerConfig,
    store: Arc<dyn ObjectStore>,
    kv: kv::Kv,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let version = cfg.version()?;
    let worker_id = cfg.worker_id();
    let launcher = launcher::Launcher::new(&cfg, &worker_id, &version).await?;
    let snapshots = snapshots::Snapshots::new(
        store.clone(),
        kv.clone(),
        version.clone(),
        launcher.snapshot_root().to_path_buf(),
        Duration::from_secs(cfg.snapshot_confirm_secs),
    );
    let (pool, warm_rx, mut restarts_rx) = Pool::new(
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
    let sessions = Arc::new(Sessions {
        store: store::Store::new(store, kv, version.clone()),
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

    let (drain_tx, mut drain_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        tracing::info!("shutting down: draining");
        let _ = drain_tx.send(true);
    });
    let drain_timeout = Duration::from_secs(cfg.drain_timeout_secs);

    let hello = WorkerMessage::Hello {
        worker_id,
        version,
        slots: pool.slots() as u32,
    };
    let mut backoff = Duration::from_millis(100);
    loop {
        if *drain_rx.borrow() {
            return Ok(());
        }
        match TcpStream::connect(&cfg.router).await {
            Ok(stream) => {
                backoff = Duration::from_millis(100);
                tracing::info!(router = %cfg.router, "connected");
                let conn = Connection {
                    pool: &pool,
                    sessions: &sessions,
                    hello: &hello,
                    drain_timeout,
                };
                let result = conn
                    .serve(stream, warm_rx.clone(), &mut restarts_rx, &mut drain_rx)
                    .await;
                if *drain_rx.borrow() {
                    tracing::info!(?result, "drained");
                    return Ok(());
                }
                tracing::warn!(?result, "router connection closed");
            }
            Err(e) => tracing::warn!(router = %cfg.router, "connect failed: {e}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = drain_rx.changed() => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

struct Connection<'a> {
    pool: &'a Arc<Pool>,
    sessions: &'a Arc<Sessions>,
    hello: &'a WorkerMessage,
    drain_timeout: Duration,
}

impl Connection<'_> {
    /// Serves one router connection. Returns when the router hangs up, or,
    /// once draining, when every job received has been answered.
    async fn serve(
        &self,
        stream: TcpStream,
        mut warm_rx: watch::Receiver<BTreeMap<usize, String>>,
        restarts_rx: &mut mpsc::UnboundedReceiver<String>,
        drain_rx: &mut watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        stream.set_nodelay(true)?;
        let (mut sink, mut source) = wire::connection(stream).split();
        let (tx, mut rx) = mpsc::unbounded_channel::<WorkerMessage>();
        wire::send(&mut sink, self.hello).await?;
        let msg = status(&warm_rx.borrow_and_update());
        wire::send(&mut sink, &msg).await?;
        // Jobs received but not yet answered on this connection.
        let mut outstanding = 0usize;
        let mut draining = false;
        let deadline = tokio::time::sleep(Duration::MAX);
        tokio::pin!(deadline);
        loop {
            if draining && outstanding == 0 {
                return Ok(());
            }
            tokio::select! {
                msg = wire::recv::<_, RouterMessage>(&mut source) => match msg? {
                    None => return Ok(()),
                    Some(RouterMessage::Job { request_id, job }) => {
                        outstanding += 1;
                        let pool = self.pool.clone();
                        let sessions = self.sessions.clone();
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let result = run_job(&pool, &sessions, job).await;
                            let _ = tx.send(WorkerMessage::Result { request_id, result });
                        });
                    }
                    Some(RouterMessage::Prewarm { headers }) if !draining => {
                        prewarm(self.pool, &headers)
                    }
                    Some(RouterMessage::Prewarm { .. }) => {}
                },
                Some(out) = rx.recv() => {
                    if matches!(out, WorkerMessage::Result { .. }) {
                        outstanding -= 1;
                        // A header the job warmed is announced first, so the
                        // client's next request can be routed here.
                        if warm_rx.has_changed().unwrap_or(false) {
                            let msg = status(&warm_rx.borrow_and_update());
                            wire::send(&mut sink, &msg).await?;
                        }
                    }
                    wire::send(&mut sink, &out).await?;
                }
                Ok(()) = warm_rx.changed() => {
                    let msg = status(&warm_rx.borrow_and_update());
                    wire::send(&mut sink, &msg).await?;
                }
                Some(cause) = restarts_rx.recv() => {
                    wire::send(&mut sink, &WorkerMessage::Restart { cause }).await?;
                }
                Ok(()) = drain_rx.changed(), if !draining => {
                    if *drain_rx.borrow() {
                        draining = true;
                        tracing::info!(outstanding, "draining");
                        wire::send(&mut sink, &WorkerMessage::Draining).await?;
                        deadline.as_mut().reset(tokio::time::Instant::now() + self.drain_timeout);
                    }
                }
                () = &mut deadline, if draining => {
                    anyhow::bail!("drain timed out with {outstanding} jobs unanswered");
                }
            }
        }
    }
}

async fn run_job(pool: &Arc<Pool>, sessions: &Sessions, job: Job) -> JobResult {
    match job {
        Job::Check(c) => JobResult::Check(check::run_check(pool, c).await),
        Job::SessionStart(j) => JobResult::SessionStarted(sessions.start(pool, j).await),
        Job::SessionTactic(j) => JobResult::TacticApplied(sessions.tactic(pool, j).await),
        Job::SessionCheck(j) => JobResult::SessionChecked(sessions.check(pool, j).await),
        Job::SnapshotCreate(j) => {
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
        Job::Unsupported => JobResult::Failed {
            error: "this worker does not support the job; it may be older than the router".into(),
        },
    }
}

fn status(warm: &BTreeMap<usize, String>) -> WorkerMessage {
    let mut warm_headers: Vec<String> = warm.values().cloned().collect();
    warm_headers.sort();
    warm_headers.dedup();
    WorkerMessage::Status { warm_headers }
}

/// Imports each header into an idle, empty slot, if there is one.
fn prewarm(pool: &Arc<Pool>, headers: &[String]) {
    for text in headers {
        let split = split_header(text);
        if pool.is_warm(&split.header_key) {
            continue;
        }
        let Some(mut lease) = pool.try_acquire_cold() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(e) = lease.warm_up(&split.header, &split.header_key).await {
                tracing::warn!(header = %split.header, "prewarm failed: {e:?}");
            }
        });
    }
}
