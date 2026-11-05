//! Tactic session endpoints. Session work is interactive, so it is queued
//! ahead of batch checks. Each state key names its version, header and hash;
//! the worker that stored a state gets first refusal of the next tactic on
//! it, which then needn't load it from storage.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use pisa_protocol::api::{
    Accepted, ApiErrors, CheckResult, ErrorKind, JobKind, Key, PreferAsync, SessionCheckRequest,
    StartRequest, StartResponse, TacticRequest, TacticResponse,
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
    description = "Checks code with `sorry`s and returns a state for each, to apply tactics to.",
    params(PreferAsync),
    request_body = StartRequest,
    responses((status = 200, description = "The session, with a state for each `sorry`", body = StartResponse), Accepted, ApiErrors),
)]
pub async fn start(
    State(app): State<Arc<App>>,
    prefer: Prefer,
    Body(req): Body<StartRequest>,
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

pub(crate) fn started(done: JobResult) -> Result<StartResponse, ApiError> {
    let JobResult::SessionStarted(result) = done else {
        return Err(unexpected(done));
    };
    result.map_err(ApiError)
}

#[utoipa::path(
    post, path = "/states/{id}/tactic", operation_id = "tactic", tag = "sessions",
    summary = "Apply a tactic to a state",
    description = "Returns the new state. Earlier states stay valid, so a session is a tree. \
        A tactic that doesn't parse is a 422 `lean`; an error Lean recovers from, like an \
        unknown identifier, is in `messages`.",
    params(("id" = String, Path, description = "A state key, from a session or an earlier tactic"), PreferAsync),
    request_body = TacticRequest,
    responses((status = 200, description = "The new state", body = TacticResponse), Accepted, ApiErrors),
)]
pub async fn tactic(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    prefer: Prefer,
    Body(req): Body<TacticRequest>,
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
    app.answer(prefer, JobKind::Tactic, p).await
}

pub(crate) fn applied(done: JobResult) -> Result<TacticResponse, ApiError> {
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
    params(("id" = String, Path, description = "The session ID"), PreferAsync),
    request_body = SessionCheckRequest,
    responses((status = 200, description = "The verdict", body = CheckResult), Accepted, ApiErrors),
)]
pub async fn check(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    prefer: Prefer,
    Body(req): Body<SessionCheckRequest>,
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
    });
    let p = Prepared {
        job,
        route,
        priority: Priority::Interactive,
    };
    app.answer(prefer, JobKind::CheckSession, p).await
}

/// The completed proof's check result, with statement changes added to
/// `reasons`. A bad or unknown session ID is an HTTP error; everything else
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
    Ok(result)
}
