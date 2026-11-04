//! Pisa's server: the check, session and snapshot API, which hands jobs to
//! the workers through the database's job queue, and declaration search, in
//! one process. Any number of them can serve one database.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{any, get};
use axum::{Json, Router};
use pisa_db::db::Db;
use pisa_router::http::App;
use pisa_router::{RouterConfig, Running};
use pisa_search::server::{Search, SearchArgs};
use serde_json::Value;

#[derive(Debug, Clone, clap::Parser)]
#[command(about = "Pisa server: the HTTP API for checks, sessions, snapshots and search")]
pub struct ServerConfig {
    #[command(flatten)]
    pub router: RouterConfig,
    /// The database that holds the job queue, shared with the workers, and
    /// the snapshot catalog that search reads: PostgreSQL (`postgres://…`),
    /// or a SQLite file for a single machine.
    #[arg(long, env = "PISA_DATABASE_URL")]
    pub database_url: String,
    /// On shutdown, how long to wait for the requests in flight, each of
    /// which may wait `--max-wait-secs` for a worker and then for its job.
    #[arg(long, env = "PISA_DRAIN_TIMEOUT_SECS", default_value_t = 360)]
    pub drain_timeout_secs: u64,
    #[command(flatten)]
    pub search: SearchArgs,
}

pub struct Server {
    pub running: Running,
    pub search: Option<Arc<Search>>,
}

/// Connects to the database and serves.
pub async fn start(cfg: ServerConfig) -> anyhow::Result<Server> {
    let db = Db::connect(Some(&cfg.database_url))
        .await
        .map_err(|e| anyhow::anyhow!("connecting to the database: {e}"))?;
    start_on(cfg, db).await
}

/// Serves from `db`, ignoring `cfg.database_url`.
pub async fn start_on(cfg: ServerConfig, db: Db) -> anyhow::Result<Server> {
    let search = Search::start(cfg.search, db.clone()).await?;
    let searching = search.clone();
    let extra = move |app: Arc<App>| {
        let health = Router::new()
            .route("/health", get(health))
            .with_state((app, searching.clone()));
        match searching {
            Some(search) => health.merge(pisa_search::server::routes(search)),
            None => health.route("/v1/{*path}", any(no_search)),
        }
    };
    let running = pisa_router::start_with(cfg.router, db, extra).await?;
    Ok(Server { running, search })
}

/// 200 whenever the API is up, as checks don't need search, with the live
/// workers; `search` says whether it can serve.
async fn health(State((app, search)): State<(Arc<App>, Option<Arc<Search>>)>) -> Json<Value> {
    let mut health = app.health().await;
    health["search"] = match search {
        Some(search) => search.health().await.1,
        None => Value::Null,
    };
    Json(health)
}

async fn no_search() -> pisa_router::http::ApiError {
    pisa_router::http::ApiError::new(
        pisa_protocol::api::ErrorKind::NotFound,
        "this server doesn't search: it has no --qdrant",
    )
}
