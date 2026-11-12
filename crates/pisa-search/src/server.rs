//! HTTP API; types and conventions in `pisa_protocol::search`. `pisa-server`
//! serves it next to the rest of pisa's API, from [`Search`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{FromRequest, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pisa_protocol::api::{ApiErrors, Error, ErrorKind, ErrorResponse};
use pisa_protocol::search::{
    GetDeclarationResponse, ResolveRequest, ResolveResponse, SearchHealth, SearchRequest,
    SearchResponse,
};
use pisa_protocol::version::Versions;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use utoipa::IntoParams;

use crate::dense::Embedder;
use crate::engine::Lanes;
use crate::engine::{Engine, SearchError};
use crate::llm::Llm;
use crate::qdrant::Qdrant;
use crate::symbols::{self, Symbols};
use crate::{indexer, informalize};

/// Most queries or names one request may carry.
const MAX_BATCH: usize = 64;

/// The longest wait between tries to open the engine.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How a server searches, if it does.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SearchArgs {
    /// Qdrant's REST URL. Without it, the server doesn't search.
    #[arg(long = "qdrant", env = "PISA_SEARCH_QDRANT")]
    pub qdrant: Option<String>,
    /// The environment variable holding Qdrant's API key, if it needs one.
    #[arg(long = "qdrant-key-env", env = "PISA_SEARCH_QDRANT_KEY_ENV")]
    pub qdrant_key_env: Option<String>,
    /// The environment whose loaded bundle to serve; required with `--qdrant`.
    #[arg(long, env = "PISA_SEARCH_ENV_ID")]
    pub env_id: Option<String>,
    /// Symbol dictionary; defaults to the shipped search/symbols.toml.
    #[arg(long, env = "PISA_SEARCH_SYMBOLS")]
    pub symbols: Option<PathBuf>,
    /// Embedding server for queries (enables the dense lane); see `embed`.
    /// It also embeds snapshot declarations, so it must serve the model
    /// that embedded the bundle.
    #[arg(long, alias = "tei", env = "PISA_SEARCH_TEI")]
    pub embedder: Option<String>,
    /// Don't search the workers' snapshots, from the database's catalog.
    #[arg(long, env = "PISA_SEARCH_NO_SNAPSHOTS")]
    pub no_snapshots: bool,
    /// The worker version whose snapshots to search (default: the env_id).
    #[arg(long, env = "PISA_SEARCH_SNAPSHOT_VERSION")]
    pub snapshot_version: Option<String>,
    /// Claude or OpenAI model that describes every snapshot declaration
    /// (needs ANTHROPIC_API_KEY or OPENAI_API_KEY); without one they are
    /// searched by name, signature and docstring.
    #[arg(long, env = "PISA_SEARCH_DESCRIBE_MODEL")]
    pub describe_model: Option<String>,
    /// LLM and embedding requests in flight for snapshots.
    #[arg(long, env = "PISA_SEARCH_DESCRIBE_CONCURRENCY", default_value_t = 8)]
    pub describe_concurrency: usize,
}

/// Where snapshots to search come from, and how to describe them.
struct Snapshots {
    db: pisa_db::db::Db,
    /// The workers' version.
    version: String,
    llm: Option<(Llm, informalize::Config)>,
    concurrency: usize,
}

/// Search over a bundle loaded into Qdrant, and the snapshots in the
/// database. Its engine opens in the background: until Qdrant answers with
/// the bundle, requests get a 503.
pub struct Search {
    engine: OnceLock<Arc<Engine>>,
    env_id: String,
    /// How a request's version resolves, as the rest of the API does.
    versions: Versions,
    q: Qdrant,
    /// Why the engine isn't open yet.
    waiting: Mutex<String>,
}

