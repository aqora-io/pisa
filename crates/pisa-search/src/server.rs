//! HTTP API; types and conventions in `pisa_protocol::search`.

use std::sync::Arc;

use axum::extract::{FromRequest, Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pisa_protocol::api::{Error, ErrorKind, ErrorResponse};
use pisa_protocol::search::{
    DeclResponse, ResolveRequest, ResolveResponse, SearchRequest, SearchResponse,
};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::engine::Lanes;
use crate::engine::{Engine, SearchError};

/// Most queries or names one request may carry.
const MAX_BATCH: usize = 64;

pub fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/search", post(search))
        .route("/v1/resolve", post(resolve))
        .route("/v1/decl/{name}", get(decl))
        .fallback(|| async { ApiError::new(ErrorKind::NotFound, "no such endpoint") })
        .with_state(engine)
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
        (status, Json(ErrorResponse { error: self.0 })).into_response()
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

fn check_env(engine: &Engine, pinned: Option<&str>) -> Result<(), ApiError> {
    match pinned {
        Some(env) if env != engine.env_id() => Err(ApiError::new(
            ErrorKind::NotFound,
            format!(
                "no index for env {env}; this server has {}",
                engine.env_id()
            ),
        )),
        _ => Ok(()),
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

/// 503 while Qdrant doesn't answer, so a readiness probe takes the replica
/// out of rotation.
async fn health(State(engine): State<Arc<Engine>>) -> (StatusCode, Json<serde_json::Value>) {
    let qdrant = engine.q.healthy().await;
    let status = if qdrant {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = json!({
        "status": if qdrant { "ok" } else { "unavailable" },
        "env_id": engine.env_id(),
        "qdrant": qdrant,
        "leader": engine.leader.load(std::sync::atomic::Ordering::Acquire),
    });
    (status, Json(body))
}

fn unavailable(e: impl std::fmt::Display) -> ApiError {
    tracing::warn!("search backend failed: {e}");
    ApiError::new(
        ErrorKind::Unavailable,
        format!("search backend unavailable: {e}"),
    )
}

async fn search(
    State(engine): State<Arc<Engine>>,
    Body(mut req): Body<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    check_env(&engine, req.env_id.as_deref())?;
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
    let env_id = engine.env_id().to_string();
    Ok(Json(SearchResponse { env_id, results }))
}

async fn resolve(
    State(engine): State<Arc<Engine>>,
    Body(req): Body<ResolveRequest>,
) -> Result<Json<ResolveResponse>, ApiError> {
    check_env(&engine, req.env_id.as_deref())?;
    check_batch(req.names.len())?;
    let results = engine.resolve(&req.names).await.map_err(unavailable)?;
    let env_id = engine.env_id().to_string();
    Ok(Json(ResolveResponse { env_id, results }))
}

async fn decl(
    State(engine): State<Arc<Engine>>,
    Path(name): Path<String>,
) -> Result<Json<DeclResponse>, ApiError> {
    match engine.detail(&name).await.map_err(unavailable)? {
        Some(decl) => Ok(Json(DeclResponse {
            env_id: engine.env_id().to_string(),
            decl,
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
