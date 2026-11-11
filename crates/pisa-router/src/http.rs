//! Client-facing HTTP API; the types and conventions are in
//! `pisa_protocol::api`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::{FromRequest, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures::StreamExt;
use pisa_db::catalog::Catalog;
use pisa_db::queue::LIVE;
use pisa_protocol::api::{
    self, Accepted, ApiErrors, CheckBatchRequest, CheckBatchResponse, CheckRequest, CheckResult,
    Detail, Error, ErrorKind, ErrorResponse, Health, HealthStatus, JobKind, MAX_AT, MAX_BATCH, Pos,
    PreferAsync, RunImport, SweepRefused,
};
use pisa_protocol::header::split_header;
use pisa_protocol::snapshot;
use pisa_protocol::version::Versions;
use pisa_protocol::wire::{CheckJob, Job, JobResult};
use pisa_snapshots::Readers;
use serde::de::DeserializeOwned;

use crate::dispatch::{DispatchError, Dispatcher, Prepared, Priority, Route};
use crate::jobs::{self, Prefer};
use crate::{documents, sessions, snapshots};

pub struct App {
    pub dispatcher: Arc<Dispatcher>,
    pub versions: Versions,
    /// Reads the snapshot catalog, which the server answers itself.
    pub snapshots: Readers,
    /// Set when the server is about to stop: `/health` fails, so that load
    /// balancers stop sending it requests while it still serves them.
    pub draining: AtomicBool,
}

/// The API's OpenAPI document, without `/health`, which whoever serves the
/// API adds.
#[derive(utoipa::OpenApi)]
#[openapi(
    components(schemas(ErrorResponse)),
    paths(
        check,
        check_batch,
        sessions::start,
        sessions::apply_tactic,
        sessions::check,
        snapshots::create,
        snapshots::list,
        snapshots::get,
        snapshots::update,
        snapshots::delete,
        snapshots::verify,
        snapshots::dependents,
        snapshots::rebuild,
        snapshots::resubmit,
        snapshots::settle,
        snapshots::audit,
        snapshots::export,
        snapshots::accept,
        snapshots::acceptance,
        snapshots::acceptances,
        documents::open,
        documents::edit,
        documents::goals,
        documents::probe,
        documents::close,
        jobs::get,
        jobs::cancel,
    )
)]
pub struct ApiDoc;

/// The API's routes, without `/health`, which whoever serves them adds.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/check", post(check))
        .route("/check/batch", post(check_batch))
        .route("/sessions", post(sessions::start))
        .route("/sessions/{id}/check", post(sessions::check))
        .route("/states/{id}/tactic", post(sessions::apply_tactic))
        .route("/snapshots", post(snapshots::create).get(snapshots::list))
        .route(
            "/snapshots/{id}",
            get(snapshots::get)
                .patch(snapshots::update)
                .delete(snapshots::delete),
        )
        .route("/snapshots/{id}/verify", post(snapshots::verify))
        .route("/snapshots/{id}/dependents", get(snapshots::dependents))
        .route("/snapshots/rebuild", post(snapshots::rebuild))
        .route("/snapshots/resubmit", post(snapshots::resubmit))
        .route("/snapshots/settle", post(snapshots::settle))
        .route("/snapshots/audit", get(snapshots::audit))
        .route("/snapshots/export", get(snapshots::export))
        .route(
            "/snapshots/accept",
            post(snapshots::accept).get(snapshots::acceptances),
        )
        .route("/snapshots/accept/{id}", get(snapshots::acceptance))
        .route("/documents", post(documents::open))
        .route(
            "/documents/{id}",
            patch(documents::edit).delete(documents::close),
        )
        .route("/documents/{id}/goals", post(documents::goals))
        .route("/documents/{id}/probes", post(documents::probe))
        .route("/jobs/{id}", get(jobs::get).delete(jobs::cancel))
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