impl Search {
    /// Starts search as `args` say, with the snapshots in `db`, resolving
    /// requests' versions through `versions`; `None` if `args` name no
    /// Qdrant. Bad settings are errors now; Qdrant being down, or the bundle
    /// not loaded yet, only delays search.
    pub async fn start(
        args: SearchArgs,
        db: pisa_db::db::Db,
        versions: Versions,
    ) -> anyhow::Result<Option<Arc<Search>>> {
        // Compose passes unset variables as empty ones.
        let some = |s: Option<String>| s.filter(|s| !s.is_empty());
        let Some(url) = some(args.qdrant) else {
            return Ok(None);
        };
        let env_id = some(args.env_id)
            .ok_or_else(|| anyhow::anyhow!("searching needs --env-id with --qdrant"))?;
        let q = Qdrant::with_key_env(&url, some(args.qdrant_key_env).as_deref())?;
        let symbols = match args.symbols {
            Some(p) => Symbols::parse(&std::fs::read_to_string(p)?)?,
            None => Symbols::parse(symbols::DEFAULT)?,
        };
        let embedder = match some(args.embedder) {
            // Its blocking client mustn't be made on an async thread.
            Some(spec) => Some(tokio::task::spawn_blocking(move || Embedder::parse(&spec)).await??),
            None => None,
        };
        let llm = match some(args.describe_model) {
            Some(model) => Some((
                Llm::for_model(&model)?,
                informalize::Config {
                    model,
                    effort: "low".into(),
                    max_tokens: 4096,
                    concurrency: args.describe_concurrency,
                },
            )),
            None => None,
        };
        let snapshots = (!args.no_snapshots).then(|| Snapshots {
            version: some(args.snapshot_version).unwrap_or_else(|| env_id.clone()),
            db,
            llm,
            concurrency: args.describe_concurrency,
        });
        let search = Arc::new(Search {
            engine: OnceLock::new(),
            env_id,
            versions,
            q,
            waiting: Mutex::new("opening the engine".into()),
        });
        tokio::spawn(search.clone().open(symbols, embedder, snapshots));
        Ok(Some(search))
    }

    /// Search over `engine`, open already.
    pub fn ready(engine: Arc<Engine>, versions: Versions) -> Arc<Search> {
        let search = Search {
            env_id: engine.env_id().to_string(),
            versions,
            q: engine.q.clone(),
            engine: OnceLock::from(engine),
            waiting: Mutex::new(String::new()),
        };
        Arc::new(search)
    }

    /// Opens the engine, trying again until it opens, then indexes the
    /// snapshots, if it searches them.
    async fn open(
        self: Arc<Self>,
        symbols: Symbols,
        embedder: Option<Embedder>,
        snapshots: Option<Snapshots>,
    ) {
        let mut backoff = Duration::from_secs(1);
        let mut engine = loop {
            match Engine::open(
                self.q.clone(),
                &self.env_id,
                symbols.clone(),
                embedder.clone(),
            )
            .await
            {
                Ok(engine) => break engine,
                Err(e) => {
                    let why = e.to_string();
                    let mut waiting = self.waiting.lock().unwrap();
                    if *waiting != why {
                        tracing::warn!("search isn't ready: {why}");
                        *waiting = why;
                    }
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        };
        engine.snapshots = snapshots.as_ref().map(|s| s.version.clone());
        let engine = Arc::new(engine);
        if let Some(s) = snapshots {
            tracing::info!(version = %s.version, "searching snapshots");
            // Snapshot vectors join the bundle's, so without those there are none.
            let embedder = embedder.filter(|_| engine.manifest.embedding_model.is_some());
            let how = indexer::Describe {
                llm: s.llm,
                embedder,
            };
            let indexer = indexer::Indexer::new(
                engine.clone(),
                s.db,
                s.version,
                how,
                s.concurrency,
                indexer::LEASE_TTL,
            );
            tokio::spawn(indexer.run());
        }
        tracing::info!(
            env_id = engine.env_id(),
            decls = engine.manifest.decls,
            snapshots = engine.snapshots.is_some(),
            "searching"
        );
        let _ = self.engine.set(engine);
    }

    fn engine(&self) -> Result<&Arc<Engine>, ApiError> {
        self.engine.get().ok_or_else(|| {
            let why = self.waiting.lock().unwrap().clone();
            ApiError::new(ErrorKind::Unavailable, format!("search isn't ready: {why}"))
        })
    }

    /// Search's state, for a server's `/health`, and whether it can serve.
    pub async fn health(&self) -> (bool, SearchHealth) {
        let qdrant = self.q.healthy().await;
        let engine = self.engine.get();
        let ready = engine.is_some() && qdrant;
        let health = SearchHealth {
            ready,
            version: self.env_id.clone(),
            qdrant,
            leader: engine.is_some_and(|e| e.leader.load(std::sync::atomic::Ordering::Acquire)),
            error: engine
                .is_none()
                .then(|| self.waiting.lock().unwrap().clone()),
        };
        (ready, health)
    }
}

/// Search's OpenAPI document.
#[derive(utoipa::OpenApi)]
#[openapi(
    components(schemas(ErrorResponse)),
    paths(search_handler, resolve, get_declaration)
)]
pub struct ApiDoc;

