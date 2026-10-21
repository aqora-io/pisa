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

use crate::engine::Engine;

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

async fn health(State(engine): State<Arc<Engine>>) -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "env_id": engine.env_id() }))
}

async fn search(
    State(engine): State<Arc<Engine>>,
    Body(req): Body<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    check_env(&engine, req.env_id.as_deref())?;
    check_batch(req.queries.len())?;
    let env_id = engine.env_id().to_string();
    let results =
        tokio::task::spawn_blocking(move || req.queries.iter().map(|q| engine.search(q)).collect())
            .await
            .map_err(|e| ApiError::new(ErrorKind::Internal, e.to_string()))?;
    Ok(Json(SearchResponse { env_id, results }))
}

async fn resolve(
    State(engine): State<Arc<Engine>>,
    Body(req): Body<ResolveRequest>,
) -> Result<Json<ResolveResponse>, ApiError> {
    check_env(&engine, req.env_id.as_deref())?;
    check_batch(req.names.len())?;
    let env_id = engine.env_id().to_string();
    let results =
        tokio::task::spawn_blocking(move || req.names.iter().map(|n| engine.resolve(n)).collect())
            .await
            .map_err(|e| ApiError::new(ErrorKind::Internal, e.to_string()))?;
    Ok(Json(ResolveResponse { env_id, results }))
}

async fn decl(
    State(engine): State<Arc<Engine>>,
    Path(name): Path<String>,
) -> Result<Json<DeclResponse>, ApiError> {
    match engine.detail(&name) {
        Some(decl) => Ok(Json(DeclResponse {
            env_id: engine.env_id().to_string(),
            decl,
        })),
        None => {
            let r = engine.resolve(&name);
            let hint = r
                .suggestions
                .first()
                .map(|s| format!("; did you mean {}?", s.name));
            Err(ApiError::new(
                ErrorKind::NotFound,
                format!("unknown declaration {name}{}", hint.unwrap_or_default()),
            ))
        }
    }
}
