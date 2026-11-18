//! Snapshot endpoints. Creating a snapshot is a check whose code later code
//! imports, so it is routed like a check of that later code: its worker then
//! has the snapshot warm. Reads, listings, updates, deletes and verifies go
//! to any worker of the version, which a snapshot id names.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use pisa_protocol::api::{
    ACCEPT_TIMEOUT, AcceptRequest, AcceptResponse, Accepted, ApiErrors, AuditSnapshotsResponse,
    CheckResult, CreateSnapshotRequest, CreateSnapshotResponse, DEFAULT_REVIEW_LIMIT,
    DeleteSnapshotResponse, Detail, Error, ErrorKind, JobKind, JobStatus, ListAcceptancesResponse,
    ListDependentsResponse, ListSnapshotsResponse, MAX_REASON, MAX_REVIEW_LIMIT, PreferAsync,
    RebuildSnapshotsRequest, RebuildSnapshotsResponse, ResubmitSnapshotsRequest,
    ResubmitSnapshotsResponse, SettleRunRequest, Snapshot, SnapshotDetail, SnapshotId, Tarball,
    UpdateSnapshotRequest, VerifySnapshotResponse,
};
use pisa_protocol::header::{header_key, split_header};
use pisa_protocol::labels::{
    DEFAULT_LIMIT, MAX_LABELS, MAX_LIMIT, is_cursor, parse_filter, snapshot_id_error,
};
use pisa_protocol::snapshot;
use pisa_protocol::wire::{
    ExportedRun, Job, JobResult, PalomarExport, SnapshotAcceptJob, SnapshotCreateJob,
    SnapshotDeleteJob, SnapshotExportJob, SnapshotRebuildJob, SnapshotVerifyJob,
};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::dispatch::{Prepared, Priority, Route};
use crate::http::{ApiError, App, Body, ResolvedRuns};
use crate::jobs::{Params, Prefer, accepted};
use crate::rebuild;
use crate::resubmit;
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
    let mut p = match prepare_create(&app, req) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = resolve_create(&app, &mut p).await {
        // In-band, as a create's other failures of its imports are.
        let mut refused = CreateSnapshotResponse {
            check: CheckResult {
                error: Some(e),
                ..Default::default()
            },
            ..Default::default()
        };
        refused.refused = refused.refusal();
        return Json(refused).into_response();
    }
    app.answer(prefer, JobKind::CreateSnapshot, p).await
}

/// Resolves the imports by run name of the create `p` prepares, on its
/// version, keeping the code as sent, and routes it by the resolved code.
async fn resolve_create(app: &App, p: &mut Prepared) -> Result<(), Error> {
    let (Job::SnapshotCreate(job)
    | Job::SnapshotReplace(job)
    | Job::SnapshotAmend(job)
    | Job::SnapshotRefute(job)) = &mut p.job
    else {
        return Ok(());
    };
    let requested = p.route.version.as_deref();
    let resolved = match replayed(app, requested, job).await? {
        Some(resolved) => Some(resolved),
        None => app.resolve_runs(requested, &job.code).await?,
    };
    let Some(resolved) = resolved else {
        return Ok(());
    };
    // A replacement that imported its own run would import the head it
    // replaces.
    if let Some(r) = &job.replaces {
        let reader = app.snapshots.version(&resolved.version);
        let replaced = reader.parse(&r.snapshot_id)?;
        let own = reader
            .catalog()
            .run_names_of(std::slice::from_ref(&replaced))
            .await?;
        if let Some(own) = own.get(&replaced)
            && resolved.runs.iter().any(|run| run.name == *own)
        {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!(
                    "{}.{own} is the run this snapshot replaces into, which it can't import: \
                     that would import the head it replaces",
                    snapshot::RUNS
                ),
            ));
        }
    }
    job.source = Some(std::mem::replace(&mut job.code, resolved.code));
    job.runs = resolved.runs;
    let split = split_header(&job.code);
    let next = snapshot::next_header(&split.header, &snapshot::id(&job.code));
    p.route.header_key = Some(header_key(&next));
    p.route.version = Some(resolved.version);
    Ok(())
}

/// How a create sent again with its key resolves: as its first create did,
/// when the key's snapshot (in the request's scope) was sent as the same
/// text, so the replay answers what it answered then, whatever the runs did
/// since. `None` otherwise, and for code that imports no run by name.
async fn replayed(
    app: &App,
    requested: Option<&str>,
    job: &SnapshotCreateJob,
) -> Result<Option<ResolvedRuns>, Error> {
    let Some(key) = &job.key else {
        return Ok(None);
    };
    let header = split_header(&job.code).header;
    if snapshot::run_imports(&header).map_or(true, |names| names.is_empty()) {
        return Ok(None);
    }
    let version = app.read_version(requested).await.map_err(|e| e.0)?;
    let reader = app.snapshots.version(&version);
    let Some((_, kept)) = reader.catalog().keyed(key).await? else {
        return Ok(None);
    };
    if kept.source.as_deref() != Some(job.code.as_str()) {
        return Ok(None);
    }
    Ok(Some(ResolvedRuns {
        code: pisa_snapshots::resolve_with(&job.code, &kept.runs),
        runs: kept.runs,
        version,
    }))
}

/// Checks that snapshot `id`, which a create's `field` names, is of the
/// create's `version`, which it sets when the create names none.
fn same_version(version: &mut Option<String>, field: &str, id: &str) -> Result<(), ApiError> {
    let named = parse_id(id)?.version;
    match version {
        Some(v) if *v != named => Err(ApiError::new(
            ErrorKind::BadRequest,
            format!(
                "{field}.snapshot_id {id} is of version {named}, and the create of version {v}"
            ),
        )),
        Some(_) => Ok(()),
        None => {
            *version = Some(named);
            Ok(())
        }
    }
}