/// Search's routes, to serve next to others.
pub fn routes(search: Arc<Search>) -> Router {
    Router::new()
        .route("/search", post(search_handler))
        .route("/resolve", post(resolve))
        .route("/declarations/{name}", get(get_declaration))
        .with_state(search)
}

/// Search's routes on their own, with a `/health` that is 503 while search
/// can't serve, and the API's 404s.
pub fn router(search: Arc<Search>) -> Router {
    let health = Router::new()
        .route("/health", get(health))
        .with_state(search.clone());
    routes(search)
        .merge(health)
        .fallback(|| async { ApiError::new(ErrorKind::NotFound, "no such endpoint") })
}

pub struct ApiError(Error);

impl ApiError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        ApiError(Error::new(kind, message))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.kind.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let retry_after = self.0.kind.retry_after();
        let mut response = (status, Json(ErrorResponse { error: self.0 })).into_response();
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, seconds.into());
        }
        response
    }
}

/// A JSON body whose rejections use the API's error shape.
pub struct Body<T>(T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Body(v)),
            Err(r) => Err(ApiError::new(ErrorKind::BadRequest, r.body_text())),
        }
    }
}

impl Search {
    /// Fails unless `requested` names this search's index or snapshots.
    fn check_version(&self, engine: &Engine, requested: Option<&str>) -> Result<(), ApiError> {
        let snapshots = engine.snapshots.as_deref();
        check_version(&self.versions, engine.env_id(), snapshots, requested)
    }
}

/// Fails with `not_found` unless `requested`, through `versions`' aliases, is
/// the `index`'s version or the `snapshots`'. A request that names none gets
/// the index, whatever the default version.
fn check_version(
    versions: &Versions,
    index: &str,
    snapshots: Option<&str>,
    requested: Option<&str>,
) -> Result<(), ApiError> {
    let Some(requested) = requested else {
        return Ok(());
    };
    let version = versions.resolve(Some(requested));
    match version.as_deref() {
        Some(v) if v == index || Some(v) == snapshots => Ok(()),
        _ => Err(ApiError::new(
            ErrorKind::NotFound,
            format!("no index for version {requested}"),
        )),
    }
}

fn check_batch(n: usize) -> Result<(), ApiError> {
    if n > MAX_BATCH {
        return Err(ApiError::new(
            ErrorKind::BadRequest,
            format!("at most {MAX_BATCH} per request, got {n}"),
        ));
    }
    Ok(())
}

async fn health(State(search): State<Arc<Search>>) -> (StatusCode, Json<serde_json::Value>) {
    let (ready, health) = search.health().await;
    let mut body = json!(health);
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    body["status"] = if ready { "ok" } else { "unavailable" }.into();
    (status, Json(body))
}

fn unavailable(e: impl std::fmt::Display) -> ApiError {
    tracing::warn!("search backend failed: {e}");
    ApiError::new(
        ErrorKind::Unavailable,
        format!("search backend unavailable: {e}"),
    )
}

