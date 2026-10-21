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

pub mod cgroup;
pub mod check;
pub mod config;
pub mod launcher;
pub mod policy;
pub mod pool;
pub mod sessions;
pub mod store;

pub use config::WorkerConfig;
use pool::{Pool, PoolConfig};
use sessions::Sessions;

pub async fn run(cfg: WorkerConfig) -> anyhow::Result<()> {
    let store = cfg.object_store()?;
    run_with_store(cfg, store).await
}

/// Runs the worker with proof states kept in `store`, which every worker of
/// the same version must share.
pub async fn run_with_store(cfg: WorkerConfig, store: Arc<dyn ObjectStore>) -> anyhow::Result<()> {
    let version = cfg.version()?;
    let worker_id = cfg.worker_id();
    let launcher = launcher::Launcher::new(&cfg, &worker_id).await?;
    let (pool, warm_rx, mut restarts_rx) = Pool::new(
        launcher,
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
        store: store::Store::new(store, version.clone()),
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

    let hello = WorkerMessage::Hello {
        worker_id,
        version,
        slots: pool.slots() as u32,
    };
    let mut backoff = Duration::from_millis(100);
    loop {
        match TcpStream::connect(&cfg.router).await {
            Ok(stream) => {
                backoff = Duration::from_millis(100);
                tracing::info!(router = %cfg.router, "connected");
                let result = serve_connection(
                    stream,
                    &pool,
                    &sessions,
                    &hello,
                    warm_rx.clone(),
                    &mut restarts_rx,
                )
                .await;
                tracing::warn!(?result, "router connection closed");
            }
            Err(e) => tracing::warn!(router = %cfg.router, "connect failed: {e}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

async fn serve_connection(
    stream: TcpStream,
    pool: &Arc<Pool>,
    sessions: &Arc<Sessions>,
    hello: &WorkerMessage,
    mut warm_rx: watch::Receiver<BTreeMap<usize, String>>,
    restarts_rx: &mut mpsc::UnboundedReceiver<String>,
) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let (mut sink, mut source) = wire::connection(stream).split();
    let (tx, mut rx) = mpsc::unbounded_channel::<WorkerMessage>();
    wire::send(&mut sink, hello).await?;
    let msg = status(&warm_rx.borrow_and_update());
    wire::send(&mut sink, &msg).await?;
    loop {
        tokio::select! {
            msg = wire::recv::<_, RouterMessage>(&mut source) => match msg? {
                None => return Ok(()),
                Some(RouterMessage::Job { request_id, job }) => {
                    let pool = pool.clone();
                    let sessions = sessions.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = run_job(&pool, &sessions, job).await;
                        let _ = tx.send(WorkerMessage::Result { request_id, result });
                    });
                }
                Some(RouterMessage::Prewarm { headers }) => prewarm(pool, &headers),
            },
            Some(out) = rx.recv() => wire::send(&mut sink, &out).await?,
            Ok(()) = warm_rx.changed() => {
                let msg = status(&warm_rx.borrow_and_update());
                wire::send(&mut sink, &msg).await?;
            }
            Some(cause) = restarts_rx.recv() => {
                wire::send(&mut sink, &WorkerMessage::Restart { cause }).await?;
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
