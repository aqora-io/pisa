//! Tactic session endpoints. Session work is interactive, so it is queued
//! ahead of batch checks. Each state key names its version, header and hash;
//! the worker that stored a state gets first refusal of the next tactic on
//! it, which then needn't load it from storage.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use pisa_protocol::api::{
    CheckResult, ErrorKind, Key, SessionCheckRequest, StartRequest, StartResponse, TacticRequest,
    TacticResponse,
};
use pisa_protocol::header::split_header;
use pisa_protocol::wire::{Job, JobResult, SessionCheckJob, SessionStartJob, SessionTacticJob};

use crate::dispatch::{Priority, Route};
use crate::http::{ApiError, App, Body, failed_result, prewarm_header};

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

pub async fn start(
    State(app): State<Arc<App>>,
    Body(req): Body<StartRequest>,
) -> Result<Json<StartResponse>, ApiError> {
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
    let done = app
        .dispatcher
        .submit(job, route, Priority::Interactive)
        .await?;
    let JobResult::SessionStarted(result) = done else {
        return Err(unexpected(done));
    };
    Ok(Json(result.map_err(ApiError)?))
}

pub async fn tactic(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Body(req): Body<TacticRequest>,
) -> Result<Json<TacticResponse>, ApiError> {
    let key = parse_key(&id)?;
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
    let done = app
        .dispatcher
        .submit(job, route, Priority::Interactive)
        .await?;
    let JobResult::TacticApplied(result) = done else {
        return Err(unexpected(done));
    };
    Ok(Json(result.map_err(ApiError)?))
}

/// The completed proof's check result, with statement changes added to
/// `reasons`. A bad or unknown session ID is an HTTP error; everything else
/// is in-band, as for `/check`.
pub async fn check(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Body(req): Body<SessionCheckRequest>,
) -> Result<Json<CheckResult>, ApiError> {
    let key = parse_key(&id)?;
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
    let done = app
        .dispatcher
        .submit(job, route, Priority::Interactive)
        .await?;
    let JobResult::SessionChecked(checked) = done else {
        return Ok(Json(failed_result(done)));
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
    Ok(Json(result))
}
