//! Snapshot endpoints. Creating a snapshot is a check whose code later code
//! imports, so it is routed like a check of that later code: its worker then
//! has the snapshot warm. Reads, listings, label edits, deletes and verifies
//! go to any worker of the version.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, RawQuery, State};
use axum::response::Response;
use pisa_protocol::api::{
    Accepted, ApiErrors, Deleted, ErrorKind, JobKind, LabelsRequest, PreferAsync, SnapshotInfo,
    SnapshotList, SnapshotRequest, SnapshotResponse, SnapshotSummary, SnapshotVerified,
    VersionQuery,
};
use pisa_protocol::header::{header_key, split_header};
use pisa_protocol::labels::{DEFAULT_LIMIT, MAX_LABELS, MAX_LIMIT, is_cursor, parse_filter};
use pisa_protocol::snapshot;
use pisa_protocol::wire::{
    Job, JobResult, SnapshotCreateJob, SnapshotJob, SnapshotLabelsJob, SnapshotListJob,
};

use crate::dispatch::{Prepared, Priority, Route};
use crate::http::{ApiError, App, Body};
use crate::jobs::Prefer;
use crate::sessions::unexpected;

#[utoipa::path(
    post, path = "/snapshots", operation_id = "createSnapshot", tag = "snapshots",
    summary = "Keep code as a snapshot",
    description = "Checks code like `/check` and, if Lean accepts it, keeps it as a module that \
        later code can import, with a new entry on it. Code that is already kept gets a new \
        entry on its module.",
    params(PreferAsync),
    request_body = SnapshotRequest,
    responses((status = 200, description = "The entry, or why nothing was kept", body = SnapshotResponse), Accepted, ApiErrors),
)]
pub async fn create(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<SnapshotRequest>,
) -> Response {
    let split = split_header(&req.code);
    let next = snapshot::next_header(&split.header, &snapshot::id(&req.code));
    let route = Route {
        version: app.versions.resolve(req.version.as_deref()),
        header_key: Some(header_key(&next)),
        header: None,
        state: None,
    };
    let job = Job::SnapshotCreate(SnapshotCreateJob {
        code: req.code,
        timeout_secs: req.timeout,
        verify: req.verify,
        labels: req.labels,
        key: req.key,
    });
    let p = Prepared {
        job,
        route,
        priority: Priority::Batch,
    };
    app.answer(prefer, JobKind::CreateSnapshot, p).await
}

pub(crate) fn created(done: JobResult) -> Result<SnapshotResponse, ApiError> {
    let JobResult::SnapshotCreated(r) = done else {
        return Err(unexpected(done));
    };
    Ok(*r)
}