#[utoipa::path(
    post, path = "/search", operation_id = "search", tag = "search",
    summary = "Search declarations",
    description = "Finds Mathlib and Physlib declarations, and snapshot declarations when asked \
        for, from a description, LaTeX, a Lean pattern or goal, or a partial or misspelled name. \
        At most 64 queries; each gets a result, in order. A 404 on a server without search.",
    request_body = SearchRequest,
    responses((status = 200, description = "A result per query, in order", body = SearchResponse), ApiErrors),
)]
async fn search_handler(
    State(search): State<Arc<Search>>,
    Body(mut req): Body<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    let engine = search.engine()?;
    search.check_version(engine, req.version.as_deref())?;
    check_batch(req.queries.len())?;
    for q in &mut req.queries {
        engine
            .check_filters(q)
            .map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    }
    let results = match engine.search_batch(&req.queries, Lanes::ALL).await {
        Ok(results) => results,
        Err(SearchError::BadRequest(m)) => return Err(ApiError::new(ErrorKind::BadRequest, m)),
        Err(SearchError::Unavailable(e)) => return Err(unavailable(e)),
    };
    let version = engine.env_id().to_string();
    Ok(Json(SearchResponse { version, results }))
}

#[utoipa::path(
    post, path = "/resolve", operation_id = "resolve", tag = "search",
    summary = "Check names exist",
    description = "Whether each name exists, with close names for one that doesn't.",
    request_body = ResolveRequest,
    responses((status = 200, description = "A resolution per name, in order", body = ResolveResponse), ApiErrors),
)]
async fn resolve(
    State(search): State<Arc<Search>>,
    Body(req): Body<ResolveRequest>,
) -> Result<Json<ResolveResponse>, ApiError> {
    let engine = search.engine()?;
    search.check_version(engine, req.version.as_deref())?;
    check_batch(req.names.len())?;
    let results = engine.resolve(&req.names).await.map_err(unavailable)?;
    let version = engine.env_id().to_string();
    Ok(Json(ResolveResponse { version, results }))
}

/// `GET /declarations/{name}`'s query.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct GetDeclarationQuery {
    /// A snapshot module to look in first.
    module: Option<String>,
    /// A version, or an alias of one: the index's, or its snapshots'. Any
    /// other is a 404.
    version: Option<String>,
}

#[utoipa::path(
    get, path = "/declarations/{name}", operation_id = "getDeclaration", tag = "search",
    summary = "A declaration's details",
    description = "Its signature, description, flags, dependencies and users: a snapshot's \
        declaration with a snapshot hit's `module`, else the bundle's.",
    params(("name" = String, Path, description = "The full name"), GetDeclarationQuery),
    responses((status = 200, description = "The declaration", body = GetDeclarationResponse), ApiErrors),
)]
async fn get_declaration(
    State(search): State<Arc<Search>>,
    Path(name): Path<String>,
    Query(q): Query<GetDeclarationQuery>,
) -> Result<Json<GetDeclarationResponse>, ApiError> {
    let engine = search.engine()?;
    search.check_version(engine, q.version.as_deref())?;
    match engine
        .detail(&name, q.module.as_deref())
        .await
        .map_err(unavailable)?
    {
        Some(declaration) => Ok(Json(GetDeclarationResponse {
            version: engine.env_id().to_string(),
            declaration,
        })),
        None => {
            let r = engine
                .resolve(std::slice::from_ref(&name))
                .await
                .map_err(unavailable)?;
            let hint = r
                .first()
                .and_then(|r| r.suggestions.first())
                .map(|s| format!("; did you mean {}?", s.name));
            Err(ApiError::new(
                ErrorKind::NotFound,
                format!("unknown declaration {name}{}", hint.unwrap_or_default()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn a_request_names_the_index_or_the_snapshots_version() {
        let versions = Versions {
            default: Some("pool".into()),
            aliases: HashMap::from([
                ("latest".into(), "index".into()),
                ("snaps".into(), "snapshots".into()),
            ]),
        };
        let check = |requested| check_version(&versions, "index", Some("snapshots"), requested);
        // No version is whatever the server has, the default pool's or not.
        for ok in [
            None,
            Some("index"),
            Some("snapshots"),
            Some("latest"),
            Some("snaps"),
        ] {
            assert!(check(ok).is_ok(), "{ok:?}");
        }
        for other in ["other", "pool"] {
            let ApiError(e) = check(Some(other)).unwrap_err();
            assert_eq!(e.kind, ErrorKind::NotFound);
            assert_eq!(e.message, format!("no index for version {other}"));
        }
        // Without snapshots, only the index's.
        assert!(check_version(&versions, "index", None, Some("snapshots")).is_err());
    }
}
