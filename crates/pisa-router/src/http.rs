//! Client-facing HTTP API; the types and conventions are in
//! `pisa_protocol::api`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
    Dependencies, DependencyState, Detail, Error, ErrorKind, ErrorResponse, Health, HealthStatus,
    JobKind, MAX_AT, MAX_BATCH, Pos, PreferAsync, RunImport, SweepRefused,
};
use pisa_protocol::header::split_header;
use pisa_protocol::scope::Scope;
use pisa_protocol::snapshot;
use pisa_protocol::version::Versions;
use pisa_protocol::wire::{CheckJob, Job, JobResult};
use pisa_snapshots::Readers;
use serde::de::DeserializeOwned;

use crate::dispatch::{DispatchError, Dispatcher, Prepared, Priority, Route};
use crate::jobs::{self, Prefer};
use crate::{documents, environments, runs, sessions, snapshots};

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
        runs::get,
        runs::put,
        documents::open,
        documents::edit,
        documents::goals,
        documents::probe,
        documents::close,
        jobs::get,
        jobs::cancel,
        environments::list,
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
        .route("/runs/{name}", get(runs::get).put(runs::put))
        .route("/documents", post(documents::open))
        .route(
            "/documents/{id}",
            patch(documents::edit).delete(documents::close),
        )
        .route("/documents/{id}/goals", post(documents::goals))
        .route("/documents/{id}/probes", post(documents::probe))
        .route("/jobs/{id}", get(jobs::get).delete(jobs::cancel))
        .route("/environments", get(environments::list))
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