/// A create's job, routed to the worker that will serve code on top of the
/// snapshot, which builds it. The snapshot it replaces must be of its
/// version, and names the version when the request doesn't; only then does
/// the server's default apply.
/// What the key the server gives a create without one starts with.
const JOB_KEY: &str = "job:";

pub(crate) fn prepare_create(app: &App, req: CreateSnapshotRequest) -> Result<Prepared, ApiError> {
    let mut version = req
        .version
        .as_deref()
        .and_then(|v| app.versions.resolve(Some(v)));
    if req.key.as_deref().is_some_and(|k| k.starts_with(JOB_KEY)) {
        return Err(ApiError::new(
            ErrorKind::BadRequest,
            format!("idempotency keys that start with {JOB_KEY} are the server's own"),
        ));
    }
    match &req.holes {
        Some(holes) if holes.is_empty() && req.amends.is_none() => {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "holes is empty: to leave no definition to change, leave holes out",
            ));
        }
        Some(_) if req.replaces.is_some() => {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "holes are a spec's: a snapshot that replaces another inherits its holes",
            ));
        }
        _ => {}
    }
    if let Some(r) = &req.replaces {
        if r.allow.as_ref().is_some_and(|a| a.definitions.is_empty()) {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "replaces.allow.definitions is empty: to change no definition, leave allow out",
            ));
        }
        same_version(&mut version, "replaces", &r.snapshot_id)?;
    }
    let mut req = req;
    if let Some(a) = &req.amends {
        if req.replaces.is_some() {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "a spec that amends another replaces nothing: create the amended spec, then \
                 resubmit the run's heads to it with POST /snapshots/resubmit",
            ));
        }
        if req.verify == Some(false) {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "an amending spec is always re-checked: leave verify out",
            ));
        }
        let chars = a.reason.chars().count();
        if a.reason.trim().is_empty() || chars > MAX_REASON {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                format!("amends.reason has {chars} characters; it takes 1 to {MAX_REASON}"),
            ));
        }
        same_version(&mut version, "amends", &a.snapshot_id)?;
        req.verify = Some(true);
    }
    if let Some(r) = &req.refutes {
        if req.replaces.is_some() || req.holes.is_some() || req.amends.is_some() {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "a refutation is a snapshot of its own: it replaces nothing, has no holes and \
                 amends no spec",
            ));
        }
        if req.verify == Some(false) {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "a refutation is always re-checked: leave verify out",
            ));
        }
        same_version(&mut version, "refutes", &r.snapshot_id)?;
        req.verify = Some(true);
    }
    if let Some(name) = &req.name {
        if req.replaces.is_some() {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "a run is named by the snapshot that starts it: a replacement is in the run of \
                 what it replaces, so it takes no name",
            ));
        }
        snapshot::check_run_name(name).map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    }
    let version = version.or_else(|| app.versions.resolve(None));
    let split = split_header(&req.code);
    let next = snapshot::next_header(&split.header, &snapshot::id(&req.code));
    let route = Route {
        version,
        header_key: Some(header_key(&next)),
        header: None,
        document: None,
    };
    let (replace, amend, refute) = (
        req.replaces.is_some(),
        req.amends.is_some(),
        req.refutes.is_some(),
    );
    let create = SnapshotCreateJob {
        code: req.code,
        timeout_secs: req.timeout,
        verify: req.verify,
        labels: req.labels,
        // A create without a key gets one of its own, so a retry of its job
        // after its worker was lost returns what the first attempt kept.
        key: Some(
            req.key
                .unwrap_or_else(|| format!("{JOB_KEY}{}", uuid::Uuid::new_v4().simple())),
        ),
        replaces: req.replaces,
        fork: req.fork,
        holes: req.holes,
        amends: req.amends,
        refutes: req.refutes,
        detail: req.detail,
        name: req.name,
        source: None,
        runs: Vec::new(),
    };
    let job = if replace {
        Job::SnapshotReplace(create)
    } else if amend {
        Job::SnapshotAmend(create)
    } else if refute {
        Job::SnapshotRefute(create)
    } else {
        Job::SnapshotCreate(create)
    };
    Ok(Prepared {
        job,
        route,
        priority: Priority::Batch,
    })
}

/// A create's answer, saying what to do next if it kept nothing. A worker
/// that failed is in-band, as for `/check`: a create carries a key, so
/// sending it again is safe.
pub(crate) fn created(done: JobResult) -> Result<CreateSnapshotResponse, ApiError> {
    let mut r = match done {
        JobResult::SnapshotCreated(r) => *r,
        other => CreateSnapshotResponse {
            check: crate::http::failed_result(other),
            ..Default::default()
        },
    };
    r.refused = r.refusal();
    Ok(r)
}

#[utoipa::path(
    get, path = "/snapshots/{id}", operation_id = "getSnapshot", tag = "snapshots",
    summary = "A snapshot, with its module's code, check and exports",
    params(("id" = String, Path, description = "The snapshot's id, `<version>~<32 hex>`"), GetParams),
    responses((status = 200, description = "The snapshot", body = SnapshotDetail), ApiErrors),
)]
/// `GET /snapshots/{id}`. The query is taken as a `Result` so that the id is
/// checked first.
pub async fn get(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    q: Result<Params<GetParams>, ApiError>,
) -> Result<Json<SnapshotDetail>, ApiError> {
    let version = parse_id(&id)?.version;
    let Params(GetParams { detail }) = q?;
    let reader = app.snapshots.version(&version);
    let mut got = reader.get(&id).await?;
    if detail != Some(Detail::Declarations) {
        let full = std::mem::take(&mut got.check.declarations);
        let allowed = reader.allowed_axioms();
        got.check.declarations = full.into_iter().map(|d| d.summarised(allowed)).collect();
    }
    Ok(Json(got))
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
        ("heads" = Option<bool>, Query, description = "`true` for the snapshots that no other snapshot replaces; every snapshot by default"),
        ("version" = Option<String>, Query, description = "The version whose snapshots to list; when absent, the server's default, or else the one version the live workers serve."),
    ),
    responses((status = 200, description = "A page of snapshots", body = ListSnapshotsResponse), ApiErrors),
)]
/// `GET /snapshots`. The query is parsed here rather than by
/// [`Query`], which can't collect the repeated `label`.
pub async fn list(
    State(app): State<Arc<App>>,
    RawQuery(query): RawQuery,
) -> Result<Json<ListSnapshotsResponse>, ApiError> {
    let (version, q) = list_query(query.as_deref().unwrap_or_default())
        .map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    let reader = app
        .snapshots
        .version(&app.read_version(version.as_deref()).await?);
    let listed = reader.list(&q.labels, q.limit, q.after.as_deref(), q.heads);
    Ok(Json(listed.await?))
}

