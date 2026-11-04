//! Client-facing HTTP API; the types and conventions are in
//! `pisa_protocol::api`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{FromRequest, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::future::join_all;
use pisa_protocol::api::{
    self, BatchRequest, BatchResponse, CheckRequest, CheckResult, Detail, ErrorKind, ErrorResponse,
};
use pisa_protocol::header::split_header;
use pisa_protocol::snapshot;
use pisa_protocol::wire::{CheckJob, CheckOutcome, Job, JobResult};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::dispatch::{DispatchError, Dispatcher, Priority, Route};
use crate::metrics as m;
use crate::{sessions, snapshots};

pub struct App {
    pub dispatcher: Arc<Dispatcher>,
    pub versions: Versions,
}

#[derive(Debug, Clone, Default)]
pub struct Versions {
    pub default: Option<String>,
    pub aliases: HashMap<String, String>,
}

impl Versions {
    /// The pool label for a requested version (or the default), following aliases.
    pub fn resolve(&self, requested: Option<&str>) -> Option<String> {
        let v = requested
            .map(str::to_string)
            .or_else(|| self.default.clone())?;
        Some(self.aliases.get(&v).cloned().unwrap_or(v))
    }
}

/// The API's routes, without `/health`, which whoever serves them adds.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .route("/check", post(check))
        .route("/check/batch", post(check_batch))
        .route("/sessions", post(sessions::start))
        .route("/sessions/{id}/check", post(sessions::check))
        .route("/states/{id}/tactic", post(sessions::tactic))
        .route("/snapshots", post(snapshots::create).get(snapshots::list))
        .route(
            "/snapshots/{entry}",
            get(snapshots::get)
                .patch(snapshots::labels)
                .delete(snapshots::delete),
        )
        .route("/snapshots/{entry}/verify", post(snapshots::verify))
        .fallback(|| async { ApiError::new(ErrorKind::NotFound, "no such endpoint") })
        .with_state(app)
}

/// An error response: the status follows the kind, the body is [`ErrorResponse`].
pub struct ApiError(pub api::Error);

impl ApiError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        ApiError(api::Error::new(kind, message))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.kind.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(ErrorResponse { error: self.0 })).into_response()
    }
}

impl From<DispatchError> for api::Error {
    fn from(e: DispatchError) -> Self {
        let kind = match e {
            DispatchError::NoCapacity => ErrorKind::Capacity,
            DispatchError::WorkerLost(_) | DispatchError::Unavailable(_) => ErrorKind::Unavailable,
        };
        api::Error::new(kind, e.to_string())
    }
}

impl From<DispatchError> for ApiError {
    fn from(e: DispatchError) -> Self {
        ApiError(e.into())
    }
}

/// A JSON body whose rejections use the API's error shape.
pub struct Body<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Body(v)),
            Err(rejection) => Err(ApiError::new(ErrorKind::BadRequest, rejection.body_text())),
        }
    }
}

pub async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn metrics(State(app): State<Arc<App>>) -> String {
    app.dispatcher.record_gauges().await;
    m::handle().render()
}

async fn check(
    State(app): State<Arc<App>>,
    Body(req): Body<CheckRequest>,
) -> Result<Json<CheckResult>, ApiError> {
    let version = app.versions.resolve(req.version.as_deref());
    Ok(Json(
        check_one(&app, req.code, req.timeout, req.detail, version).await?,
    ))
}

/// Every item gets a result; failures, including running out of capacity,
/// are reported in that item's `error`.
async fn check_batch(
    State(app): State<Arc<App>>,
    Body(req): Body<BatchRequest>,
) -> Json<BatchResponse> {
    let version = app.versions.resolve(req.version.as_deref());
    let items = req.items.into_iter().map(|item| {
        let app = &app;
        let version = version.clone();
        let detail = req.detail.clone();
        async move {
            let mut result = match check_one(app, item.code, req.timeout, detail, version).await {
                Ok(r) => r,
                Err(e) => CheckResult {
                    error: Some(e.into()),
                    ..Default::default()
                },
            };
            result.id = Some(item.id);
            result
        }
    });
    Json(BatchResponse {
        results: join_all(items).await,
    })
}

async fn check_one(
    app: &App,
    code: String,
    timeout_secs: u64,
    detail: Vec<Detail>,
    version: Option<String>,
) -> Result<CheckResult, DispatchError> {
    let split = split_header(&code);
    let route = Route {
        version,
        header_key: Some(split.header_key),
        header: prewarm_header(split.header),
        state: None,
    };
    let job = Job::Check(CheckJob {
        code,
        timeout_secs,
        detail,
    });
    let done = app.dispatcher.submit(job, route, Priority::Batch).await?;
    Ok(match done.result {
        JobResult::Check(o) => check_result(o, "check"),
        other => failed_result(other, "check"),
    })
}

/// The header new workers may pre-warm, when it is in demand. Headers that import
/// snapshots are left out: there is a new one for every statement, and a new
/// worker would mostly pre-warm abandoned ones.
pub fn prewarm_header(header: String) -> Option<String> {
    snapshot::imports(&header)
        .is_ok_and(|ids| ids.is_empty())
        .then_some(header)
}

/// Turns a worker's check outcome into the API result, recording metrics.
pub fn check_result(o: CheckOutcome, kind: &'static str) -> CheckResult {
    let hit = if o.warm { "hit" } else { "miss" };
    metrics::counter!(m::HEADER_CACHE, "result" => hit).increment(1);
    let result = CheckResult::from(o);
    record_result(&result, kind);
    result
}

/// Counts a finished job and the reasons it was rejected.
pub fn record_result(result: &CheckResult, kind: &'static str) {
    result.reasons.iter().for_each(|r| record_rejection(r));
    let outcome = if result.error.is_some() {
        "error"
    } else {
        "ok"
    };
    metrics::counter!(m::JOBS, "kind" => kind, "outcome" => outcome).increment(1);
}

/// A worker answered with something other than a check outcome.
pub fn failed_result(result: JobResult, kind: &'static str) -> CheckResult {
    metrics::counter!(m::JOBS, "kind" => kind, "outcome" => "failed").increment(1);
    let message = match result {
        JobResult::Failed { error } => error,
        other => format!("unexpected worker result: {other:?}"),
    };
    CheckResult {
        error: Some(api::Error::new(ErrorKind::Internal, message)),
        ..Default::default()
    }
}

/// Counts a rejection under the reason's prefix, e.g. "axiom" or "statement".
pub fn record_rejection(reason: &str) {
    let kind = reason.split(':').next().unwrap_or(reason).to_string();
    metrics::counter!(m::REJECTIONS, "reason" => kind).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_headers_are_not_prewarmed() {
        assert_eq!(
            prewarm_header("import Mathlib".into()),
            Some("import Mathlib".into())
        );
        let snapshot = format!("import Mathlib\nimport PisaSnap.s{}", "0".repeat(32));
        assert_eq!(prewarm_header(snapshot), None);
        assert_eq!(prewarm_header("import PisaSnap.oops".into()), None);
    }
}
