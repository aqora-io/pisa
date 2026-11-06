//! Snapshot endpoints. Creating a snapshot is a check whose code later code
//! imports, so it is routed like a check of that later code: its worker then
//! has the snapshot warm. Reads, listings, updates, deletes and verifies go
//! to any worker of the version, which a snapshot id names.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, RawQuery, State};
use axum::response::{IntoResponse, Response};
use pisa_protocol::api::{
    Accepted, ApiErrors, AuditSnapshotsResponse, CreateSnapshotRequest, CreateSnapshotResponse,
    DeleteSnapshotResponse, ErrorKind, JobKind, JobStatus, ListDependentsResponse,
    ListSnapshotsResponse, PreferAsync, RebuildSnapshotsRequest, RebuildSnapshotsResponse,
    Snapshot, SnapshotDetail, SnapshotId, UpdateSnapshotRequest, VerifySnapshotResponse,
};
use pisa_protocol::header::{header_key, split_header};
use pisa_protocol::labels::{
    DEFAULT_LIMIT, MAX_LABELS, MAX_LIMIT, is_cursor, parse_filter, snapshot_id_error,
};
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

/// The snapshot id `id`, from a path or a body.
pub(crate) fn parse_id(id: &str) -> Result<SnapshotId, ApiError> {
    SnapshotId::parse(id).ok_or_else(|| ApiError::new(ErrorKind::BadRequest, snapshot_id_error(id)))
}

#[utoipa::path(
    post, path = "/snapshots", operation_id = "createSnapshot", tag = "snapshots",
    summary = "Keep code as a snapshot",
    description = "Checks code like `/check` and, if Lean accepts it, keeps it as a module that \
        later code can import, with a new snapshot of it. Code that is already kept gets a new \
        snapshot of its module.",
    params(PreferAsync),
    request_body = CreateSnapshotRequest,
    responses((status = 200, description = "The snapshot, or why nothing was kept", body = CreateSnapshotResponse), Accepted, ApiErrors),
)]
pub async fn create(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<CreateSnapshotRequest>,
) -> Response {
    match prepare_create(&app, req) {
        Ok(p) => app.answer(prefer, JobKind::CreateSnapshot, p).await,
        Err(e) => e.into_response(),
    }
}