impl From<api::Error> for ApiError {
    fn from(e: api::Error) -> Self {
        ApiError(e)
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

pub async fn health(State(app): State<Arc<App>>) -> (StatusCode, Json<Health>) {
    let (status, body) = app.health().await;
    (status, Json(body))
}

impl App {
    /// The version a snapshot read is of: the one `requested` resolves to,
    /// through the aliases and the default, or, with neither, the one
    /// version the live workers serve.
    pub async fn read_version(&self, requested: Option<&str>) -> Result<String, ApiError> {
        if let Some(version) = self.versions.resolve(requested) {
            return Ok(version);
        }
        let live = self
            .dispatcher
            .queue()
            .live_versions(LIVE)
            .await
            .map_err(|e| ApiError::new(ErrorKind::Unavailable, e.to_string()))?;
        match <[String; 1]>::try_from(live) {
            Ok([version]) => Ok(version),
            Err(live) => {
                let served = match live.is_empty() {
                    true => "no live worker says which".to_string(),
                    false => format!("the live workers serve {}", live.join(", ")),
                };
                Err(ApiError::new(
                    ErrorKind::BadRequest,
                    format!("name a version: the server has no default, and {served}"),
                ))
            }
        }
    }

    /// `code` with its imports by run name (`import PisaRun.<name>`)
    /// resolved to the runs' heads, on the version `requested` reads as
    /// [`read_version`](Self::read_version) has it; `None` for code that
    /// imports no run by name, which needs no read.
    pub(crate) async fn resolve_runs(
        &self,
        requested: Option<&str>,
        code: &str,
    ) -> Result<Option<ResolvedRuns>, Error> {
        let header = split_header(code).header;
        let names =
            snapshot::run_imports(&header).map_err(|e| Error::new(ErrorKind::BadRequest, e))?;
        if names.is_empty() {
            return Ok(None);
        }
        let version = self.read_version(requested).await.map_err(|e| e.0)?;
        let resolved = self
            .snapshots
            .version(&version)
            .resolve_runs(code, &[])
            .await?;
        Ok(Some(ResolvedRuns {
            code: resolved.code,
            runs: resolved.runs,
            version,
        }))
    }

    /// The API's health: up, or 503 once draining, with the number of live
    /// workers on the queue, those without limits, and the versions whose
    /// sweeps are refused, each `null` if it can't be read.
    pub async fn health(&self) -> (StatusCode, Health) {
        let workers = self.dispatcher.workers().await;
        let (code, status) = if self.draining.load(Ordering::Acquire) {
            (StatusCode::SERVICE_UNAVAILABLE, HealthStatus::Draining)
        } else {
            (StatusCode::OK, HealthStatus::Ok)
        };
        let queue = self.dispatcher.queue();
        let unlimited_workers = match queue.unlimited_workers(LIVE).await {
            Ok(names) => Some(names),
            Err(e) => {
                tracing::warn!("reading the workers without limits failed: {e}");
                None
            }
        };
        let health = Health {
            status,
            workers,
            unlimited_workers,
            sweeps_refused: self.sweeps_refused().await,
            search: None,
        };
        (code, health)
    }

    /// The versions of the live workers whose sweeps are refused.
    async fn sweeps_refused(&self) -> Option<Vec<SweepRefused>> {
        let queue = self.dispatcher.queue();
        let read = async {
            let mut refused = Vec::new();
            for version in queue.live_versions(LIVE).await.map_err(|e| e.to_string())? {
                let catalog = Catalog::new(queue.db().clone(), version.clone());
                if let Some((reason, since)) =
                    catalog.sweep_refused().await.map_err(|e| e.message)?
                {
                    refused.push(SweepRefused {
                        version,
                        reason,
                        since: since / 1_000_000,
                    });
                }
            }
            Ok::<_, String>(refused)
        };
        match read.await {
            Ok(refused) => Some(refused),
            Err(e) => {
                tracing::warn!("reading the refused sweeps failed: {e}");
                None
            }
        }
    }
}

/// A request's code with its imports by run name resolved.
pub(crate) struct ResolvedRuns {
    /// The code, importing the runs' heads in their place.
    pub code: String,
    /// What each name resolved to.
    pub runs: Vec<RunImport>,
    /// The version the names were read on.
    pub version: String,
}

/// What a header's imports by run name resolve to, with the version read,
/// if it has any.
type Resolution = Result<Option<(Vec<RunImport>, String)>, Error>;

/// A check whose code can't be resolved: its result, with the failure
/// in-band, as other failures of the code's imports are.
fn unresolved(e: Error) -> CheckResult {
    CheckResult {
        error: Some(e),
        ..Default::default()
    }
}

#[utoipa::path(
    post, path = "/check", operation_id = "check", tag = "checks",
    summary = "Check Lean code",
    description = "Checks code against the version's Lean and imports. The verdict is `ok`; \
        what Lean, the policy or a limit reported is in-band, in `error`, `messages` and `reasons`.",
    params(PreferAsync),
    request_body = CheckRequest,
    responses((status = 200, description = "The verdict", body = CheckResult), Accepted, ApiErrors),
)]
async fn check(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<CheckRequest>,
) -> Result<Response, ApiError> {
    check_positions(&req.detail, &req.at)?;
    let (code, runs, version) = match app.resolve_runs(req.version.as_deref(), &req.code).await {
        Ok(None) => (
            req.code,
            Vec::new(),
            app.versions.resolve(req.version.as_deref()),
        ),
        Ok(Some(r)) => (r.code, r.runs, Some(r.version)),
        Err(e) => return Ok(Json(unresolved(e)).into_response()),
    };
    let p = prepare_check(code, req.timeout, req.detail, req.at, version, runs);
    Ok(app.answer(prefer, JobKind::Check, p).await)
}

