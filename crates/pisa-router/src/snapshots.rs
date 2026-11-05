//! Snapshot endpoints. Creating a snapshot is a check whose code later code
//! imports, so it is routed like a check of that later code: its worker then
//! has the snapshot warm. Reads, listings, label edits, deletes and verifies
//! go to any worker of the version.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, RawQuery, State};
use axum::response::{IntoResponse, Response};
use pisa_protocol::api::{
    Accepted, ApiErrors, Deleted, ErrorKind, JobKind, JobState, LabelsRequest, PreferAsync,
    RebuildRequest, RebuildResponse, SnapshotAudit, SnapshotDependents, SnapshotInfo, SnapshotList,
    SnapshotRequest, SnapshotResponse, SnapshotSummary, SnapshotVerified, VersionQuery,
};
use pisa_protocol::header::{header_key, split_header};
use pisa_protocol::labels::{DEFAULT_LIMIT, MAX_LABELS, MAX_LIMIT, is_cursor, parse_filter};
use pisa_protocol::snapshot;
use pisa_protocol::wire::{
    Job, JobResult, SnapshotAuditJob, SnapshotCreateJob, SnapshotDependentsJob, SnapshotJob,
    SnapshotLabelsJob, SnapshotListJob, SnapshotRebuildJob,
};

use crate::dispatch::{DispatchError, Prepared, Priority, Route};
use crate::http::{ApiError, App, Body};
use crate::jobs::{Prefer, accepted, respond};
use crate::rebuild;
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
    let p = prepare_create(&app, req);
    app.answer(prefer, JobKind::CreateSnapshot, p).await
}