/// `GET /snapshots`'s filters, page and `heads`.
struct ListQuery {
    labels: Vec<(String, String)>,
    limit: usize,
    after: Option<String>,
    heads: bool,
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
/// checks the job's filters (see [`label_query`]), limit, cursor and
/// `heads`. Other parameters are ignored.
fn list_query(query: &str) -> Result<(Option<String>, ListQuery), String> {
    let (version, labels) = label_query(query)?;
    let mut job = ListQuery {
        labels,
        limit: DEFAULT_LIMIT,
        after: None,
        heads: false,
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
            "heads" => {
                job.heads = value
                    .parse()
                    .map_err(|_| format!("heads {value:?} must be true or false"))?
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
        declared, and the declarations that are `sorry` or `rejected`. With `against` \
        instead, for a run: the snapshots it reaches from the spec snapshots through the \
        snapshots that replace them, to its heads, with the forks on the way, each head \
        compared with its spec, allowing only the spec's holes and the definitions `allow` \
        names to change, each spec declaration's status on each head, the links that \
        allowed changes, and whether the run is `clean`. The server reads the records \
        itself, so the audit needs no worker.",
    params(
        ("label" = Option<Vec<String>>, Query, description = "`key:value`, split on the first `:`; repeatable"),
        ("against" = Option<Vec<String>>, Query, description = "A spec snapshot's id, `<version>~<32 hex>`; repeatable, all of one version, and not with `label`"),
        ("allow" = Option<Vec<String>>, Query, description = "With `against`, a definition the run's heads may change, as `allow` names it in `replaces` (`_private.h` for a private one); repeatable. The links' own allows don't count: by default only the spec's holes may change"),
        ("version" = Option<String>, Query, description = "The version whose snapshots to audit; when absent, the server's default, or else the one version the live workers serve. With `against`, the ids' version, which it must be if given."),
        ("review" = Option<bool>, Query, description = "With `against`, `true` adds `review`: for each spec snapshot and head, the declarations the spec's statements depend on there, each hole with its value"),
        ("review_limit" = Option<u32>, Query, description = "With `review`, the most declarations the review list holds over all its entries, 1 to 10000, 1000 by default; `review_left_out` counts the rest"),
    ),
    responses((status = 200, description = "The audit", body = AuditSnapshotsResponse), ApiErrors),
)]
/// `GET /snapshots/audit`.
pub async fn audit(
    State(app): State<Arc<App>>,
    RawQuery(query): RawQuery,
) -> Result<Json<AuditSnapshotsResponse>, ApiError> {
    let query = query.as_deref().unwrap_or_default();
    let (version, labels) =
        label_query(query).map_err(|e| ApiError::new(ErrorKind::BadRequest, e))?;
    let allow = allow_query(query)?;
    let review = review_query(query)?;
    let (version, against) = match against_query(query, &labels)? {
        Some((ids, against)) => {
            // A version beside the ids must be theirs, as with a create's
            // `replaces`.
            if let Some(v) = version
                .as_deref()
                .and_then(|v| app.versions.resolve(Some(v)))
                && v != ids
            {
                return Err(ApiError::new(
                    ErrorKind::BadRequest,
                    format!("against names snapshots of version {ids}, and the query version {v}"),
                ));
            }
            (Some(ids), against)
        }
        None if !allow.is_empty() => {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "allow is for an audit against a spec: it names the definitions a run's heads \
                 may change",
            ));
        }
        None if review => {
            return Err(ApiError::new(
                ErrorKind::BadRequest,
                "review is for an audit against a spec: it lists what the spec's statements \
                 depend on, on each head",
            ));
        }
        None => (
            Some(app.read_version(version.as_deref()).await?),
            Vec::new(),
        ),
    };
    let reader = app.snapshots.version(&version.unwrap_or_default());
    if against.is_empty() {
        return Ok(Json(reader.audit(&labels).await?));
    }
    let mut audit = reader.audit_against(&against, &allow).await?;
    if review {
        let limit = review_limit_query(query)?;
        let reviewed = pisa_snapshots::review::review(&reader, &audit.comparisons, &allow, limit);
        (audit.review, audit.review_left_out) = reviewed.await?;
    }
    Ok(Json(audit))
}

/// How many declarations an audit's review list may hold.
fn review_limit_query(query: &str) -> Result<usize, ApiError> {
    let given = form_urlencoded::parse(query.as_bytes()).find(|(name, _)| name == "review_limit");
    let Some((_, value)) = given else {
        return Ok(DEFAULT_REVIEW_LIMIT);
    };
    match value.parse::<usize>() {
        Ok(n) if (1..=MAX_REVIEW_LIMIT).contains(&n) => Ok(n),
        _ => Err(ApiError::new(
            ErrorKind::BadRequest,
            format!("review_limit is 1 to {MAX_REVIEW_LIMIT}, not {value}"),
        )),
    }
}