/// A create's job, routed to the worker that will serve code on top of the
/// snapshot, which builds it. The snapshot it replaces must be of its
/// version, and names the version when the request doesn't; only then does
/// the server's default apply.
pub(crate) fn prepare_create(app: &App, req: CreateSnapshotRequest) -> Result<Prepared, ApiError> {
    let mut version = req
        .version
        .as_deref()
        .and_then(|v| app.versions.resolve(Some(v)));
    if let Some(r) = &req.replaces {
        let replaced = parse_id(&r.snapshot_id)?.version;
        match &version {
            Some(v) if *v != replaced => {
                return Err(ApiError::new(
                    ErrorKind::BadRequest,
                    format!(
                        "replaces.snapshot_id {} is of version {replaced}, and the create of version {v}",
                        r.snapshot_id
                    ),
                ));
            }
            Some(_) => {}
            None => version = Some(replaced),
        }
    }
    let version = version.or_else(|| app.versions.resolve(None));
    let split = split_header(&req.code);
    let next = snapshot::next_header(&split.header, &snapshot::id(&req.code));
    let route = Route {
        version,
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
    Ok(Prepared {
        job,
        route,
        priority: Priority::Batch,
    })
}

pub(crate) fn created(done: JobResult) -> Result<CreateSnapshotResponse, ApiError> {
    let JobResult::SnapshotCreated(r) = done else {
        return Err(unexpected(done));
    };
    Ok(*r)
}

#[utoipa::path(
    get, path = "/snapshots/{id}", operation_id = "getSnapshot", tag = "snapshots",
    summary = "A snapshot, with its module's code, check and exports",
    params(("id" = String, Path, description = "The snapshot's id, `<version>~<32 hex>`")),
    responses((status = 200, description = "The snapshot", body = SnapshotDetail), ApiErrors),
)]
pub async fn get(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<SnapshotDetail>, ApiError> {
    let version = parse_id(&id)?.version;
    let job = Job::SnapshotGet(SnapshotJob { entry: id });
    match submit(&app, job, Some(version), Priority::Interactive).await? {
        JobResult::Snapshot(r) => Ok(Json(*r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    get, path = "/snapshots", operation_id = "listSnapshots", tag = "snapshots",
    summary = "List snapshots",
    description = "Newest first, with every label given. A full page has `next`: pass it as \
        `after` for the next one.",
    params(
        ("label" = Option<Vec<String>>, Query, description = "`key:value`, split on the first `:`; repeatable"),
        ("limit" = Option<usize>, Query, description = "1–1000, by default 100"),
        ("after" = Option<String>, Query, description = "The previous page's `next`"),
        ("version" = Option<String>, Query, description = "The worker version; the server's default when absent."),
    ),
    responses((status = 200, description = "A page of snapshots", body = ListSnapshotsResponse), ApiErrors),
)]
/// `GET /snapshots`. The query is parsed here rather than by
/// [`Query`], which can't collect the repeated `label`.
pub async fn list(
    State(app): State<Arc<App>>,
    RawQuery(query): RawQuery,
) -> Result<Json<ListSnapshotsResponse>, ApiError> {
    let (version, job) = list_query(query.as_deref().unwrap_or_default())
        .map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    let version = app.versions.resolve(version.as_deref());
    match submit(&app, Job::SnapshotList(job), version, Priority::Interactive).await? {
        JobResult::SnapshotList(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

/// A query's `label` filters, as `(key, value)` pairs.
type Filters = Vec<(String, String)>;

/// The version and the `label` filters of a query, at most [`MAX_LABELS`]
/// of them. Other parameters are ignored.
fn label_query(query: &str) -> Result<(Option<String>, Filters), String> {
    let mut version = None;
    let mut labels = Vec::new();
    for (name, value) in form_urlencoded::parse(query.as_bytes()) {
        match &*name {
            "version" => version = Some(value.into_owned()),
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
    Ok((version, labels))
}

/// Splits the query of `GET /snapshots` into the version and the job, and
/// checks the job's filters (see [`label_query`]), limit and cursor. Other
/// parameters are ignored.
fn list_query(query: &str) -> Result<(Option<String>, SnapshotListJob), String> {
    let (version, labels) = label_query(query)?;
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
    Ok((version, job))
}

#[utoipa::path(
    get, path = "/snapshots/audit", operation_id = "auditSnapshots", tag = "snapshots",
    summary = "What a project rests on",
    description = "For the snapshots with every label: their declarations by status, each \
        assumption (an axiom declared in snapshot code) with what uses it, wherever it is \
        declared, and the declarations that are `sorry` or `rejected`. It reads records only.",
    params(
        ("label" = Option<Vec<String>>, Query, description = "`key:value`, split on the first `:`; repeatable"),
        ("version" = Option<String>, Query, description = "The worker version; the server's default when absent."),
    ),
    responses((status = 200, description = "The audit", body = AuditSnapshotsResponse), ApiErrors),
)]
/// `GET /snapshots/audit`.
pub async fn audit(
    State(app): State<Arc<App>>,
    RawQuery(query): RawQuery,
) -> Result<Json<AuditSnapshotsResponse>, ApiError> {
    let (version, labels) = label_query(query.as_deref().unwrap_or_default())
        .map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    let version = app.versions.resolve(version.as_deref());
    let job = Job::SnapshotAudit(SnapshotAuditJob { labels });
    match submit(&app, job, version, Priority::Interactive).await? {
        JobResult::SnapshotAudit(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    get, path = "/snapshots/{id}/dependents", operation_id = "listDependents", tag = "snapshots",
    summary = "The snapshots above a snapshot",
    description = "The snapshots of every module that imports the snapshot's module, directly \
        or through others, parents first: by level, then oldest first. With `label` filters, \
        only the snapshots with every label.",
    params(
        ("id" = String, Path, description = "The snapshot's id, `<version>~<32 hex>`"),
        ("label" = Option<Vec<String>>, Query, description = "`key:value`, split on the first `:`; repeatable"),
    ),
    responses((status = 200, description = "The snapshots above it", body = ListDependentsResponse), ApiErrors),
)]
/// `GET /snapshots/{id}/dependents`.
pub async fn dependents(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Json<ListDependentsResponse>, ApiError> {
    let version = parse_id(&id)?.version;
    let (_, labels) = label_query(query.as_deref().unwrap_or_default())
        .map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    let job = Job::SnapshotDependents(SnapshotDependentsJob { entry: id, labels });
    match submit(&app, job, Some(version), Priority::Interactive).await? {
        JobResult::SnapshotDependents(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    post, path = "/snapshots/rebuild", operation_id = "rebuildSnapshots", tag = "snapshots",
    summary = "Rebuild a snapshot's dependents on its replacement",
    description = "Plans the creates that make each dependent of `from` with a snapshot that \
        has the labels again on `to`, by level, and unless `dry_run` runs them: a level at a \
        time, a failed chunk skipping only what builds on it, then with `retire` deleting the \
        old snapshots and `from`. Unless `to` replaces `from`, `to` must not change what `from` \
        states, or `allow_changes` be set. `from` and `to` are of one version, which the \
        rebuild runs on. Sending the same rebuild again replays the creates that are done. A \
        run can be async; a dry run answers at once.",
    params(PreferAsync),
    request_body = RebuildSnapshotsRequest,
    responses((status = 200, description = "The plan, and what the run did", body = RebuildSnapshotsResponse), Accepted, ApiErrors),
)]
/// `POST /snapshots/rebuild`.
pub async fn rebuild(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<RebuildSnapshotsRequest>,
) -> Response {
    if req.labels.is_empty() {
        let e = ApiError::new(
            ErrorKind::BadRequest,
            "a rebuild takes labels: which dependents to rebuild",
        );
        return e.into_response();
    }
    let version = match rebuild::version(&req) {
        Ok(version) => version,
        Err(e) => return e.into_response(),
    };
    if req.dry_run {
        let job = Job::SnapshotRebuild(SnapshotRebuildJob {
            from: req.from,
            to: req.to,
            labels: req.labels,
            allow_changes: req.allow_changes,
        });
        return respond(
            JobKind::RebuildSnapshots,
            submit_job(&app, job, version).await,
        );
    }
    if !prefer.respond_async {
        return match rebuild::run(&app, &req, &version).await {
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
        let outcome = rebuild::run(&app, &req, &version).await.map_err(|e| e.0);
        if let Err(e) = held.finish(&JobResult::SnapshotRebuild(outcome)).await {
            tracing::warn!("storing a rebuild's answer failed: {e}");
        }
    });
    accepted(job, JobKind::RebuildSnapshots, JobStatus::Running)
}

pub(crate) fn rebuilt(done: JobResult) -> Result<RebuildSnapshotsResponse, ApiError> {
    match done {
        JobResult::SnapshotRebuild(r) => r.map_err(ApiError),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    patch, path = "/snapshots/{id}", operation_id = "updateSnapshot", tag = "snapshots",
    summary = "Set or remove a snapshot's labels",
    description = "A JSON merge patch of the labels: `null` removes a label, a string sets it, \
        and the labels not named stay.",
    params(("id" = String, Path, description = "The snapshot's id, `<version>~<32 hex>`")),
    request_body = UpdateSnapshotRequest,
    responses((status = 200, description = "The snapshot, with its labels", body = Snapshot), ApiErrors),
)]
/// `PATCH /snapshots/{id}`.
pub async fn update(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Body(req): Body<UpdateSnapshotRequest>,
) -> Result<Json<Snapshot>, ApiError> {
    let version = parse_id(&id)?.version;
    let job = Job::SnapshotLabels(SnapshotLabelsJob {
        entry: id,
        labels: req.labels,
    });
    match submit(&app, job, Some(version), Priority::Interactive).await? {
        JobResult::SnapshotLabels(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    delete, path = "/snapshots/{id}", operation_id = "deleteSnapshot", tag = "snapshots",
    summary = "Delete a snapshot",
    description = "Its module goes too once no snapshot or other module refers to it: \
        `removed` lists the modules that went.",
    params(("id" = String, Path, description = "The snapshot's id, `<version>~<32 hex>`")),
    responses((status = 200, description = "What went", body = DeleteSnapshotResponse), ApiErrors),
)]
pub async fn delete(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<DeleteSnapshotResponse>, ApiError> {
    let version = parse_id(&id)?.version;
    let job = Job::SnapshotDelete(SnapshotJob { entry: id });
    match submit(&app, job, Some(version), Priority::Interactive).await? {
        JobResult::SnapshotDeleted(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    post, path = "/snapshots/{id}/verify", operation_id = "verifySnapshot", tag = "snapshots",
    summary = "Re-check a snapshot's module with the kernel",
    description = "Re-checks the module and the modules it imports; `failure` says which one \
        failed and why.",
    params(("id" = String, Path, description = "The snapshot's id, `<version>~<32 hex>`"), PreferAsync),
    responses((status = 200, description = "Whether the module passed", body = VerifySnapshotResponse), Accepted, ApiErrors),
)]
pub async fn verify(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    prefer: Prefer,
) -> Response {
    let version = match parse_id(&id) {
        Ok(id) => id.version,
        Err(e) => return e.into_response(),
    };
    let job = Job::SnapshotVerify(SnapshotJob { entry: id });
    let p = prepare(job, Some(version), Priority::Batch);
    app.answer(prefer, JobKind::VerifySnapshot, p).await
}

pub(crate) fn verified(done: JobResult) -> Result<VerifySnapshotResponse, ApiError> {
    match done {
        JobResult::SnapshotVerified(r) => r.map_err(ApiError),
        other => Err(unexpected(other)),
    }
}

/// A catalog job, for a worker of `version` (the pool label), or of any
/// version if `None`.
fn prepare(job: Job, version: Option<String>, priority: Priority) -> Prepared {
    let route = Route {
        version,
        ..Default::default()
    };
    Prepared {
        job,
        route,
        priority,
    }
}

/// Runs a catalog job on a worker of `version`, as an interactive one.
async fn submit_job(app: &App, job: Job, version: String) -> Result<JobResult, DispatchError> {
    app.dispatcher
        .submit(prepare(job, Some(version), Priority::Interactive))
        .await
}

pub(crate) async fn submit(
    app: &App,
    job: Job,
    version: Option<String>,
    priority: Priority,
) -> Result<JobResult, ApiError> {
    Ok(app
        .dispatcher
        .submit(prepare(job, version, priority))
        .await?)
}
