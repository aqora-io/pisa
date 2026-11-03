//! Snapshot endpoints. Creating a snapshot is a check whose code later code
//! imports, so it is routed like a check of that later code: its worker then
//! has the snapshot warm. Reads, deletes and verifies go to any worker of the
//! version.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use pisa_protocol::api::{
    Deleted, SnapshotInfo, SnapshotRequest, SnapshotResponse, SnapshotVerified, VersionQuery,
};
use pisa_protocol::header::{header_key, split_header};
use pisa_protocol::snapshot;
use pisa_protocol::wire::{Job, JobResult, SnapshotCreateJob, SnapshotJob};

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
        affinity: None,
    };
    let job = Job::SnapshotCreate(SnapshotCreateJob {
        code: req.code,
        timeout_secs: req.timeout,
        verify: req.verify,
        labels: req.labels,
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
