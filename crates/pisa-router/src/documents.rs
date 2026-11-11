//! Open document endpoints: goals at positions and candidate tactics probed
//! there, on code a client keeps open on Lean Beam. Only workers with Beam
//! slots run a document's jobs, and while the worker holding it open is live,
//! it runs them all. They are interactive, queued ahead of batch checks.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use pisa_db::queue::LIVE;
use pisa_db::records::Records;
use pisa_protocol::api::{
    ApiErrors, Document, DocumentGoals, DocumentGoalsRequest, DocumentId, DocumentProbes,
    DocumentProbesRequest, EditDocumentRequest, ErrorKind, ErrorResponse, LinePos, MAX_AT,
    MAX_CANDIDATES, MAX_PROBE_BUDGET, OpenDocumentRequest, ProbeAt, default_probe_budget,
};
use pisa_protocol::wire::{
    DocumentEditJob, DocumentGoalsJob, DocumentJob, DocumentOpenJob, DocumentProbeJob, Job,
    JobResult,
};

use crate::dispatch::{Prepared, Priority, Route};
use crate::http::{ApiError, App, Body};
use crate::sessions::unexpected;

fn parse_id(id: &str) -> Result<DocumentId, ApiError> {
    DocumentId::parse(id)
        .ok_or_else(|| ApiError::new(ErrorKind::BadRequest, format!("invalid document id: {id}")))
}

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorKind::BadRequest, message)
}

/// Fails unless a live worker of `version` (of any, for `None`) serves
/// documents: otherwise a job on one would only wait for its time limit.
pub(crate) async fn serving(app: &App, version: Option<&str>) -> Result<(), ApiError> {
    let n = app
        .dispatcher
        .queue()
        .serving_documents(version, LIVE)
        .await
        .map_err(|e| ApiError::new(ErrorKind::Unavailable, e.to_string()))?;
    if n > 0 {
        return Ok(());
    }
    let of = version.map_or(String::new(), |v| format!(" of version {v}"));
    Err(ApiError::new(
        ErrorKind::Unavailable,
        format!("no worker{of} serves documents: workers need Lean Beam slots (PISA_BEAM_SLOTS)"),
    ))
}

/// Runs `job` on document `document` of `version`.
async fn submit(
    app: &App,
    version: Option<String>,
    document: String,
    job: Job,
) -> Result<JobResult, ApiError> {
    serving(app, version.as_deref()).await?;
    let p = Prepared {
        job,
        route: Route {
            version,
            document: Some(document),
            ..Route::default()
        },
        priority: Priority::Interactive,
    };
    Ok(app.dispatcher.submit(p).await?)
}

fn positions(at: &[LinePos]) -> Result<(), ApiError> {
    if at.is_empty() || at.len() > MAX_AT {
        return Err(bad_request(format!(
            "`at` has {} positions; it takes 1 to {MAX_AT}",
            at.len()
        )));
    }
    match at.iter().find(|p| p.line == 0) {
        Some(_) => Err(bad_request("lines count from 1")),
        None => Ok(()),
    }
}

fn document(done: JobResult) -> Result<Json<Document>, ApiError> {
    match done {
        JobResult::Document(r) => r.map(Json).map_err(ApiError),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    post, path = "/documents", operation_id = "openDocument", tag = "documents",
    summary = "Open code for goals and probes",
    description = "Opens the code on Lean Beam, and answers Lean's errors and warnings and the \
        goals of its `sorry`s. The document stays open, for goals at positions and candidate \
        tactics probed there. A worker holds it open, and the code is stored, so another worker \
        reopens it if that one goes; unused, it expires after the workers' `PISA_STATE_TTL_SECS`. Nothing \
        here is a verdict: check finished code with `/check`. A 503 means no worker serves \
        documents.",
    request_body = OpenDocumentRequest,
    responses((status = 200, description = "The open document", body = Document), ApiErrors),
)]
pub async fn open(
    State(app): State<Arc<App>>,
    Body(req): Body<OpenDocumentRequest>,
) -> Result<Json<Document>, ApiError> {
    let (code, runs, version) = match app.resolve_runs(req.version.as_deref(), &req.code).await? {
        Some(r) => (r.code, r.runs, Some(r.version)),
        None => (
            req.code,
            Vec::new(),
            app.versions.resolve(req.version.as_deref()),
        ),
    };
    let id = uuid::Uuid::new_v4().simple().to_string();
    let job = Job::DocumentOpen(DocumentOpenJob {
        document: id.clone(),
        code,
        timeout_secs: req.timeout,
        runs,
    });
    document(submit(&app, version, id, job).await?)
}