impl From<DispatchError> for api::Error {
    fn from(e: DispatchError) -> Self {
        let (kind, resend_safe) = match &e {
            DispatchError::NoCapacity => (ErrorKind::Capacity, None),
            DispatchError::Unavailable(_) => (ErrorKind::Unavailable, None),
            DispatchError::WorkerLost { resend_safe, .. } => {
                (ErrorKind::WorkerLost, Some(*resend_safe))
            }
            DispatchError::ServerStopped => (ErrorKind::WorkerLost, Some(true)),
        };
        api::Error {
            resend_safe,
            ..api::Error::new(kind, e.to_string())
        }
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
    /// [`read_version`](Self::read_version) has it, once its imports are
    /// checked as [`guard_imports`](Self::guard_imports) does; `None` for
    /// code that imports no run by name.
    pub(crate) async fn resolve_runs(
        &self,
        requested: Option<&str>,
        code: &str,
    ) -> Result<Option<ResolvedRuns>, Error> {
        self.resolve_runs_with(requested, code, &[]).await
    }

    /// As [`resolve_runs`](Self::resolve_runs), with the names `pins` has
    /// resolving as it says, as a document's do, each with where its run has
    /// moved since (`moved_to`). What the names bring in must load with the
    /// rest of the imports, as a create checks.
    pub(crate) async fn resolve_runs_with(
        &self,
        requested: Option<&str>,
        code: &str,
        pins: &[RunImport],
    ) -> Result<Option<ResolvedRuns>, Error> {
        let header = split_header(code).header;
        self.guard_imports(requested, &header).await?;
        let names =
            snapshot::run_imports(&header).map_err(|e| Error::new(ErrorKind::BadRequest, e))?;
        if names.is_empty() {
            return Ok(None);
        }
        let version = self.read_version(requested).await.map_err(|e| e.0)?;
        let reader = self.snapshots.version(&version);
        let resolved = reader.resolve_runs(code, pins).await?;
        if !pins.is_empty() {
            reader.pins_kept(&resolved.runs).await?;
        }
        let imports = snapshot::imports(&split_header(&resolved.code).header).unwrap_or_default();
        if let Some(e) = reader.clashes(&imports).await? {
            let named: Vec<String> = (resolved.runs.iter())
                .map(|r| format!("{}.{} is {}", snapshot::RUNS, r.name, r.module))
                .collect();
            let message = format!(
                "{} (imported by name: {}; import a run by its name only, without another \
                 version of it)",
                e.message,
                named.join(", ")
            );
            return Err(Error::new(e.kind, message));
        }
        let runs = match pins.is_empty() {
            true => resolved.runs,
            false => reader.moved(resolved.runs).await,
        };
        Ok(Some(ResolvedRuns {
            code: resolved.code,
            runs,
            version,
        }))
    }

    /// Refuses what normalised `header` can't import: an open document's
    /// module, and, within a scope, a snapshot module no entry in the scope
    /// is on, which is `not_found` as for one that doesn't exist.
    pub(crate) async fn guard_imports(
        &self,
        requested: Option<&str>,
        header: &str,
    ) -> Result<(), Error> {
        if let Some(m) = snapshot::document_import(header) {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("{m} is an open document's module, which code can't import"),
            ));
        }
        let ids = snapshot::imports(header).unwrap_or_default();
        if ids.is_empty() || pisa_db::scope::current().is_empty() {
            return Ok(());
        }
        let version = self.read_version(requested).await.map_err(|e| e.0)?;
        match self.snapshots.version(&version).unreachable(&ids).await? {
            Some(id) => Err(pisa_db::catalog::unknown_module(&id)),
            None => Ok(()),
        }
    }

    /// The API's health: up, or 503 once draining, with the number of live
    /// workers on the queue, those without limits, the versions whose
    /// sweeps are refused, the commits the workers were built from and the
    /// memory limits of the workers per host, each
    /// `null` if it can't be read, the commit of the server, and whether the
    /// database and the module store are reachable. Each read of the
    /// database gets [`HEALTH_READ`] to answer, so a stalled database delays
    /// the answer by that long, not for good.
    pub async fn health(&self) -> (StatusCode, Health) {
        let (code, status) = if self.draining.load(Ordering::Acquire) {
            (StatusCode::SERVICE_UNAVAILABLE, HealthStatus::Draining)
        } else {
            (StatusCode::OK, HealthStatus::Ok)
        };
        let queue = self.dispatcher.queue();
        let (counts, unlimited_workers, worker_revisions, pools, sweeps_refused, store, hosts) = tokio::join!(
            bounded("the queue's state", self.dispatcher.workers()),
            read("the workers without limits", queue.unlimited_workers(LIVE)),
            read("the workers' revisions", queue.live_revisions(LIVE)),
            read("the pools", queue.pools(LIVE)),
            bounded("the refused sweeps", self.sweeps_refused()),
            read("the store's state", queue.store_state(LIVE)),
            read("the hosts' memory", queue.hosts(LIVE)),
        );
        let database = if counts.is_some() {
            DependencyState::Ok
        } else {
            DependencyState::Unreachable
        };
        let health = Health {
            status,
            workers: counts.map(|(workers, _)| workers),
            queued: counts.map(|(_, queued)| queued),
            unlimited_workers,
            sweeps_refused,
            revision: pisa_protocol::revision::REVISION.to_string(),
            worker_revisions,
            pools,
            dependencies: Dependencies {
                database,
                store: store.unwrap_or_default(),
            },
            hosts,
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

/// How long `/health` waits for each read of the database.
const HEALTH_READ: Duration = Duration::from_secs(2);

/// What `read` gives, or `None` if it takes longer than [`HEALTH_READ`].
async fn bounded<T>(what: &str, read: impl Future<Output = Option<T>>) -> Option<T> {
    match tokio::time::timeout(HEALTH_READ, read).await {
        Ok(read) => read,
        Err(_) => {
            tracing::warn!("reading {what} took more than {HEALTH_READ:?}");
            None
        }
    }
}

/// As [`bounded`], for a read that may fail, which it logs.
async fn read<T, E: std::fmt::Display>(
    what: &str,
    read: impl Future<Output = Result<T, E>>,
) -> Option<T> {
    bounded(what, async {
        read.await
            .map_err(|e| tracing::warn!("reading {what} failed: {e}"))
            .ok()
    })
    .await
}

/// Runs a request within the scope its `Pisa-Scope` header gives (see
/// [`Scope`]): none without the header, and a 400 for a header that isn't
/// one.
pub(crate) async fn scoped(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let header = req.headers().get(pisa_protocol::scope::HEADER);
    let parsed = header.map(|h| match h.to_str() {
        Ok(text) => Scope::parse(text),
        Err(_) => Err("Pisa-Scope must be text".to_string()),
    });
    let scope = match parsed {
        None => Scope::default(),
        Some(Ok(scope)) => scope,
        Some(Err(e)) => return ApiError::new(ErrorKind::BadRequest, e).into_response(),
    };
    pisa_db::scope::within(scope, next.run(req)).await
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
    // A check against a snapshot runs on its version, unless it names one.
    let requested = match &req.against {
        Some(id) => Some(against(&app, id, req.version.as_deref()).await?),
        None => req.version.clone(),
    };
    let (code, runs, version) = match app.resolve_runs(requested.as_deref(), &req.code).await {
        Ok(None) => (
            req.code,
            Vec::new(),
            app.versions.resolve(requested.as_deref()),
        ),
        Ok(Some(r)) => (req.code, r.runs, Some(r.version)),
        Err(e) => return Ok(Json(unresolved(e)).into_response()),
    };
    let mut p = prepare_check(code, req.timeout, req.detail, req.at, version, runs);
    if let Job::Check(job) = &mut p.job {
        job.against = req.against;
    }
    Ok(app.answer(prefer, JobKind::Check, p).await)
}

/// The version a check against snapshot `id` runs on, which must be the
/// requested one if it names one: the snapshot's, which must exist.
async fn against(app: &App, id: &str, requested: Option<&str>) -> Result<String, ApiError> {
    let parsed = snapshots::parse_id(id)?;
    if let Some(v) = requested.and_then(|v| app.versions.resolve(Some(v)))
        && v != parsed.version
    {
        let message = format!(
            "`against` is a snapshot of version {}; the check is for version {v}",
            parsed.version
        );
        return Err(ApiError::new(ErrorKind::BadRequest, message));
    }
    let reader = app.snapshots.version(&parsed.version);
    if reader.catalog().find_entry(&parsed.entry).await?.is_none() {
        return Err(ApiError::new(
            ErrorKind::NotFound,
            format!("no snapshot {id}"),
        ));
    }
    Ok(parsed.version)
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
            Ok(Some((runs, v))) => (item.code, Some(v.clone()), runs.clone()),
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

/// A check of `code`, as sent: it runs with its imports by run name
/// resolved as `runs` says.
fn prepare_check(
    code: String,
    timeout_secs: u64,
    detail: Vec<Detail>,
    at: Vec<Pos>,
    version: Option<String>,
    runs: Vec<RunImport>,
) -> Prepared {
    let (code, source) = match runs.is_empty() {
        true => (code, None),
        false => (pisa_snapshots::resolve_with(&code, &runs), Some(code)),
    };
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
        source,
        against: None,
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
    fn a_lost_job_says_whether_to_send_it_again() {
        let error = |e: DispatchError| {
            let e = api::Error::from(e);
            (e.kind, e.resend_safe, e.message)
        };
        assert_eq!(
            error(DispatchError::WorkerLost {
                attempts: 1,
                attempts_exhausted: false,
                resend_safe: false,
            }),
            (
                ErrorKind::WorkerLost,
                Some(false),
                "no worker that can run the job came back in time (it was claimed 1 time)".into()
            )
        );
        assert_eq!(
            error(DispatchError::WorkerLost {
                attempts: 2,
                attempts_exhausted: true,
                resend_safe: true,
            }),
            (
                ErrorKind::WorkerLost,
                Some(true),
                "job failed on 2 workers that disconnected".into()
            )
        );
        // A rebuild's or a resubmit's steps carry keys.
        let (kind, resend_safe, _) = error(DispatchError::ServerStopped);
        assert_eq!((kind, resend_safe), (ErrorKind::WorkerLost, Some(true)));
        // Nothing ran, or the answer is the queue's: it doesn't say.
        let (kind, resend_safe, _) = error(DispatchError::NoCapacity);
        assert_eq!((kind, resend_safe), (ErrorKind::Capacity, None));
        let (kind, resend_safe, _) = error(DispatchError::Unavailable("down".into()));
        assert_eq!((kind, resend_safe), (ErrorKind::Unavailable, None));
    }

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
