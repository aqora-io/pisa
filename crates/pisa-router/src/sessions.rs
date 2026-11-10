//! Tactic session endpoints. Session work is interactive, so it is queued
//! ahead of batch checks. Each state key names its version, header and hash;
//! the worker that stored a state gets first refusal of the next tactic on
//! it, which then needn't load it from storage.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use pisa_protocol::api::{
    Accepted, ApiErrors, ApplyTacticRequest, ApplyTacticResponse, CheckResult, CheckSessionRequest,
    ErrorKind, JobKind, Key, PreferAsync, Session, StartSessionRequest,
};
use pisa_protocol::header::split_header;
use pisa_protocol::wire::{Job, JobResult, SessionCheckJob, SessionStartJob, SessionTacticJob};

use crate::dispatch::{Prepared, Priority, Route};
use crate::http::{ApiError, App, Body, failed_result, prewarm_header};
use crate::jobs::Prefer;

fn parse_key(id: &str) -> Result<Key, ApiError> {
    Key::parse(id).ok_or_else(|| ApiError::new(ErrorKind::BadRequest, format!("invalid key: {id}")))
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
    description = "Checks code with `sorry`s and returns a state for each, to apply tactics to. \
        Each state starts from the imports alone: a tactic can't use what the code declares \
        before its `sorry`, which a warning in `messages` points out. Declarations to use go in \
        a snapshot that the code imports.",
    params(PreferAsync),
    request_body = StartSessionRequest,
    responses((status = 200, description = "The session, with a state for each `sorry`", body = Session), Accepted, ApiErrors),
)]
pub async fn start(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<StartSessionRequest>,
) -> Response {
    let split = split_header(&req.code);
    let route = Route {
        version: app.versions.resolve(req.version.as_deref()),
        header_key: Some(split.header_key),
        header: prewarm_header(split.header),
        state: None,
    };
    let job = Job::SessionStart(SessionStartJob {
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
    description = "Returns the new state. Earlier states stay valid, so a session is a tree. \
        A tactic that doesn't parse is a 422 `lean`; an error Lean recovers from, like an \
        unknown identifier, is in `messages`.",
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
    let route = Route {
        version: Some(key.version),
        header_key: Some(key.header_key),
        header: None,
        state: Some(key.id),
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
    let key = match parse_key(&id) {
        Ok(key) => key,
        Err(e) => return e.into_response(),
    };
    let route = Route {
        version: Some(key.version),
        header_key: Some(key.header_key),
        header: None,
        state: None,
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
