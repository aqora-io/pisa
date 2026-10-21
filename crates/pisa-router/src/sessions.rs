//! Tactic session endpoints. Session work is interactive, so it is queued
//! ahead of batch checks. Each state key names its version and header, and the
//! router remembers which worker last held each state so the next tactic can
//! skip loading it from storage.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, State};
use pisa_protocol::api::{
    CheckResult, ErrorKind, Key, SessionCheckRequest, StartRequest, StartResponse, TacticRequest,
    TacticResponse,
};
use pisa_protocol::header::split_header;
use pisa_protocol::wire::{Job, JobResult, SessionCheckJob, SessionStartJob, SessionTacticJob};

use crate::dispatch::{Priority, Route};
use crate::http::{ApiError, App, Body, check_result, failed_result, record_rejection};
use crate::metrics as m;

/// Bound on remembered state locations; the map is reset when it fills up.
const AFFINITY_CAP: usize = 100_000;

/// State key → name of the worker that last produced or loaded it.
#[derive(Default)]
pub struct Affinity(Mutex<HashMap<String, String>>);

impl Affinity {
    fn get(&self, state: &str) -> Option<String> {
        self.0.lock().unwrap().get(state).cloned()
    }

    fn record<'a>(&self, states: impl IntoIterator<Item = &'a String>, worker: &str) {
        let mut map = self.0.lock().unwrap();
        for s in states {
            if map.len() >= AFFINITY_CAP {
                map.clear();
            }
            map.insert(s.clone(), worker.to_string());
        }
    }
}

fn parse_key(id: &str) -> Result<Key, ApiError> {
    Key::parse(id).ok_or_else(|| ApiError::new(ErrorKind::BadRequest, format!("invalid key: {id}")))
}

fn unexpected(result: JobResult) -> ApiError {
    let message = match result {
        JobResult::Failed { error } => error,
        other => format!("unexpected worker result: {other:?}"),
    };
    ApiError::new(ErrorKind::Internal, message)
}

fn count(kind: &'static str, error: bool) {
    let outcome = if error { "error" } else { "ok" };
    metrics::counter!(m::JOBS, "kind" => kind, "outcome" => outcome).increment(1);
}

pub async fn start(
    State(app): State<Arc<App>>,
    Body(req): Body<StartRequest>,
) -> Result<Json<StartResponse>, ApiError> {
    let split = split_header(&req.code);
    let route = Route {
        version: app.versions.resolve(req.version.as_deref()),
        header_key: Some(split.header_key),
        header: Some(split.header),
        affinity: None,
    };
    let job = Job::SessionStart(SessionStartJob {
        code: req.code,
        timeout_secs: req.timeout,
    });
    let done = app
        .dispatcher
        .submit(job, route, Priority::Interactive)
        .await?;
    let JobResult::SessionStarted(result) = done.result else {
        return Err(unexpected(done.result));
    };
    count("session_start", result.is_err());
    let r = result.map_err(ApiError)?;
    app.affinity
        .record(r.states.iter().map(|s| &s.state), &done.worker);
    Ok(Json(r))
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
        affinity: app.affinity.get(&id),
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
    let JobResult::TacticApplied(result) = done.result else {
        return Err(unexpected(done.result));
    };
    count("tactic", result.is_err());
    let r = result.map_err(ApiError)?;
    app.affinity.record([&r.state], &done.worker);
    Ok(Json(r))
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
        affinity: None,
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
    let JobResult::SessionChecked(checked) = done.result else {
        return Ok(Json(failed_result(done.result, "session_check")));
    };
    let about_the_request = |e: &&pisa_protocol::api::Error| {
        matches!(e.kind, ErrorKind::NotFound | ErrorKind::BadRequest)
    };
    if let Some(e) = checked.outcome.error.as_ref().filter(about_the_request) {
        return Err(ApiError(e.clone()));
    }
    let mut result = check_result(checked.outcome, "session_check");
    checked
        .statement_reasons
        .iter()
        .for_each(|r| record_rejection(r));
    result.ok &= checked.statement_reasons.is_empty();
    result.reasons.extend(checked.statement_reasons);
    Ok(Json(result))
}