/// Whether an audit's query asks for the review list.
fn review_query(query: &str) -> Result<bool, ApiError> {
    let mut review = false;
    for (name, value) in form_urlencoded::parse(query.as_bytes()) {
        if name == "review" {
            review = match &*value {
                "true" => true,
                "false" => false,
                _ => {
                    return Err(ApiError::new(
                        ErrorKind::BadRequest,
                        format!("review is true or false, not {value}"),
                    ));
                }
            };
        }
    }
    Ok(review)
}

/// The definitions an audit's query allows to change.
fn allow_query(query: &str) -> Result<Vec<String>, ApiError> {
    let allow: Vec<String> = form_urlencoded::parse(query.as_bytes())
        .filter(|(name, _)| name == "allow")
        .map(|(_, value)| value.into_owned())
        .collect();
    if allow.iter().any(String::is_empty) {
        return Err(ApiError::new(
            ErrorKind::BadRequest,
            "allow names a definition, and is empty",
        ));
    }
    Ok(allow)
}

/// The `against` ids of an audit's query, if it has any, with the version
/// they share. They don't combine with `label` filters.
fn against_query(query: &str, labels: &Filters) -> Result<Option<(String, Vec<String>)>, ApiError> {
    let against: Vec<String> = form_urlencoded::parse(query.as_bytes())
        .filter(|(name, _)| name == "against")
        .map(|(_, value)| value.into_owned())
        .collect();
    if against.is_empty() {
        return Ok(None);
    }
    let bad = |message: String| ApiError::new(ErrorKind::BadRequest, message);
    if !labels.is_empty() {
        return Err(bad(
            "label and against don't combine: audit a project by its labels, or a run against \
             its spec"
                .into(),
        ));
    }
    Ok(Some((run_version(&against)?, against)))
}

/// The version that a run's spec snapshots `against` share.
fn run_version(against: &[String]) -> Result<String, ApiError> {
    let bad = |message: String| ApiError::new(ErrorKind::BadRequest, message);
    let mut versions: Vec<String> = Vec::new();
    for id in against {
        let version = parse_id(id)?.version;
        if !versions.contains(&version) {
            versions.push(version);
        }
    }
    match versions.as_slice() {
        [version] => Ok(version.clone()),
        [] => Err(bad(
            "against names the run's spec snapshots, and is empty".into()
        )),
        _ => Err(bad(format!(
            "against names snapshots of versions {}: a run is of one version",
            versions.join(", ")
        ))),
    }
}

#[utoipa::path(
    get, path = "/snapshots/export", operation_id = "exportRun", tag = "snapshots",
    summary = "A run as a Lake project",
    description = "The run that the spec snapshots start, as `GET /snapshots/audit?against=` \
        walks it, as a Lake project in `pisa-run/` for `lake comparator`, which shares no code \
        with the audit: every module the spec snapshots and their heads need, the worker \
        project's toolchain, manifest and requirements, and a configuration in `comparator/` \
        for each spec snapshot and head it reaches. Each names the spec's theorems, its holes \
        and `allow` as definition holes, and the standard axioms and the spec's assumptions as \
        permitted. It is for judging the run outside pisa: `POST /snapshots/accept` judges \
        pisa's own builds of the same modules, with these configurations naming them by their \
        hash. With `profile=palomar`, one pair is a Palomar package in `palomar/` instead: its \
        challenge and solution named \
        `challenge_module` and `solution_module`, and every module the pair needs made a \
        module-system file; a `lakefile.toml` that requires Mathlib alone, at the commit the \
        worker's manifest resolved, and that manifest without the worker's other packages, \
        each package pinned to its commit; the pair's configuration as `comparator.json`; and \
        `formalization.yaml`, `REVIEW.md` and `conversion.json` for what pisa can't decide and \
        what it changed. A pair whose spec permits an axiom of its own, whose challenge imports \
        anything but Mathlib, Init and Lean, that imports a module that is neither the run's, \
        Lean's, Mathlib's nor of a package Mathlib builds on (an allowlist, so the worker's \
        Physlib is refused), or whose spec is its own head is a `bad_request` that names it.",
    params(
        ("against" = Vec<String>, Query, description = "A spec snapshot's id, `<version>~<32 hex>`; repeatable, all of one version"),
        ("allow" = Option<Vec<String>>, Query, description = "A definition the run's heads may change besides the spec's holes, as for the audit; repeatable"),
        ("profile" = Option<String>, Query, description = "`palomar` for one pair of the run as a Palomar package"),
        ("pair" = Option<u32>, Query, minimum = 1, description = "With `profile`, the pair to package, from 1 as `comparator/<n>.json` numbers them; needed unless the run has one pair"),
        ("challenge_module" = Option<String>, Query, description = "With `profile`, the challenge's module name; `Challenge` by default"),
        ("solution_module" = Option<String>, Query, description = "With `profile`, the solution's module name; `Solution` by default"),
    ),
    responses(
        (status = 200, description = "The project, as a tar.gz", content_type = "application/gzip", body = Tarball),
        ApiErrors,
    ),
)]
/// `GET /snapshots/export`.
pub async fn export(
    State(app): State<Arc<App>>,
    RawQuery(query): RawQuery,
) -> Result<Response, ApiError> {
    let query = query.as_deref().unwrap_or_default();
    let allow = allow_query(query)?;
    let palomar = palomar_query(query)?;
    let Some((version, against)) = against_query(query, &Filters::new())? else {
        return Err(ApiError::new(
            ErrorKind::BadRequest,
            "against names the run's spec snapshots, and is required",
        ));
    };
    let root = match palomar {
        Some(_) => PALOMAR_ROOT,
        None => EXPORT_ROOT,
    };
    let job = Job::SnapshotExport(SnapshotExportJob {
        against,
        allow,
        palomar,
    });
    let run = match submit(&app, job, Some(version), Priority::Interactive).await? {
        JobResult::SnapshotExport(r) => r.map_err(ApiError)?,
        other => return Err(unexpected(other)),
    };
    let tarball = tarball(&run, root)
        .map_err(|e| ApiError::new(ErrorKind::Internal, format!("packing the export: {e}")))?;
    let headers = [
        (CONTENT_TYPE, "application/gzip".to_string()),
        (
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{root}.tar.gz\""),
        ),
    ];
    Ok((headers, tarball).into_response())
}