#[utoipa::path(
    post, path = "/check/batch", operation_id = "checkBatch", tag = "checks",
    summary = "Check several pieces of code",
    description = "Each item gets a result, as from `/check`, with its own failure in `error`. \
        Up to 128 items, of which 32 are queued at a time, so that an item's wait for a \
        worker starts when it is queued, not when the batch arrives.",
    request_body = CheckBatchRequest,
    responses((status = 200, description = "A result per item, in order", body = CheckBatchResponse), ApiErrors),
)]
/// Every item gets a result; failures, including running out of capacity,
/// are reported in that item's `error`.
async fn check_batch(
    State(app): State<Arc<App>>,
    Body(req): Body<CheckBatchRequest>,
) -> Result<Json<CheckBatchResponse>, ApiError> {
    check_positions(&req.detail, &req.at)?;
    if req.items.len() > MAX_BATCH {
        let message = format!("`items` has {}; the most is {MAX_BATCH}", req.items.len());
        return Err(ApiError::new(ErrorKind::BadRequest, message));
    }
    let version = app.versions.resolve(req.version.as_deref());
    // Each header's imports by run name, resolved once for the batch.
    let mut resolved: HashMap<String, Resolution> = HashMap::new();
    for item in &req.items {
        if let Entry::Vacant(at) = resolved.entry(split_header(&item.code).header) {
            let r = app.resolve_runs(req.version.as_deref(), &item.code).await;
            at.insert(r.map(|r| r.map(|r| (r.runs, r.version))));
        }
    }
    let items = req.items.into_iter().map(|item| {
        let app = &app;
        let header = split_header(&item.code).header;
        let (code, version, runs) = match &resolved[&header] {
            Ok(None) => (item.code, version.clone(), Vec::new()),
            Ok(Some((runs, v))) => (
                pisa_snapshots::resolve_with(&item.code, runs),
                Some(v.clone()),
                runs.clone(),
            ),
            Err(e) => {
                let e = e.clone();
                return futures::future::Either::Left(async move { unresolved(e) });
            }
        };
        let (detail, at) = (req.detail.clone(), req.at.clone());
        futures::future::Either::Right(async move {
            let checked = check_one(app, code, req.timeout, detail, at, version, runs).await;
            match checked {
                Ok(r) => r,
                Err(e) => CheckResult {
                    error: Some(e.into()),
                    ..Default::default()
                },
            }
        })
    });
    let results = futures::stream::iter(items).buffered(BATCH_WINDOW);
    Ok(Json(CheckBatchResponse {
        results: results.collect().await,
    }))
}

/// The most items of a batch queued at once.
const BATCH_WINDOW: usize = 32;

/// A check asks about at most [`MAX_AT`] positions, and for terms only at
/// some.
fn check_positions(detail: &[Detail], at: &[Pos]) -> Result<(), ApiError> {
    let wrong = if at.len() > MAX_AT {
        format!("`at` has {} positions; the most is {MAX_AT}", at.len())
    } else if at.is_empty() && detail.contains(&Detail::Terms) {
        "`terms` needs positions in `at`".to_string()
    } else {
        return Ok(());
    };
    Err(ApiError::new(ErrorKind::BadRequest, wrong))
}

async fn check_one(
    app: &App,
    code: String,
    timeout_secs: u64,
    detail: Vec<Detail>,
    at: Vec<Pos>,
    version: Option<String>,
    runs: Vec<RunImport>,
) -> Result<CheckResult, DispatchError> {
    let p = prepare_check(code, timeout_secs, detail, at, version, runs);
    Ok(check_result(app.dispatcher.submit(p).await?))
}

fn prepare_check(
    code: String,
    timeout_secs: u64,
    detail: Vec<Detail>,
    at: Vec<Pos>,
    version: Option<String>,
    runs: Vec<RunImport>,
) -> Prepared {
    let split = split_header(&code);
    let route = Route {
        version,
        header_key: Some(split.header_key),
        header: prewarm_header(split.header),
        document: None,
    };
    let job = Job::Check(CheckJob {
        code,
        timeout_secs,
        detail,
        at,
        runs,
    });
    Prepared {
        job,
        route,
        priority: Priority::Batch,
    }
}

/// A check's result: failures are in-band.
pub(crate) fn check_result(done: JobResult) -> CheckResult {
    match done {
        JobResult::Check(o) => CheckResult::from(o),
        other => failed_result(other),
    }
}

/// The header new workers may pre-warm, when it is in demand. Headers that import
/// snapshots are left out: there is a new one for every statement, and a new
/// worker would mostly pre-warm abandoned ones.
pub fn prewarm_header(header: String) -> Option<String> {
    snapshot::imports(&header)
        .is_ok_and(|ids| ids.is_empty())
        .then_some(header)
}

/// A worker answered with something other than a check outcome.
pub fn failed_result(result: JobResult) -> CheckResult {
    let message = match result {
        JobResult::Failed { error } => error,
        other => format!("unexpected worker result: {other:?}"),
    };
    CheckResult {
        error: Some(api::Error::new(ErrorKind::Internal, message)),
        ..Default::default()
    }
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