#[utoipa::path(
    get, path = "/snapshots/{entry}", operation_id = "getSnapshot", tag = "snapshots",
    summary = "An entry and its module",
    params(("entry" = String, Path, description = "The entry ID, 32 hex digits"), VersionQuery),
    responses((status = 200, description = "The entry and its module", body = SnapshotInfo), ApiErrors),
)]
pub async fn get(
    State(app): State<Arc<App>>,
    Path(entry): Path<String>,
    Query(q): Query<VersionQuery>,
) -> Result<Json<SnapshotInfo>, ApiError> {
    let job = Job::SnapshotGet(SnapshotJob { entry });
    match submit(&app, job, q, Priority::Interactive).await? {
        JobResult::Snapshot(r) => Ok(Json(*r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    get, path = "/snapshots", operation_id = "listSnapshots", tag = "snapshots",
    summary = "List entries",
    description = "Newest first, with every label given. A full page has `next`: pass it as \
        `after` for the next one.",
    params(
        ("label" = Option<Vec<String>>, Query, description = "`key:value`, split on the first `:`; repeatable"),
        ("limit" = Option<usize>, Query, description = "1–1000, by default 100"),
        ("after" = Option<String>, Query, description = "The previous page's `next`"),
        VersionQuery,
    ),
    responses((status = 200, description = "A page of entries", body = SnapshotList), ApiErrors),
)]
/// `GET /snapshots`. The query is parsed here rather than by
/// [`Query`], which can't collect the repeated `label`.
pub async fn list(
    State(app): State<Arc<App>>,
    RawQuery(query): RawQuery,
) -> Result<Json<SnapshotList>, ApiError> {
    let (q, job) = list_query(query.as_deref().unwrap_or_default())
        .map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    match submit(&app, Job::SnapshotList(job), q, Priority::Interactive).await? {
        JobResult::SnapshotList(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

/// Splits the query of `GET /snapshots` into the version and the job, and
/// checks the job's filters, at most [`MAX_LABELS`] of them, limit and
/// cursor. Other parameters are ignored.
fn list_query(query: &str) -> Result<(VersionQuery, SnapshotListJob), String> {
    let mut q = VersionQuery::default();
    let mut job = SnapshotListJob {
        labels: Vec::new(),
        limit: DEFAULT_LIMIT,
        after: None,
    };
    for (name, value) in form_urlencoded::parse(query.as_bytes()) {
        match &*name {
            "version" => q.version = Some(value.into_owned()),
            "label" => job.labels.push(parse_filter(&value)?),
            "limit" => {
                job.limit = value
                    .parse()
                    .ok()
                    .filter(|n| (1..=MAX_LIMIT).contains(n))
                    .ok_or_else(|| format!("limit {value:?} must be from 1 to {MAX_LIMIT}"))?
            }
            "after" if is_cursor(&value) => job.after = Some(value.into_owned()),
            "after" => {
                return Err(format!(
                    "after {value:?} is not a cursor: pass the `next` of the page before"
                ));
            }
            _ => {}
        }
    }
    if job.labels.len() > MAX_LABELS {
        return Err(format!(
            "{} label filters; a listing takes at most {MAX_LABELS}",
            job.labels.len()
        ));
    }
    Ok((q, job))
}

#[utoipa::path(
    patch, path = "/snapshots/{entry}", operation_id = "setSnapshotLabels", tag = "snapshots",
    summary = "Set or remove an entry's labels",
    params(("entry" = String, Path, description = "The entry ID, 32 hex digits"), VersionQuery),
    request_body = LabelsRequest,
    responses((status = 200, description = "The entry, with its labels", body = SnapshotSummary), ApiErrors),
)]
/// `PATCH /snapshots/{entry}`.
pub async fn labels(
    State(app): State<Arc<App>>,
    Path(entry): Path<String>,
    Query(q): Query<VersionQuery>,
    Body(req): Body<LabelsRequest>,
) -> Result<Json<SnapshotSummary>, ApiError> {
    let job = Job::SnapshotLabels(SnapshotLabelsJob {
        entry,
        set: req.set,
        remove: req.remove,
    });
    match submit(&app, job, q, Priority::Interactive).await? {
        JobResult::SnapshotLabels(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    delete, path = "/snapshots/{entry}", operation_id = "deleteSnapshot", tag = "snapshots",
    summary = "Delete an entry",
    description = "Its module goes too once no entry or other module refers to it: `removed` \
        lists the modules that went.",
    params(("entry" = String, Path, description = "The entry ID, 32 hex digits"), VersionQuery),
    responses((status = 200, description = "What went", body = Deleted), ApiErrors),
)]
pub async fn delete(
    State(app): State<Arc<App>>,
    Path(entry): Path<String>,
    Query(q): Query<VersionQuery>,
) -> Result<Json<Deleted>, ApiError> {
    let job = Job::SnapshotDelete(SnapshotJob { entry });
    match submit(&app, job, q, Priority::Interactive).await? {
        JobResult::SnapshotDeleted(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    post, path = "/snapshots/{entry}/verify", operation_id = "verifySnapshot", tag = "snapshots",
    summary = "Re-check an entry's module with the kernel",
    description = "Re-checks the module and the modules it imports; `failure` says which one \
        failed and why.",
    params(("entry" = String, Path, description = "The entry ID, 32 hex digits"), VersionQuery, PreferAsync),
    responses((status = 200, description = "Whether the module passed", body = SnapshotVerified), Accepted, ApiErrors),
)]
pub async fn verify(
    State(app): State<Arc<App>>,
    Path(entry): Path<String>,
    Query(q): Query<VersionQuery>,
    prefer: Prefer,
) -> Response {
    let job = Job::SnapshotVerify(SnapshotJob { entry });
    let p = prepare(&app, job, q, Priority::Batch);
    app.answer(prefer, JobKind::VerifySnapshot, p).await
}

pub(crate) fn verified(done: JobResult) -> Result<SnapshotVerified, ApiError> {
    match done {
        JobResult::SnapshotVerified(r) => r.map_err(ApiError),
        other => Err(unexpected(other)),
    }
}

fn prepare(app: &App, job: Job, q: VersionQuery, priority: Priority) -> Prepared {
    let route = Route {
        version: app.versions.resolve(q.version.as_deref()),
        ..Default::default()
    };
    Prepared {
        job,
        route,
        priority,
    }
}

async fn submit(
    app: &App,
    job: Job,
    q: VersionQuery,
    priority: Priority,
) -> Result<JobResult, ApiError> {
    Ok(app
        .dispatcher
        .submit(prepare(app, job, q, priority))
        .await?)
}