/// The Palomar package an export's query asks for with `profile=palomar`,
/// whose `pair`, `challenge_module` and `solution_module` are for it alone.
fn palomar_query(query: &str) -> Result<Option<PalomarExport>, ApiError> {
    let get = |key: &str| {
        let mut pairs = form_urlencoded::parse(query.as_bytes());
        pairs
            .find(|(name, _)| name == key)
            .map(|(_, v)| v.into_owned())
    };
    let bad = |message: String| ApiError::new(ErrorKind::BadRequest, message);
    let (pair, challenge, solution) =
        (get("pair"), get("challenge_module"), get("solution_module"));
    match get("profile").as_deref() {
        Some("palomar") => {}
        Some(other) => return Err(bad(format!("profile is palomar, not {other}"))),
        None if pair.is_some() || challenge.is_some() || solution.is_some() => {
            return Err(bad(
                "pair, challenge_module and solution_module are for profile=palomar".into(),
            ));
        }
        None => return Ok(None),
    }
    let pair = match pair.map(|p| (p.parse::<u32>(), p)) {
        None => None,
        Some((Ok(n), _)) if n >= 1 => Some(n),
        Some((_, p)) => {
            return Err(bad(format!(
                "pair is a pair's number, from 1 as comparator/<n>.json numbers them, not {p}"
            )));
        }
    };
    let challenge_module = module_name("challenge_module", challenge, "Challenge")?;
    let solution_module = module_name("solution_module", solution, "Solution")?;
    if challenge_module == solution_module {
        return Err(bad(format!(
            "challenge_module and solution_module are both {challenge_module}: two modules need \
             two names"
        )));
    }
    Ok(Some(PalomarExport {
        pair,
        challenge_module,
        solution_module,
    }))
}

/// The module name a query gives as `key`, or `default`: dotted parts, each
/// a letter or `_` then letters, digits and `_`, and none of Lean's
/// keywords.
fn module_name(key: &str, name: Option<String>, default: &str) -> Result<String, ApiError> {
    let name = name.unwrap_or_else(|| default.to_string());
    let part = |part: &str| {
        let mut chars = part.chars();
        let first = chars.next();
        first.is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !snapshot::KEYWORDS.contains(&part)
    };
    if name.split('.').all(part) {
        return Ok(name);
    }
    Err(ApiError::new(
        ErrorKind::BadRequest,
        format!(
            "{key} is a module name, dotted parts each a letter or '_' then letters, digits and \
             '_', none of them a Lean keyword, not {name:?}"
        ),
    ))
}

/// The directory an export unpacks to.
const EXPORT_ROOT: &str = "pisa-run";

/// The directory a Palomar package unpacks to.
const PALOMAR_ROOT: &str = "palomar";

/// `run`'s files as a tar.gz, in `root`.
fn tarball(run: &ExportedRun, root: &str) -> std::io::Result<Vec<u8>> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for f in &run.files {
        let mut header = tar::Header::new_gnu();
        header.set_size(f.text.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(now);
        let path = format!("{root}/{}", f.path);
        tar.append_data(&mut header, path, f.text.as_bytes())?;
    }
    tar.into_inner()?.finish()
}

#[utoipa::path(
    post, path = "/snapshots/accept", operation_id = "acceptRun", tag = "snapshots",
    summary = "Judge a run and store the verdict",
    description = "Audits the run that the spec snapshots start, as `GET /snapshots/audit` \
        does, and runs `lake comparator`, which shares no code with the audit, on exactly the \
        pairs the audit compared, each spec snapshot against each head it reaches. Comparator \
        judges the modules pisa built and kernel-checked when the snapshots were created, \
        exported with Lean's bundled `leanexport`, and builds nothing; its configurations are \
        the export's (`GET /snapshots/export`). A head passes comparator if each theorem of the spec \
        is proved there with the same statement, the definitions those statements use are the \
        same but for the spec's holes and `allow`, and the proofs use only the standard axioms \
        and the spec's assumptions. The run is `accepted` when the audit is `clean` and \
        comparator accepted every pair. The result is stored, and \
        `GET /snapshots/accept/{id}` reads it again. A run with heads that hold only through \
        links (the audit's `unsettled`) is a 409 `conflict` at once, naming the settle to send. \
        A Mathlib-sized pair takes about half a minute, or a minute paranoid: send it with \
        `Prefer: respond-async`.",
    params(PreferAsync),
    request_body = AcceptRequest,
    responses((status = 200, description = "The audit and comparator's verdict on each pair", body = AcceptResponse), Accepted, ApiErrors),
)]
/// `POST /snapshots/accept`.
pub async fn accept(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<AcceptRequest>,
) -> Response {
    let version = match run_version(&req.against) {
        Ok(version) => version,
        Err(e) => return e.into_response(),
    };
    if req.allow.iter().any(String::is_empty) {
        let e = ApiError::new(
            ErrorKind::BadRequest,
            "allow names a definition, and is empty",
        );
        return e.into_response();
    }
    let job = Job::SnapshotAccept(SnapshotAcceptJob {
        against: req.against,
        allow: req.allow,
        paranoid: req.paranoid,
        timeout_secs: req.timeout.unwrap_or(ACCEPT_TIMEOUT),
        // A worker of the release before builds the run in this; this release's ignore it.
        build_timeout_secs: 1800,
    });
    let p = prepare(job, Some(version), Priority::Batch);
    app.answer(prefer, JobKind::AcceptRun, p).await
}