/// A create's job, routed to the worker that will serve code on top of the
/// snapshot, which builds it.
pub(crate) fn prepare_create(app: &App, req: SnapshotRequest) -> Prepared {
    let split = split_header(&req.code);
    let next = snapshot::next_header(&split.header, &snapshot::id(&req.code));
    let route = Route {
        version: app.versions.resolve(req.version.as_deref()),
        header_key: Some(header_key(&next)),
        header: None,
        state: None,
    };
    let replace = req.replaces.is_some();
    let create = SnapshotCreateJob {
        code: req.code,
        timeout_secs: req.timeout,
        verify: req.verify,
        labels: req.labels,
        key: req.key,
        replaces: req.replaces,
    };
    let job = if replace {
        Job::SnapshotReplace(create)
    } else {
        Job::SnapshotCreate(create)
    };
    Prepared {
        job,
        route,
        priority: Priority::Batch,
    }
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

/// The version and the `label` filters of a query, at most [`MAX_LABELS`]
/// of them. Other parameters are ignored.
fn label_query(query: &str) -> Result<(VersionQuery, Vec<(String, String)>), String> {
    let mut q = VersionQuery::default();
    let mut labels = Vec::new();
    for (name, value) in form_urlencoded::parse(query.as_bytes()) {
        match &*name {
            "version" => q.version = Some(value.into_owned()),
            "label" => labels.push(parse_filter(&value)?),
            _ => {}
        }
    }
    if labels.len() > MAX_LABELS {
        return Err(format!(
            "{} label filters; a listing takes at most {MAX_LABELS}",
            labels.len()
        ));
    }
    Ok((q, labels))
}

/// Splits the query of `GET /snapshots` into the version and the job, and
/// checks the job's filters (see [`label_query`]), limit and cursor. Other
/// parameters are ignored.
fn list_query(query: &str) -> Result<(VersionQuery, SnapshotListJob), String> {
    let (q, labels) = label_query(query)?;
    let mut job = SnapshotListJob {
        labels,
        limit: DEFAULT_LIMIT,
        after: None,
    };
    for (name, value) in form_urlencoded::parse(query.as_bytes()) {
        match &*name {
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
    Ok((q, job))
}

#[utoipa::path(
    get, path = "/snapshots/audit", operation_id = "auditSnapshots", tag = "snapshots",
    summary = "What a project rests on",
    description = "For the entries with every label: their declarations by status, each \
        assumption (an axiom declared in snapshot code) with what uses it, wherever it is \
        declared, and the declarations that are `sorry` or `rejected`. It reads records only.",
    params(
        ("label" = Option<Vec<String>>, Query, description = "`key:value`, split on the first `:`; repeatable"),
        VersionQuery,
    ),
    responses((status = 200, description = "The audit", body = SnapshotAudit), ApiErrors),
)]
/// `GET /snapshots/audit`.
pub async fn audit(
    State(app): State<Arc<App>>,
    RawQuery(query): RawQuery,
) -> Result<Json<SnapshotAudit>, ApiError> {
    let (q, labels) = label_query(query.as_deref().unwrap_or_default())
        .map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    let job = Job::SnapshotAudit(SnapshotAuditJob { labels });
    match submit(&app, job, q, Priority::Interactive).await? {
        JobResult::SnapshotAudit(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    get, path = "/snapshots/{entry}/dependents", operation_id = "listDependents", tag = "snapshots",
    summary = "The entries above an entry",
    description = "The entries on every module that imports the entry's module, directly or \
        through others, parents first: by level, then oldest first. With `label` filters, \
        only the entries with every label.",
    params(
        ("entry" = String, Path, description = "The entry ID, 32 hex digits"),
        ("label" = Option<Vec<String>>, Query, description = "`key:value`, split on the first `:`; repeatable"),
        VersionQuery,
    ),
    responses((status = 200, description = "The entries above it", body = SnapshotDependents), ApiErrors),
)]
/// `GET /snapshots/{entry}/dependents`.
pub async fn dependents(
    State(app): State<Arc<App>>,
    Path(entry): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Json<SnapshotDependents>, ApiError> {
    let (q, labels) = label_query(query.as_deref().unwrap_or_default())
        .map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    let job = Job::SnapshotDependents(SnapshotDependentsJob { entry, labels });
    match submit(&app, job, q, Priority::Interactive).await? {
        JobResult::SnapshotDependents(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    post, path = "/snapshots/rebuild", operation_id = "rebuildSnapshots", tag = "snapshots",
    summary = "Rebuild an entry's dependents on its replacement",
    description = "Plans the creates that make each dependent of `from` with an entry that has \
        the labels again on `to`, by level, and unless `dry_run` runs them: a level at a time, a \
        failed chunk skipping only what builds on it, then with `retire` deleting the old \
        entries and `from`. Unless `to` replaces `from`, `to` must not change what `from` \
        states, or `allow_changes` be set. Sending the same rebuild again replays the creates \
        that are done. A run can be async; a dry run answers at once.",
    params(PreferAsync),
    request_body = RebuildRequest,
    responses((status = 200, description = "The plan, and what the run did", body = RebuildResponse), Accepted, ApiErrors),
)]
/// `POST /snapshots/rebuild`.
pub async fn rebuild(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<RebuildRequest>,
) -> Response {
    if req.labels.is_empty() {
        let e = ApiError::new(
            ErrorKind::BadRequest,
            "a rebuild takes labels: which dependents to rebuild",
        );
        return e.into_response();
    }
    if req.dry_run {
        let job = Job::SnapshotRebuild(SnapshotRebuildJob {
            from: req.from,
            to: req.to,
            labels: req.labels,
            allow_changes: req.allow_changes,
        });
        let q = VersionQuery {
            version: req.version,
        };
        return respond(JobKind::RebuildSnapshots, submit_job(&app, job, q).await);
    }
    if !prefer.respond_async {
        return match rebuild::run(&app, &req).await {
            Ok(r) => Json(r).into_response(),
            Err(e) => e.into_response(),
        };
    }
    let record = Job::SnapshotRebuild(SnapshotRebuildJob {
        from: req.from.clone(),
        to: req.to.clone(),
        labels: req.labels.clone(),
        allow_changes: req.allow_changes,
    });
    let held = match app
        .dispatcher
        .hold(JobKind::RebuildSnapshots, &record)
        .await
    {
        Ok(held) => held,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let job = held.token.clone();
    tokio::spawn(async move {
        let outcome = rebuild::run(&app, &req).await.map_err(|e| e.0);
        if let Err(e) = held.finish(&JobResult::SnapshotRebuild(outcome)).await {
            tracing::warn!("storing a rebuild's answer failed: {e}");
        }
    });
    accepted(job, JobKind::RebuildSnapshots, JobState::Running)
}

pub(crate) fn rebuilt(done: JobResult) -> Result<RebuildResponse, ApiError> {
    match done {
        JobResult::SnapshotRebuild(r) => r.map_err(ApiError),
        other => Err(unexpected(other)),
    }
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

/// Runs a catalog job on a worker of the version, as an interactive one.
async fn submit_job(app: &App, job: Job, q: VersionQuery) -> Result<JobResult, DispatchError> {
    app.dispatcher
        .submit(prepare(app, job, q, Priority::Interactive))
        .await
}

pub(crate) async fn submit(
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
