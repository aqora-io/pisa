//! Pisa router: accepts client requests over HTTP, queues them globally, and
//! routes each one to a worker that dialled in over TCP.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use pisa_protocol::wire::{self, RouterMessage, WorkerMessage};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

pub mod dispatch;
pub mod http;
pub mod metrics;
pub mod sessions;
pub mod snapshots;

use dispatch::{DispatchConfig, Dispatcher};
use http::{App, Versions};

#[derive(Debug, Clone, clap::Parser)]
#[command(about = "Pisa router: queues Lean jobs and routes them to workers")]
pub struct RouterConfig {
    /// Client HTTP API.
    #[arg(long, env = "PISA_HTTP", default_value = "0.0.0.0:8000")]
    pub http: SocketAddr,
    /// Where workers connect.
    #[arg(long, env = "PISA_WORKERS", default_value = "0.0.0.0:7070")]
    pub workers: SocketAddr,
    /// Seconds a job may wait for a free worker before the request gets 429.
    #[arg(long, env = "PISA_MAX_WAIT_SECS", default_value_t = 60)]
    pub max_wait_secs: u64,
    #[arg(long, env = "PISA_MAX_RETRIES", default_value_t = 2)]
    pub max_retries: u32,
    /// Version used when a request names none.
    #[arg(long, env = "PISA_DEFAULT_VERSION")]
    pub default_version: Option<String>,
    /// Version aliases, e.g. `physlib=v4.35.0-1a2b3c4d`. Separate several with ','.
    #[arg(long, env = "PISA_ALIASES", value_delimiter = ',', value_parser = parse_alias)]
    pub alias: Vec<(String, String)>,
}

fn parse_alias(s: &str) -> Result<(String, String), String> {
    let (k, v) = s.split_once('=').ok_or("expected NAME=VERSION")?;
    Ok((k.trim().to_string(), v.trim().to_string()))
}

pub struct Running {
    pub http_addr: SocketAddr,
    pub worker_addr: SocketAddr,
    pub app: Arc<App>,
}

/// Binds both listeners and serves in background tasks.
pub async fn start(cfg: RouterConfig) -> anyhow::Result<Running> {
    metrics::handle();
    let dispatcher = Arc::new(Dispatcher::new(DispatchConfig {
        max_wait: Duration::from_secs(cfg.max_wait_secs),
        max_retries: cfg.max_retries,
        ..Default::default()
    }));
    let app = Arc::new(App {
        dispatcher: dispatcher.clone(),
        versions: Versions {
            default: cfg.default_version.clone(),
            aliases: cfg.alias.iter().cloned().collect(),
        },
        affinity: Default::default(),
    });

    let worker_listener = TcpListener::bind(cfg.workers).await?;
    let worker_addr = worker_listener.local_addr()?;
    tokio::spawn(accept_workers(worker_listener, dispatcher.clone()));

    let d = dispatcher.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tick.tick().await;
            d.expire();
        }
    });

    let http_listener = TcpListener::bind(cfg.http).await?;
    let http_addr = http_listener.local_addr()?;
    let routes = http::router(app.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(http_listener, routes).await {
            tracing::error!("http server failed: {e}");
        }
    });
    tracing::info!(%http_addr, %worker_addr, "router listening");
    Ok(Running {
        http_addr,
        worker_addr,
        app,
    })
}

async fn accept_workers(listener: TcpListener, dispatcher: Arc<Dispatcher>) {
    static NEXT_CONN: AtomicU64 = AtomicU64::new(0);
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
                let d = dispatcher.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_worker(conn, stream, &d).await {
                        tracing::warn!(%peer, "worker connection error: {e}");
                    }
                    d.disconnect(conn);
                });
            }
            Err(e) => tracing::warn!("accept failed: {e}"),
        }
    }
}

async fn serve_worker(conn: u64, stream: TcpStream, d: &Dispatcher) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let (mut sink, mut source) = wire::connection(stream).split();
    let Some(WorkerMessage::Hello {
        worker_id,
        version,
        slots,
    }) = wire::recv(&mut source).await?
    else {
        anyhow::bail!("expected hello");
    };
    tracing::info!(%worker_id, %version, slots, "worker connected");
    let (tx, mut rx) = mpsc::unbounded_channel::<RouterMessage>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if wire::send(&mut sink, &msg).await.is_err() {
                break;
            }
        }
    });
    d.connect(conn, worker_id, version, slots, tx);
    let result = async {
        while let Some(msg) = wire::recv::<_, WorkerMessage>(&mut source).await? {
            match msg {
                WorkerMessage::Status { warm_headers } => d.status(conn, warm_headers),
                WorkerMessage::Result { request_id, result } => {
                    d.complete(conn, request_id, result)
                }
                WorkerMessage::Restart { cause } => {
                    ::metrics::counter!(metrics::RESTARTS, "cause" => cause).increment(1)
                }
                WorkerMessage::Draining => d.drain(conn),
                WorkerMessage::Hello { .. } => tracing::warn!("duplicate hello"),
            }
        }
        anyhow::Ok(())
    }
    .await;
    writer.abort();
    result
}