pub(crate) fn judged(done: JobResult) -> Result<AcceptResponse, ApiError> {
    match done {
        JobResult::SnapshotAccepted(r) => r.map(|r| *r).map_err(ApiError),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    get, path = "/snapshots/accept/{id}", operation_id = "getAcceptance", tag = "snapshots",
    summary = "A stored acceptance",
    description = "What `POST /snapshots/accept` answered, as it was stored.",
    params(("id" = String, Path, description = "The acceptance's id, `<version>~<32 hex>`")),
    responses((status = 200, description = "The acceptance", body = AcceptResponse), ApiErrors),
)]
/// `GET /snapshots/accept/{id}`.
pub async fn acceptance(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<AcceptResponse>, ApiError> {
    let parsed = SnapshotId::parse(&id).ok_or_else(|| {
        ApiError::new(
            ErrorKind::BadRequest,
            format!("{id} isn't an acceptance's id, `<version>~<32 hex>`"),
        )
    })?;
    let reader = app.snapshots.version(&parsed.version);
    let Some(stored) = reader.catalog().acceptance(&parsed.entry).await? else {
        return Err(ApiError::new(
            ErrorKind::NotFound,
            format!("no acceptance {id}"),
        ));
    };
    Ok(Json(read_back(&reader, stored, &mut HashMap::new()).await?))
}

#[utoipa::path(
    get, path = "/snapshots/accept", operation_id = "listAcceptances", tag = "snapshots",
    summary = "A spec snapshot's acceptances",
    description = "The stored acceptances that judged a spec snapshot, newest first, at most \
        100. Each says `stale` once the run's heads are no longer the ones it judged, its spec \
        is amended, or a target is refuted that its audit didn't list.",
    params(("spec" = String, Query, description = "The spec snapshot's id, `<version>~<32 hex>`, which may be deleted since")),
    responses((status = 200, description = "Its acceptances", body = ListAcceptancesResponse), ApiErrors),
)]
/// `GET /snapshots/accept?spec=<id>`.
pub async fn acceptances(
    State(app): State<Arc<App>>,
    RawQuery(query): RawQuery,
) -> Result<Json<ListAcceptancesResponse>, ApiError> {
    let spec = form_urlencoded::parse(query.as_deref().unwrap_or_default().as_bytes())
        .find(|(name, _)| name == "spec")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| {
            ApiError::new(ErrorKind::BadRequest, "name the spec snapshot: ?spec=<id>")
        })?;
    let parsed = parse_id(&spec)?;
    let reader = app.snapshots.version(&parsed.version);
    let mut acceptances = Vec::new();
    let mut specs = HashMap::new();
    for stored in reader.catalog().acceptances_of(&parsed.entry, 100).await? {
        acceptances.push(read_back(&reader, stored, &mut specs).await?);
    }
    Ok(Json(ListAcceptancesResponse { acceptances }))
}

/// What a spec snapshot's run is now, for reading acceptances back: its
/// heads, whether an amendment supersedes it, and the snapshots that refute
/// its targets. `None` once it is gone.
struct SpecNow {
    heads: Vec<String>,
    superseded: bool,
    refuted_by: BTreeSet<String>,
}

async fn spec_now(
    reader: &pisa_snapshots::Reader,
    entry: &str,
) -> Result<Option<SpecNow>, ApiError> {
    let heads = match reader.heads_from(entry).await {
        Ok(heads) => heads,
        Err(e) if e.kind == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let superseded = reader.superseding(entry).await?.is_some();
    let refuted_by = (reader.refutations_against(entry).await?)
        .into_iter()
        .map(|(_, r)| r.snapshot_id)
        .collect();
    Ok(Some(SpecNow {
        heads,
        superseded,
        refuted_by,
    }))
}

/// A stored acceptance as it is answered: `stale` once its specs' runs'
/// heads are no longer the ones it judged, a spec is gone or amended, or a
/// target is refuted that its audit didn't list. `specs` keeps what each
/// spec is now across a listing.
async fn read_back(
    reader: &pisa_snapshots::Reader,
    stored: serde_json::Value,
    specs: &mut HashMap<String, Option<SpecNow>>,
) -> Result<AcceptResponse, ApiError> {
    let mut accepted: AcceptResponse = serde_json::from_value(stored)
        .map_err(|e| ApiError::new(ErrorKind::Internal, format!("a stored acceptance: {e}")))?;
    let judged_refutations: BTreeSet<&String> = (accepted.audit.refutations.iter())
        .map(|r| &r.snapshot_id)
        .collect();
    let mut now = BTreeSet::new();
    let mut stale = false;
    for spec in &accepted.against {
        let Ok(entry) = reader.parse(spec) else {
            stale = true;
            continue;
        };
        if !specs.contains_key(&entry) {
            let found = spec_now(reader, &entry).await?;
            specs.insert(entry.clone(), found);
        }
        match &specs[&entry] {
            Some(spec) => {
                now.extend(spec.heads.iter().cloned());
                stale |= spec.superseded;
                stale |= !spec
                    .refuted_by
                    .iter()
                    .all(|r| judged_refutations.contains(r));
            }
            None => stale = true,
        }
    }
    let judged: BTreeSet<String> = accepted.heads.iter().cloned().collect();
    accepted.stale = stale || now != judged;
    Ok(accepted)
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
    let reader = app.snapshots.version(&version);
    Ok(Json(reader.dependents(&id, &labels).await?))
}

#[utoipa::path(
    post, path = "/snapshots/rebuild", operation_id = "rebuildSnapshots", tag = "snapshots",
    summary = "Rebuild a snapshot's dependents on its replacement",
    description = "Plans the creates that copy each head among the dependents of `from` with \
        the labels onto `to`, by level, each copy replacing its original when its statements \
        keep their meaning, and unless `dry_run` runs them: a level at a time, a failed chunk \
        skipping only what builds on it, then with `retire` deleting the originals their \
        copies replace; `from` stays. A labelled snapshot that one without the labels \
        replaces is listed in `replaced`, not copied. Unless `to` replaces `from`, \
        `to` must not change what `from` states, or `allow_changes` be set. `from` and `to` \
        are of one version, which the rebuild runs on. Sending the same rebuild again replays \
        the creates that are done. A run can be async; a dry run answers at once.",
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
        let plan = SnapshotRebuildJob {
            from: req.from,
            to: req.to,
            labels: req.labels,
            allow_changes: req.allow_changes,
        };
        return match app.snapshots.version(&version).rebuild_plan(&plan).await {
            Ok(r) => Json(r).into_response(),
            Err(e) => ApiError(e).into_response(),
        };
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
    // The run creates in the request's scope.
    tokio::spawn(pisa_db::scope::within(
        pisa_db::scope::current(),
        async move {
            let outcome = rebuild::run(&app, &req, &version).await.map_err(|e| e.0);
            if let Err(e) = held.finish(&JobResult::SnapshotRebuild(outcome)).await {
                tracing::warn!("storing a rebuild's answer failed: {e}");
            }
        },
    ));
    accepted(job, JobKind::RebuildSnapshots, JobStatus::Running)
}