#[utoipa::path(
    patch, path = "/documents/{id}", operation_id = "editDocument", tag = "documents",
    summary = "Replace a document's code",
    description = "Opens the new code as the document's next revision. An edit names the \
        revision it replaces: if the document is at another, it is a 409 `conflict`, and \
        nothing changes.",
    params(("id" = String, Path, description = "A document's id")),
    request_body = EditDocumentRequest,
    responses(
        (status = 200, description = "The document at its new revision", body = Document),
        (status = 409, description = "`conflict`: the document is at another revision", body = ErrorResponse),
        ApiErrors,
    ),
)]
pub async fn edit(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Body(req): Body<EditDocumentRequest>,
) -> Result<Json<Document>, ApiError> {
    let id = parse_id(&id)?;
    // Imports by run name resolve as the document's did when it was opened,
    // and new ones afresh.
    let records = Records::new(app.dispatcher.queue().db().clone(), id.version.clone());
    let pins = match records.document(&id.id).await {
        Ok(Some(row)) => row.runs,
        _ => Vec::new(),
    };
    let resolved = app
        .resolve_runs_with(Some(&id.version), &req.code, &pins)
        .await?;
    let (code, runs) = match resolved {
        Some(r) => (r.code, r.runs),
        None => (req.code, Vec::new()),
    };
    let job = Job::DocumentEdit(DocumentEditJob {
        document: id.id.clone(),
        code,
        revision: req.revision,
        timeout_secs: req.timeout,
        runs,
    });
    document(submit(&app, Some(id.version), id.id, job).await?)
}

#[utoipa::path(
    post, path = "/documents/{id}/goals", operation_id = "documentGoals", tag = "documents",
    summary = "The goals at positions in a document",
    description = "The goals before (or after) the tactic at each position, as Lean prints \
        them. A position with only a line is the line's first tactic. There are none in the \
        imports or outside a proof.",
    params(("id" = String, Path, description = "A document's id")),
    request_body = DocumentGoalsRequest,
    responses(
        (status = 200, description = "The goals at each position", body = DocumentGoals),
        (status = 409, description = "`conflict`: the document is at another revision", body = ErrorResponse),
        ApiErrors,
    ),
)]
pub async fn goals(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Body(req): Body<DocumentGoalsRequest>,
) -> Result<Json<DocumentGoals>, ApiError> {
    let id = parse_id(&id)?;
    positions(&req.at)?;
    let job = Job::DocumentGoals(DocumentGoalsJob {
        document: id.id.clone(),
        revision: req.revision,
        at: req.at,
        mode: req.mode,
        timeout_secs: req.timeout,
    });
    match submit(&app, Some(id.version), id.id, job).await? {
        JobResult::DocumentGoals(r) => r.map(Json).map_err(ApiError),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    post, path = "/documents/{id}/probes", operation_id = "probeDocument", tag = "documents",
    summary = "Try candidate tactics at a position",
    description = "Runs each candidate at a `sorry` of the document, or a position in its body, \
        in turn and each within the time limit and what is left of the request's budget, on the \
        document as it is: nothing a candidate does stays. The policy refuses a candidate as it \
        refuses code, and the others still run, in a new Lean Beam if it restarts. \
        `closed` is no verdict: Lean Beam checks neither termination nor what a tactic \
        elaborated to.",
    params(("id" = String, Path, description = "A document's id")),
    request_body = DocumentProbesRequest,
    responses(
        (status = 200, description = "A result per candidate, in order", body = DocumentProbes),
        (status = 409, description = "`conflict`: the document is at another revision", body = ErrorResponse),
        ApiErrors,
    ),
)]
pub async fn probe(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Body(req): Body<DocumentProbesRequest>,
) -> Result<Json<DocumentProbes>, ApiError> {
    let id = parse_id(&id)?;
    if req.candidates.is_empty() || req.candidates.len() > MAX_CANDIDATES {
        return Err(bad_request(format!(
            "`candidates` has {}; it takes 1 to {MAX_CANDIDATES}",
            req.candidates.len()
        )));
    }
    if let ProbeAt::Pos(at) = req.at {
        positions(&[at])?;
    }
    let budget = req.budget.unwrap_or_else(default_probe_budget);
    if budget > MAX_PROBE_BUDGET {
        return Err(bad_request(format!(
            "`budget` is {budget}s; it takes at most {MAX_PROBE_BUDGET}"
        )));
    }
    let job = Job::DocumentProbe(DocumentProbeJob {
        document: id.id.clone(),
        revision: req.revision,
        at: req.at,
        candidates: req.candidates,
        timeout_secs: req.timeout,
        budget_secs: budget,
    });
    match submit(&app, Some(id.version), id.id, job).await? {
        JobResult::DocumentProbes(r) => r.map(Json).map_err(ApiError),
        other => Err(unexpected(other)),
    }
}

#[utoipa::path(
    delete, path = "/documents/{id}", operation_id = "closeDocument", tag = "documents",
    summary = "Close a document",
    description = "Closes the document and deletes its code.",
    params(("id" = String, Path, description = "A document's id")),
    responses((status = 204, description = "Closed"), ApiErrors),
)]
pub async fn close(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let job = Job::DocumentClose(DocumentJob {
        document: id.id.clone(),
    });
    match submit(&app, Some(id.version), id.id, job).await? {
        JobResult::DocumentClosed(r) => r.map(|()| StatusCode::NO_CONTENT).map_err(ApiError),
        other => Err(unexpected(other)),
    }
}
