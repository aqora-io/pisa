//! Snapshot endpoints. Creating a snapshot is a check whose code later code
//! imports, so it is routed like a check of that later code: its worker then
//! has the snapshot warm. Reads, listings, label edits, deletes and verifies
//! go to any worker of the version.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, RawQuery, State};
use pisa_protocol::api::{
    Deleted, ErrorKind, LabelsRequest, SnapshotInfo, SnapshotList, SnapshotRequest,
    SnapshotResponse, SnapshotSummary, SnapshotVerified, VersionQuery,
};
use pisa_protocol::header::{header_key, split_header};
use pisa_protocol::labels::{DEFAULT_LIMIT, MAX_LABELS, MAX_LIMIT, is_cursor, parse_filter};
use pisa_protocol::snapshot;
use pisa_protocol::wire::{
    Job, JobResult, SnapshotCreateJob, SnapshotJob, SnapshotLabelsJob, SnapshotListJob,
};

use crate::dispatch::{Priority, Route};
use crate::http::{ApiError, App, Body, record_result};
use crate::metrics as m;
use crate::sessions::unexpected;

pub async fn create(
    State(app): State<Arc<App>>,
    Body(req): Body<SnapshotRequest>,
) -> Result<Json<SnapshotResponse>, ApiError> {
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
    let done = app.dispatcher.submit(job, route, Priority::Batch).await?;
    let JobResult::SnapshotCreated(r) = done.result else {
        return Err(unexpected(done.result));
    };
    record_result(&r.result, "snapshot");
    Ok(Json(*r))
}

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

pub async fn verify(
    State(app): State<Arc<App>>,
    Path(entry): Path<String>,
    Query(q): Query<VersionQuery>,
) -> Result<Json<SnapshotVerified>, ApiError> {
    let job = Job::SnapshotVerify(SnapshotJob { entry });
    match submit(&app, job, q, Priority::Batch).await? {
        JobResult::SnapshotVerified(r) => {
            let r = r.map_err(ApiError)?;
            let outcome = if r.failure.is_some() { "error" } else { "ok" };
            metrics::counter!(m::JOBS, "kind" => "snapshot_verify", "outcome" => outcome)
                .increment(1);
            Ok(Json(r))
        }
        other => Err(unexpected(other)),
    }
}

async fn submit(
    app: &App,
    job: Job,
    q: VersionQuery,
    priority: Priority,
) -> Result<JobResult, ApiError> {
    let route = Route {
        version: app.versions.resolve(q.version.as_deref()),
        ..Default::default()
    };
    let done = app.dispatcher.submit(job, route, priority).await?;
    Ok(done.result)
}
