//! Pisa's server: the check, session and snapshot API, which hands jobs to
//! the workers through the database's job queue, and declaration search, in
//! one process. Any number of them can serve one database.

use std::sync::{Arc, LazyLock};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{any, get};
use axum::{Json, Router};
use pisa_db::db::Db;
use pisa_protocol::api::Health;
use pisa_router::http::App;
use pisa_router::{RouterConfig, Running};
use pisa_search::server::{Search, SearchArgs};
use serde_json::{Value, json};
use utoipa::OpenApi;

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
            .route("/openapi.json", get(|| async { Json(&*SPEC) }))
            .with_state((app, searching.clone()));
        match searching {
            Some(search) => health.merge(pisa_search::server::routes(search)),
            None => health.route("/v1/{*path}", any(no_search)),
        }
    };
    let running = pisa_router::start_with(cfg.router, db, extra).await?;
    Ok(Server { running, search })
}

/// The API's health, with the live workers: 200 whenever it is up, as
/// checks don't need search, until the server drains. `search` says whether
/// search can serve.
#[utoipa::path(
    get, path = "/health", operation_id = "health", tag = "health",
    summary = "The server's health",
    description = "200 whenever the API is up, as checks don't need search, with the live \
        workers and search's state; 503 once the server is stopping.",
    responses(
        (status = 200, description = "Up", body = Health),
        (status = 503, description = "Draining: the server is stopping", body = Health),
    ),
)]
async fn health(
    State((app, search)): State<(Arc<App>, Option<Arc<Search>>)>,
) -> (StatusCode, Json<Health>) {
    let (status, mut health) = app.health().await;
    if let Some(search) = search {
        health.search = Some(search.health().await.1);
    }
    (status, Json(health))
}

/// The OpenAPI document of the API and search, which `/openapi.json` serves.
pub fn openapi() -> Value {
    #[derive(OpenApi)]
    #[openapi(
        info(
            title = "pisa",
            description = "Checks Lean code, runs proof sessions, keeps snapshots and searches \
                declarations. Errors are `{error: {kind, message}}`, with a status that follows \
                the kind. `/v1/*` is a 404 on a server without search.",
        ),
        paths(health),
        tags(
            (name = "checks", description = "Checking code"),
            (name = "sessions", description = "Proving `sorry`s a tactic at a time"),
            (name = "snapshots", description = "Keeping code for later code to import"),
            (name = "search", description = "Finding declarations"),
            (name = "health", description = "Whether a server can take requests"),
        ),
        servers((url = "/", description = "The server that serves this document")),
    )]
    struct Doc;
    let mut doc = Doc::openapi();
    doc.merge(pisa_router::http::ApiDoc::openapi());
    doc.merge(pisa_search::server::ApiDoc::openapi());
    let mut spec = json!(doc);
    absent_not_null(&mut spec);
    spec
}

static SPEC: LazyLock<Value> = LazyLock::new(openapi);

/// Drops `null` from what may be absent: properties that aren't required,
/// and optional parameters. The protocol leaves out a `None` rather than
/// sending `null`, and sends a field that can be `null` always, marked
/// required; see `pisa_protocol`.
fn absent_not_null(v: &mut Value) {
    match v {
        Value::Object(o) => {
            let required: Vec<Value> = match o.get("required") {
                Some(Value::Array(names)) => names.clone(),
                _ => Vec::new(),
            };
            if let Some(Value::Object(props)) = o.get_mut("properties") {
                for (name, p) in props.iter_mut() {
                    if !required.contains(&json!(name)) {
                        drop_null(p);
                    }
                }
            }
            let parameter = o.contains_key("in") && o.get("required") != Some(&json!(true));
            if parameter && let Some(schema) = o.get_mut("schema") {
                drop_null(schema);
            }
            o.values_mut().for_each(absent_not_null);
        }
        Value::Array(a) => a.iter_mut().for_each(absent_not_null),
        _ => {}
    }
}

/// `schema` without its `null` alternative: `type: [T, "null"]` becomes
/// `type: T`, and `oneOf: [{type: "null"}, S]` becomes `S`.
fn drop_null(schema: &mut Value) {
    let Value::Object(o) = schema else { return };
    if let Some(Value::Array(types)) = o.get_mut("type") {
        types.retain(|t| t != "null");
        if let [only] = types.as_slice() {
            let only = only.clone();
            o.insert("type".into(), only);
        }
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(alternatives)) = o.get_mut(key) {
            alternatives.retain(|a| a.get("type") != Some(&json!("null")));
            if let [Value::Object(only)] = alternatives.as_slice() {
                let only = only.clone();
                o.remove(key);
                for (k, v) in only {
                    o.entry(k).or_insert(v);
                }
            }
        }
    }
}

async fn no_search() -> pisa_router::http::ApiError {
    pisa_router::http::ApiError::new(
        pisa_protocol::api::ErrorKind::NotFound,
        "this server doesn't search: it has no --qdrant",
    )
}