#[utoipa::path(
    post, path = "/snapshots/resubmit", operation_id = "resubmitSnapshots", tag = "snapshots",
    summary = "Carry an amended spec's run over to the spec that amends it",
    description = "Compares each head of the run of `from` with `to`, the spec that amends \
        it, as an audit against `to` would, allowing `to`'s holes to change, and unless \
        `dry_run` creates each head that keeps `to`'s statements again: the same code, \
        replacing `to`, with its head's labels or the request's. The heads of a forked run \
        come over forked. A head that changes a statement is listed with its changes, and one \
        whose record the kernel hasn't re-checked with that reason. `from` may be deleted. \
        Sending the same resubmit again replays the creates that are done. A run can be async; \
        a dry run answers at once.",
    params(PreferAsync),
    request_body = ResubmitSnapshotsRequest,
    responses((status = 200, description = "The plan, and what the run did", body = ResubmitSnapshotsResponse), Accepted, ApiErrors),
)]
/// `POST /snapshots/resubmit`.
pub async fn resubmit(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<ResubmitSnapshotsRequest>,
) -> Response {
    let version = match resubmit::version(&req) {
        Ok(version) => version,
        Err(e) => return e.into_response(),
    };
    if req.dry_run || !prefer.respond_async {
        return match resubmit::run(&app, &req, &version).await {
            Ok(r) => Json(r).into_response(),
            Err(e) => e.into_response(),
        };
    }
    // Refused at once, as the request would be.
    if let Err(e) = app
        .snapshots
        .version(&version)
        .resubmit_plan(&req.from, &req.to)
        .await
    {
        return ApiError(e).into_response();
    }
    let record = Job::SnapshotResubmit(req.clone());
    let held = match app
        .dispatcher
        .hold(JobKind::ResubmitSnapshots, &record)
        .await
    {
        Ok(held) => held,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let job = held.token.clone();
    // The run creates in the request's scope.
    tokio::spawn(pisa_db::scope::within(
        pisa_db::scope::current(),
        async move {
            let outcome = (resubmit::run(&app, &req, &version).await)
                .map(Box::new)
                .map_err(|e| e.0);
            if let Err(e) = held.finish(&JobResult::SnapshotResubmit(outcome)).await {
                tracing::warn!("storing a resubmit's answer failed: {e}");
            }
        },
    ));
    accepted(job, JobKind::ResubmitSnapshots, JobStatus::Running)
}

#[utoipa::path(
    post, path = "/snapshots/settle", operation_id = "settleRun", tag = "snapshots",
    summary = "Rebuild a run's heads onto what their targets hold through",
    description = "Audits the run that the spec snapshots `against` start, under `allow`, and \
        rebuilds each head in its `unsettled` onto the heads its targets are read through, in \
        one plan, as a rebuild would: each copy replaces its head, what is between is rebuilt \
        as support items, and those go once every level has run. Its targets' `status` is then \
        what their `linked_status` was. A head the audit lists blockers for refuses the plan, \
        with them in `changes`; nothing to settle is an empty plan. A run can be async; a dry \
        run answers at once.",
    params(PreferAsync),
    request_body = SettleRunRequest,
    responses((status = 200, description = "The plan, and what the run did", body = RebuildSnapshotsResponse), Accepted, ApiErrors),
)]
/// `POST /snapshots/settle`.
pub async fn settle(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<SettleRunRequest>,
) -> Response {
    let mut versions: Vec<String> = Vec::new();
    for id in &req.against {
        match parse_id(id) {
            Ok(id) if !versions.contains(&id.version) => versions.push(id.version),
            Ok(_) => {}
            Err(e) => return e.into_response(),
        }
    }
    let version = match versions.as_slice() {
        [version] => version.clone(),
        [] => {
            let e = ApiError::new(ErrorKind::BadRequest, "against names no snapshot");
            return e.into_response();
        }
        _ => {
            let message = format!("against is of several versions: {}", versions.join(", "));
            return ApiError::new(ErrorKind::BadRequest, message).into_response();
        }
    };
    let plan = match app
        .snapshots
        .version(&version)
        .settle_plan(&req.against, &req.allow)
        .await
    {
        Ok(plan) => plan,
        Err(e) => return ApiError(e).into_response(),
    };
    let creates = rebuild::Creates {
        timeout: req.timeout,
        verify: req.verify,
        retire: false,
    };
    if req.dry_run || !plan.ok {
        return Json(plan).into_response();
    }
    if !prefer.respond_async {
        return Json(rebuild::run_plan(&app, plan, &creates, &version).await).into_response();
    }
    let record = Job::SnapshotSettle(req.clone());
    let held = match app
        .dispatcher
        .hold(JobKind::RebuildSnapshots, &record)
        .await
    {
        Ok(held) => held,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let job = held.token.clone();
    // The run creates in the request's scope.
    tokio::spawn(pisa_db::scope::within(
        pisa_db::scope::current(),
        async move {
            let done = rebuild::run_plan(&app, plan, &creates, &version).await;
            if let Err(e) = held.finish(&JobResult::SnapshotRebuild(Ok(done))).await {
                tracing::warn!("storing a settle's answer failed: {e}");
            }
        },
    ));
    accepted(job, JobKind::RebuildSnapshots, JobStatus::Running)
}

pub(crate) fn resubmitted(done: JobResult) -> Result<ResubmitSnapshotsResponse, ApiError> {
    match done {
        JobResult::SnapshotResubmit(r) => r.map(|r| *r).map_err(ApiError),
        other => Err(unexpected(other)),
    }
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
    let reader = app.snapshots.version(&version);
    Ok(Json(reader.set_labels(&id, &req.labels).await?))
}

#[utoipa::path(
    delete, path = "/snapshots/{id}", operation_id = "deleteSnapshot", tag = "snapshots",
    summary = "Delete a snapshot",
    description = "Its module goes too once no snapshot or other module refers to it: \
        `removed` lists the modules that went. The snapshots that replace it replace what \
        it replaced instead, with the two links composed. With `with_replacers=true` it \
        deletes those snapshots as well, and the ones that replace them, and splices \
        nothing: a whole branch of a run goes, and what the snapshot replaced is a head \
        again if nothing else replaces it. `deleted` lists the snapshots that went, the \
        asked one first.",
    params(("id" = String, Path, description = "The snapshot's id, `<version>~<32 hex>`"), DeleteParams),
    responses((status = 200, description = "What went", body = DeleteSnapshotResponse), ApiErrors),
)]
/// `DELETE /snapshots/{id}`. The query is taken as a `Result` so that the id
/// is checked first.
pub async fn delete(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    q: Result<Params<DeleteParams>, ApiError>,
) -> Result<Json<DeleteSnapshotResponse>, ApiError> {
    let version = parse_id(&id)?.version;
    let Params(DeleteParams { with_replacers }) = q?;
    let job = Job::SnapshotDelete(SnapshotDeleteJob {
        entry: id,
        with_replacers,
    });
    match submit(&app, job, Some(version), Priority::Interactive).await? {
        JobResult::SnapshotDeleted(r) => Ok(Json(r.map_err(ApiError)?)),
        other => Err(unexpected(other)),
    }
}

