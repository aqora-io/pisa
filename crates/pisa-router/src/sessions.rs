//! Tactic session endpoints. Session work is interactive, so it is queued
//! ahead of batch checks. A session is an open document on Lean Beam, and a
//! state's id names its version and its session's document, so a tactic goes
//! to the worker holding the document without a lookup. The final check runs
//! on the REPL, as checks do.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use pisa_protocol::api::{
    Accepted, ApiErrors, ApplyTacticRequest, ApplyTacticResponse, CheckResult, CheckSessionRequest,
    DocumentId, ErrorKind, JobKind, Key, PreferAsync, Session, StartSessionRequest,
};
use pisa_protocol::header::split_header;
use pisa_protocol::wire::{Job, JobResult, SessionCheckJob, SessionStartJob, SessionTacticJob};

use crate::dispatch::{Prepared, Priority, Route};
use crate::documents::serving;
use crate::http::{ApiError, App, Body, failed_result};
use crate::jobs::Prefer;

fn parse_key(id: &str) -> Result<Key, ApiError> {
    Key::parse(id).ok_or_else(|| ApiError::new(ErrorKind::BadRequest, format!("invalid key: {id}")))
}

fn parse_session(id: &str) -> Result<DocumentId, ApiError> {
    DocumentId::parse(id)
        .ok_or_else(|| ApiError::new(ErrorKind::BadRequest, format!("invalid session id: {id}")))
}

pub(crate) fn unexpected(result: JobResult) -> ApiError {
    let message = match result {
        JobResult::Failed { error } => error,
        other => format!("unexpected worker result: {other:?}"),
    };
    ApiError::new(ErrorKind::Internal, message)
}

#[utoipa::path(
    post, path = "/sessions", operation_id = "startSession", tag = "sessions",
    summary = "Start a proof session",
    description = "Opens code with `sorry`s on Lean Beam and returns a state for each, to apply \
        tactics to. A tactic runs in the code's own context, so it can use what the code \
        declares before its `sorry`. The session's id is also its document's, for \
        `/documents/{id}/goals` and `/probes`. A 503 means no worker serves documents.",
    params(PreferAsync),
    request_body = StartSessionRequest,
    responses((status = 200, description = "The session, with a state for each `sorry`", body = Session), Accepted, ApiErrors),
)]
pub async fn start(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<StartSessionRequest>,
) -> Response {
    let version = app.versions.resolve(req.version.as_deref());
    if let Err(e) = serving(&app, version.as_deref()).await {
        return e.into_response();
    }
    let document = uuid::Uuid::new_v4().simple().to_string();
    let route = Route {
        version,
        document: Some(document.clone()),
        ..Route::default()
    };
    let job = Job::SessionStart(SessionStartJob {
        document,
        code: req.code,
        timeout_secs: req.timeout,
    });
    let p = Prepared {
        job,
        route,
        priority: Priority::Interactive,
    };
    app.answer(prefer, JobKind::StartSession, p).await
}

pub(crate) fn started(done: JobResult) -> Result<Session, ApiError> {
    let JobResult::SessionStarted(result) = done else {
        return Err(unexpected(done));
    };
    result.map_err(ApiError)
}

#[utoipa::path(
    post, path = "/states/{id}/tactic", operation_id = "applyTactic", tag = "sessions",
    summary = "Apply a tactic to a state",
    description = "Returns the new state. Earlier states stay valid, so a session is a tree, and \
        the same tactic on the same state gives the same state. A tactic that fails is a 422 \
        `lean`, with Lean's errors in its message. When Lean Beam itself fails, the state is \
        replayed and the tactic run once more, and a second failure is a 503 `unavailable`. A \
        worker that doesn't hold the session's document replays the steps from the `sorry`, in \
        milliseconds each.",
    params(("id" = String, Path, description = "A state's id, from a session or an earlier tactic"), PreferAsync),
    request_body = ApplyTacticRequest,
    responses((status = 200, description = "The new state", body = ApplyTacticResponse), Accepted, ApiErrors),
)]
pub async fn apply_tactic(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    prefer: Prefer,
    Body(req): Body<ApplyTacticRequest>,
) -> Response {
    let key = match parse_key(&id) {
        Ok(key) => key,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = serving(&app, Some(&key.version)).await {
        return e.into_response();
    }
    let route = Route {
        version: Some(key.version),
        document: Some(key.session),
        ..Route::default()
    };
    let job = Job::SessionTactic(SessionTacticJob {
        state: id,
        tactic: req.tactic,
        timeout_secs: req.timeout,
    });
    let p = Prepared {
        job,
        route,
        priority: Priority::Interactive,
    };
    app.answer(prefer, JobKind::ApplyTactic, p).await
}

pub(crate) fn applied(done: JobResult) -> Result<ApplyTacticResponse, ApiError> {
    let JobResult::TacticApplied(result) = done else {
        return Err(unexpected(done));
    };
    result.map_err(ApiError)
}

#[utoipa::path(
    post, path = "/sessions/{id}/check", operation_id = "checkSession", tag = "sessions",
    summary = "Check a session's finished proof",
    description = "Re-checks the completed code from scratch and confirms it proves the \
        statements the session started from, with the same imports. Failures are in-band, \
        as for `/check`, with statement changes in `reasons`.",
    params(("id" = String, Path, description = "A session's id"), PreferAsync),
    request_body = CheckSessionRequest,
    responses((status = 200, description = "The verdict", body = CheckResult), Accepted, ApiErrors),
)]
pub async fn check(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    prefer: Prefer,
    Body(req): Body<CheckSessionRequest>,
) -> Response {
    let session = match parse_session(&id) {
        Ok(session) => session,
        Err(e) => return e.into_response(),
    };
    let route = Route {
        version: Some(session.version),
        header_key: Some(split_header(&req.code).header_key),
        ..Route::default()
    };
    let job = Job::SessionCheck(SessionCheckJob {
        session: id,
        code: req.code,
        timeout_secs: req.timeout,
        detail: req.detail,
    });
    let p = Prepared {
        job,
        route,
        priority: Priority::Interactive,
    };
    app.answer(prefer, JobKind::CheckSession, p).await
}

/// The completed proof's check result, with statement changes added to
/// `reasons`. A bad or unknown session id is an HTTP error; everything else
/// is in-band, as for `/check`.
pub(crate) fn checked(done: JobResult) -> Result<CheckResult, ApiError> {
    let JobResult::SessionChecked(checked) = done else {
        return Ok(failed_result(done));
    };
    let about_the_request = |e: &&pisa_protocol::api::Error| {
        matches!(e.kind, ErrorKind::NotFound | ErrorKind::BadRequest)
    };
    if let Some(e) = checked.outcome.error.as_ref().filter(about_the_request) {
        return Err(ApiError(e.clone()));
    }
    let mut result = CheckResult::from(checked.outcome);
    result.ok &= checked.statement_reasons.is_empty();
    result.reasons.extend(checked.statement_reasons);
    result.changes.extend(checked.statement_changes);
    Ok(result)
}
