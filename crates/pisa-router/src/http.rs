//! Client-facing HTTP API, compatible with Kimina Lean Server.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::future::join_all;
use pisa_protocol::header::split_header;
use pisa_protocol::kimina::{
    CheckRequest, CheckResponse, CheckResult, VerifyRequest, VerifyResponse, VerifyResult,
};
use pisa_protocol::wire::{CheckJob, CheckOutcome, Job, JobResult};
use serde_json::json;

use crate::dispatch::{DispatchError, Dispatcher, Priority, Route};
use crate::metrics as m;

pub struct App {
    pub dispatcher: Arc<Dispatcher>,
    pub versions: Versions,
    pub affinity: crate::sessions::Affinity,
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

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(health))
        .route("/health", get(health))
        .route("/api/check", post(check))
        .route("/verify", post(verify))
        .route("/one_pass_verify_batch", post(verify))
        .route("/sessions", post(crate::sessions::start))
        .route("/sessions/{id}/tactic", post(crate::sessions::tactic))
        .route("/sessions/{id}/check", post(crate::sessions::check))
        .route("/metrics", get(metrics))
        .with_state(app)
}

pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // FastAPI's error shape, which Kimina clients expect.
        (self.0, Json(json!({ "detail": self.1 }))).into_response()
    }
}

impl From<DispatchError> for ApiError {
    fn from(e: DispatchError) -> Self {
        let status = match e {
            DispatchError::NoCapacity => StatusCode::TOO_MANY_REQUESTS,
            DispatchError::WorkerLost(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        ApiError(status, e.to_string())
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn metrics() -> String {
    m::handle().render()
}

async fn check(
    State(app): State<Arc<App>>,
    Json(req): Json<CheckRequest>,
) -> Result<Json<CheckResponse>, ApiError> {
    let version = app.versions.resolve(req.version.as_deref());
    let items = req.snippets.into_iter().map(|s| {
        let job = CheckJob {
            code: s.code,
            timeout_secs: req.timeout,
            infotree: req.infotree.clone(),
            reuse: req.reuse,
        };
        check_one(&app, s.id, job, version.clone())
    });
    let results = join_all(items)
        .await
        .into_iter()
        .collect::<Result<_, _>>()?;
    Ok(Json(CheckResponse { results }))
}

async fn verify(
    State(app): State<Arc<App>>,
    Json(req): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>, ApiError> {
    let version = app.versions.resolve(req.version.as_deref());
    let items = req.codes.iter().map(|c| {
        let job = CheckJob {
            code: c.text().to_string(),
            timeout_secs: req.timeout,
            infotree: req.infotree_type.clone(),
            reuse: !req.disable_cache,
        };
        check_one(&app, c.id(), job, version.clone())
    });
    let results = join_all(items)
        .await
        .into_iter()
        .map(|r| r.map(|r| VerifyResult::from_check(r.id.clone(), r)))
        .collect::<Result<_, _>>()?;
    Ok(Json(VerifyResponse { results }))
}

pub async fn check_one(
    app: &App,
    id: String,
    job: CheckJob,
    version: Option<String>,
) -> Result<CheckResult, DispatchError> {
    let split = split_header(&job.code);
    let route = Route {
        version,
        header_key: Some(split.header_key),
        header: Some(split.header),
        affinity: None,
    };
    let done = app
        .dispatcher
        .submit(Job::Check(job), route, Priority::Batch)
        .await?;
    match done.result {
        JobResult::Check(o) => Ok(check_result(id, o, "check")),
        JobResult::Failed { error } => Ok(failed_result(id, error, "check")),
        other => Ok(failed_result(
            id,
            format!("unexpected result: {other:?}"),
            "check",
        )),
    }
}

/// Turns a worker's check outcome into the API result, recording metrics.
pub fn check_result(id: String, o: CheckOutcome, kind: &'static str) -> CheckResult {
    let hit = if o.warm { "hit" } else { "miss" };
    metrics::counter!(m::HEADER_CACHE, "result" => hit).increment(1);
    if let Some(v) = o.verification.as_ref().filter(|v| !v.ok) {
        for reason in &v.reasons {
            record_rejection(reason);
        }
    }
    let outcome = if o.error.is_some() { "error" } else { "ok" };
    metrics::counter!(m::JOBS, "kind" => kind, "outcome" => outcome).increment(1);
    CheckResult {
        id,
        time: o.time,
        error: o.error,
        response: o.response,
        diagnostics: None,
        verification: o.verification,
    }
}

pub fn failed_result(id: String, error: String, kind: &'static str) -> CheckResult {
    metrics::counter!(m::JOBS, "kind" => kind, "outcome" => "failed").increment(1);
    CheckResult {
        id,
        time: 0.0,
        error: Some(error),
        response: None,
        diagnostics: None,
        verification: None,
    }
}

/// Counts a rejection under the reason's prefix, e.g. "axiom" or "statement".
pub fn record_rejection(reason: &str) {
    let kind = reason.split(':').next().unwrap_or(reason).to_string();
    metrics::counter!(m::REJECTIONS, "reason" => kind).increment(1);
}
