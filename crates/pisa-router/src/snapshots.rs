//! Snapshot endpoints. Creating a snapshot is a check whose code later code
//! imports, so it is routed like a check of that later code: its worker then
//! has the snapshot warm. Reads and deletes go to any worker of the version.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use pisa_protocol::api::{Deleted, SnapshotInfo, SnapshotRequest, SnapshotResponse, VersionQuery};
use pisa_protocol::header::{header_key, split_header};
use pisa_protocol::snapshot;
use pisa_protocol::wire::{Job, JobResult, SnapshotCreateJob, SnapshotJob};

use crate::dispatch::{Priority, Route};
use crate::http::{ApiError, App, Body, record_result};
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
    Path(name): Path<String>,
    Query(q): Query<VersionQuery>,
) -> Result<Json<SnapshotInfo>, ApiError> {
    let job = Job::SnapshotGet(SnapshotJob { snapshot: name });
    match submit(&app, job, q).await? {
        JobResult::Snapshot(r) => Ok(Json(*r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

pub async fn delete(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    Query(q): Query<VersionQuery>,
) -> Result<Json<Deleted>, ApiError> {
    let job = Job::SnapshotDelete(SnapshotJob { snapshot: name });
    match submit(&app, job, q).await? {
        JobResult::SnapshotDeleted(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

async fn submit(app: &App, job: Job, q: VersionQuery) -> Result<JobResult, ApiError> {
    let route = Route {
        version: app.versions.resolve(q.version.as_deref()),
        ..Default::default()
    };
    let done = app
        .dispatcher
        .submit(job, route, Priority::Interactive)
        .await?;
    Ok(done.result)
}
