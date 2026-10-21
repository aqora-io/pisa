//! Tactic session endpoints. Session work is interactive, so it is queued
//! ahead of batch checks. Each state key names its version and header, and the
//! router remembers which worker last held each state so the next tactic can
//! skip loading it from storage.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use pisa_protocol::header::split_header;
use pisa_protocol::session::{
    CheckRequest, CheckResponse, Key, StartRequest, StartResponse, TacticRequest, TacticResponse,
};
use pisa_protocol::wire::{Job, JobResult, SessionCheckJob, SessionStartJob, SessionTacticJob};

use crate::dispatch::{Priority, Route};
use crate::http::{ApiError, App, check_result, failed_result, record_rejection};
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
    Key::parse(id).ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("invalid key: {id}")))
}

fn unexpected(result: JobResult) -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("unexpected worker result: {result:?}"),
    )
}

fn count(kind: &'static str, error: bool) {
    let outcome = if error { "error" } else { "ok" };
    metrics::counter!(m::JOBS, "kind" => kind, "outcome" => outcome).increment(1);
}

pub async fn start(
    State(app): State<Arc<App>>,
    Json(req): Json<StartRequest>,
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
    match done.result {
        JobResult::SessionStarted(r) => {
            count("session_start", r.error.is_some());
            app.affinity
                .record(r.states.iter().map(|s| &s.state), &done.worker);
            Ok(Json(r))
        }
        JobResult::Failed { error } => Ok(Json(StartResponse {
            error: Some(error),
            ..Default::default()
        })),
        other => Err(unexpected(other)),
    }
}

pub async fn tactic(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(req): Json<TacticRequest>,
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
    match done.result {
        JobResult::TacticApplied(r) => {
            count("tactic", r.error.is_some());
            app.affinity.record(&r.state, &done.worker);
            Ok(Json(r))
        }
        JobResult::Failed { error } => Ok(Json(TacticResponse {
            error: Some(error),
            ..Default::default()
        })),
        other => Err(unexpected(other)),
    }
}

pub async fn check(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(req): Json<CheckRequest>,
) -> Result<Json<CheckResponse>, ApiError> {
    let key = parse_key(&id)?;
    let route = Route {
        version: Some(key.version),
        header_key: Some(key.header_key),
        header: None,
        affinity: None,
    };
    let job = Job::SessionCheck(SessionCheckJob {
        session: id.clone(),
        code: req.code,
        timeout_secs: req.timeout,
    });
    let done = app
        .dispatcher
        .submit(job, route, Priority::Interactive)
        .await?;
    let (result, statement_reasons) = match done.result {
        JobResult::SessionChecked(c) => (
            check_result(id, c.outcome, "session_check"),
            c.statement_reasons,
        ),
        JobResult::Failed { error } => (failed_result(id, error, "session_check"), Vec::new()),
        other => return Err(unexpected(other)),
    };
    statement_reasons.iter().for_each(|r| record_rejection(r));
    let mut reasons = result
        .verification
        .as_ref()
        .map(|v| v.reasons.clone())
        .unwrap_or_default();
    reasons.extend(statement_reasons);
    if let Some(e) = &result.error {
        reasons.push(format!("error: {e}"));
    }
    let verified = result.verification.as_ref().is_some_and(|v| v.ok);
    Ok(Json(CheckResponse {
        ok: verified && reasons.is_empty(),
        reasons,
        result,
    }))
}
