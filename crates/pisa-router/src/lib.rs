//! Pisa router: the HTTP API for checks, sessions and snapshots. It hands
//! each request's job to the workers through the database's job queue and
//! waits for the result, so any number of servers can serve one queue.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::{HeaderValue, Method, header};
use pisa_db::db::Db;
use pisa_db::queue::Queue;
use pisa_protocol::version::Versions;
use pisa_snapshots::Readers;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub mod dispatch;
pub mod documents;
pub mod http;
pub mod jobs;
mod rebuild;
mod resubmit;
pub mod sessions;
pub mod snapshots;

use dispatch::{DispatchConfig, Dispatcher};
use http::App;

/// How often a server deletes the queue's leftovers: results nobody took,
/// jobs nobody can claim, workers gone quiet.
const SWEEP_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, clap::Args)]
pub struct RouterConfig {
    /// Client HTTP API.
    #[arg(long, env = "PISA_HTTP", default_value = "0.0.0.0:8000")]
    pub http: SocketAddr,
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
    /// The job queue that the servers and their workers share. Deployments
    /// sharing a database need queues of their own.
    #[arg(long, env = "PISA_QUEUE", default_value = "pisa")]
    pub queue: String,
    /// Axioms a checked declaration may depend on, as the workers allow
    /// them: an audit leaves them out of what it lists of a declaration
    /// that isn't proved.
    #[arg(long, env = "PISA_ALLOWED_AXIOMS", value_delimiter = ',',
          default_value = pisa_protocol::verify::DEFAULT_AXIOMS)]
    pub allowed_axioms: Vec<String>,
    /// Origins whose pages may call the API from a browser, e.g.
    /// `https://app.example.com`, or `*` for any. Separate several with ','.
    /// None by default.
    #[arg(long = "cors-origin", env = "PISA_CORS_ORIGINS", value_delimiter = ',')]
    pub cors_origins: Vec<String>,
}

impl RouterConfig {
    /// How requests' versions resolve: through the default version and the
    /// aliases.
    pub fn versions(&self) -> Versions {
        Versions {
            default: self.default_version.clone(),
            aliases: self.alias.iter().cloned().collect(),
        }
    }
}

/// The CORS layer for `origins`, if there are any.
fn cors(origins: &[String]) -> anyhow::Result<Option<CorsLayer>> {
    let origins: Vec<&str> = origins
        .iter()
        .map(|o| o.trim())
        .filter(|o| !o.is_empty())
        .collect();
    if origins.is_empty() {
        return Ok(None);
    }
    let allow = if origins.contains(&"*") {
        AllowOrigin::any()
    } else {
        let origins = origins
            .iter()
            .map(|o| HeaderValue::from_str(o))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("bad CORS origin: {e}"))?;
        AllowOrigin::list(origins)
    };
    let layer = CorsLayer::new()
        .allow_origin(allow)
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
        // An async request prefers `respond-async`, and its 202 names the job.
        .allow_headers([
            header::CONTENT_TYPE,
            header::HeaderName::from_static("prefer"),
            header::HeaderName::from_static(pisa_protocol::scope::HEADER),
        ])
        .expose_headers([
            header::LOCATION,
            header::HeaderName::from_static("preference-applied"),
        ])
        .max_age(Duration::from_secs(3600));
    Ok(Some(layer))
}

fn parse_alias(s: &str) -> Result<(String, String), String> {
    let (k, v) = s.split_once('=').ok_or("expected NAME=VERSION")?;
    Ok((k.trim().to_string(), v.trim().to_string()))
}

pub struct Running {
    pub http_addr: SocketAddr,
    pub app: Arc<App>,
    stop: watch::Sender<bool>,
    served: JoinHandle<()>,
    sweeper: JoinHandle<()>,
}

impl Running {
    /// Fails `/health` but keeps serving for `grace`, while load balancers
    /// stop sending requests here; then stops taking connections, and waits
    /// up to `drain` for the requests in flight. Their jobs are in the queue:
    /// one cut off still runs, and its result is swept.
    pub async fn shutdown(self, grace: Duration, drain: Duration) {
        self.app.draining.store(true, Ordering::Release);
        tokio::time::sleep(grace).await;
        let _ = self.stop.send(true);
        if tokio::time::timeout(drain, self.served).await.is_err() {
            tracing::warn!("requests still in flight after {drain:?}");
        }
        self.sweeper.abort();
    }
}

/// Serves the API, with its `/health`, from the queue in `db`.
pub async fn start(cfg: RouterConfig, db: Db) -> anyhow::Result<Running> {
    let versions = cfg.versions();
    start_with(cfg, versions, db, |app| {
        axum::Router::new()
            .route("/health", axum::routing::get(http::health))
            .with_state(app)
    })
    .await
}

/// Serves the API and the routes `extra` makes, which must include
/// `/health`, from the queue in `db`. Requests' versions resolve through
/// `versions`, rather than `cfg`'s, so that whatever else serves with the API
/// resolves them the same way.
pub async fn start_with(
    cfg: RouterConfig,
    versions: Versions,
    db: Db,
    extra: impl FnOnce(Arc<App>) -> axum::Router,
) -> anyhow::Result<Running> {
    let snapshots = Readers::new(db.clone(), cfg.allowed_axioms.clone());
    let queue = Queue::new(db, cfg.queue.clone());
    let dispatcher = Dispatcher::start(
        queue.clone(),
        DispatchConfig {
            max_wait: Duration::from_secs(cfg.max_wait_secs),
            max_retries: cfg.max_retries,
            ..Default::default()
        },
    )
    .await;
    let app = Arc::new(App {
        dispatcher: Arc::new(dispatcher),
        versions,
        snapshots,
        draining: AtomicBool::new(false),
    });

    let sweeper = tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            match queue.sweep().await {
                Ok(n) => tracing::debug!(deleted = n, "swept the job queue"),
                Err(e) => tracing::warn!("sweeping the job queue failed: {e}"),
            }
        }
    });

    let listener = TcpListener::bind(cfg.http).await?;
    let http_addr = listener.local_addr()?;
    let mut routes = http::router(app.clone())
        .merge(extra(app.clone()))
        .layer(axum::middleware::from_fn(http::scoped));
    if let Some(cors) = cors(&cfg.cors_origins)? {
        routes = routes.layer(cors);
    }
    let (stop, mut stopping) = watch::channel(false);
    let served = tokio::spawn(async move {
        // Dropping `Running` leaves the server serving.
        let stopped = async move {
            if stopping.wait_for(|stop| *stop).await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        if let Err(e) = axum::serve(listener, routes)
            .with_graceful_shutdown(stopped)
            .await
        {
            tracing::error!("http server failed: {e}");
        }
    });
    tracing::info!(%http_addr, queue = %cfg.queue, "router listening");
    Ok(Running {
        http_addr,
        app,
        stop,
        served,
        sweeper,
    })
}