/// `GET /snapshots/{id}`'s query. Other parameters are ignored.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GetParams {
    /// `declarations` for every field of the check's declarations
    #[serde(default)]
    detail: Option<Detail>,
}

/// `DELETE /snapshots/{id}`'s query. Other parameters are ignored.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteParams {
    /// `true` to delete the snapshots that replace it too, directly or through others; `false` by default
    #[serde(default)]
    with_replacers: bool,
}

/// `POST /snapshots/{id}/verify`'s query. Other parameters are ignored.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct VerifyParams {
    /// Lean's time limit in seconds for each module, once their imports are loaded; 60 by default
    #[serde(default = "default_verify_secs")]
    timeout: u64,
    /// `true` to re-check the modules that are verified already too, which describes their records again; `false` by default
    #[serde(default)]
    again: bool,
}

fn default_verify_secs() -> u64 {
    60
}

#[utoipa::path(
    post, path = "/snapshots/{id}/verify", operation_id = "verifySnapshot", tag = "snapshots",
    summary = "Re-check a snapshot's module with the kernel",
    description = "Re-checks the module and the modules it imports, in one process that loads \
        their imports once; `failure` says which one failed and why. A module verified already \
        is skipped unless `again`, which re-checks it and describes its record again, as a \
        record made before what it now records (`own_axioms`, `inherits`) needs.",
    params(("id" = String, Path, description = "The snapshot's id, `<version>~<32 hex>`"), VerifyParams, PreferAsync),
    responses((status = 200, description = "Whether the module passed", body = VerifySnapshotResponse), Accepted, ApiErrors),
)]
/// The query is taken as a `Result` so that the id is checked first.
pub async fn verify(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    q: Result<Params<VerifyParams>, ApiError>,
    prefer: Prefer,
) -> Response {
    let version = match parse_id(&id) {
        Ok(id) => id.version,
        Err(e) => return e.into_response(),
    };
    let params = match q {
        Ok(Params(params)) => params,
        Err(e) => return e.into_response(),
    };
    let job = Job::SnapshotVerify(SnapshotVerifyJob {
        entry: id,
        timeout_secs: params.timeout,
        again: params.again,
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::api::Refusal;

    /// A worker that failed a create answers in-band, as for `/check`: the
    /// create carries a key, so it can be sent again.
    #[test]
    fn a_failed_create_is_answered_in_band() {
        let failed = JobResult::Failed {
            error: "the worker stopped".into(),
        };
        let Ok(r) = created(failed) else {
            panic!("a failed create is an HTTP error");
        };
        assert_eq!(r.refused, Some(Refusal::Retry));
        let error = r.check.error.unwrap();
        assert_eq!(
            (error.kind, error.message.as_str()),
            (ErrorKind::Internal, "the worker stopped")
        );
    }
}
