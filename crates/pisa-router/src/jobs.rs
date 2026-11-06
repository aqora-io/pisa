//! Async requests. An endpoint that may run long, asked with
//! `Prefer: respond-async`, queues its job and answers 202 with it at once;
//! `/jobs/{id}` then answers, for an hour after the job is done, as the
//! request would have. Without the header the endpoint waits, as before.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::header::{CACHE_CONTROL, LOCATION};
use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use pisa_protocol::api::{ApiErrors, ErrorKind, Job, JobKind, JobOutcome, JobStatus, JobWait};
use pisa_protocol::wire::JobResult;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::dispatch::{DispatchError, Looked, Looking, Prepared};
use crate::http::{ApiError, App, check_result};
use crate::{sessions, snapshots};

/// The longest `/jobs/{id}` waits for a job to finish.
const MAX_WAIT: Duration = Duration::from_secs(60);

const PREFERENCE_APPLIED: HeaderName = HeaderName::from_static("preference-applied");

/// Whether the request's `Prefer` headers ask for `respond-async`.
pub struct Prefer {
    pub respond_async: bool,
}

impl<S: Send + Sync> FromRequestParts<S> for Prefer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let respond_async = parts
            .headers
            .get_all("prefer")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(prefers_async);
        Ok(Prefer { respond_async })
    }
}

/// Whether a `Prefer` header's value has the `respond-async` preference,
/// among others and whatever its parameters (RFC 7240).
fn prefers_async(value: &str) -> bool {
    value.split(',').any(|preference| {
        let name = preference.split([';', '=']).next().unwrap_or_default();
        name.trim().eq_ignore_ascii_case("respond-async")
    })
}

impl App {
    /// Runs `p` and answers as the endpoint does or, if the request prefers
    /// it, queues it as `kind` and answers 202 with its job.
    pub(crate) async fn answer(&self, prefer: Prefer, kind: JobKind, p: Prepared) -> Response {
        if !prefer.respond_async {
            return respond(kind, self.dispatcher.submit(p).await);
        }
        match self.dispatcher.submit_async(kind, p).await {
            Ok(id) => accepted(id, kind, JobStatus::Queued),
            Err(e) => ApiError::from(e).into_response(),
        }
    }
}

/// The 202 to a request that prefers `respond-async`: its job.
pub(crate) fn accepted(id: String, kind: JobKind, status: JobStatus) -> Response {
    let location = format!("/jobs/{id}");
    let job = Job { id, kind, status };
    let mut response = no_store((StatusCode::ACCEPTED, Json(job)));
    let headers = response.headers_mut();
    if let Ok(location) = HeaderValue::from_str(&location) {
        headers.insert(LOCATION, location);
    }
    headers.insert(
        PREFERENCE_APPLIED,
        HeaderValue::from_static("respond-async"),
    );
    response
}

/// What a request of `kind` answers, given its job's outcome.
pub(crate) fn respond(kind: JobKind, outcome: Result<JobResult, DispatchError>) -> Response {
    let outcome = outcome.map_err(ApiError::from);
    match kind {
        JobKind::Check => answer(outcome.map(check_result)),
        JobKind::StartSession => answer(outcome.and_then(sessions::started)),
        JobKind::ApplyTactic => answer(outcome.and_then(sessions::applied)),
        JobKind::CheckSession => answer(outcome.and_then(sessions::checked)),
        JobKind::CreateSnapshot => answer(outcome.and_then(snapshots::created)),
        JobKind::VerifySnapshot => answer(outcome.and_then(snapshots::verified)),
        JobKind::RebuildSnapshots => answer(outcome.and_then(snapshots::rebuilt)),
    }
}

fn answer<T: Serialize>(answer: Result<T, ApiError>) -> Response {
    match answer {
        Ok(body) => Json(body).into_response(),
        Err(e) => e.into_response(),
    }
}

/// A job's answer is its own, and its status changes: not for caches.
fn no_store(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    let value = HeaderValue::from_static("no-store");
    response.headers_mut().insert(CACHE_CONTROL, value);
    response
}

/// A query whose rejections use the API's error shape.
pub struct Params<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for Params<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(v)) => Ok(Params(v)),
            Err(rejection) => Err(ApiError::new(ErrorKind::BadRequest, rejection.body_text())),
        }
    }
}

fn unknown(id: &str) -> Response {
    let message = format!("unknown job {id}: it never was, or it has expired");
    no_store(ApiError::new(ErrorKind::NotFound, message))
}

/// Whether `id` can be a job's id: 32 hex digits.
fn is_token(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[utoipa::path(
    get, path = "/jobs/{id}", operation_id = "getJob", tag = "jobs",
    summary = "An async request's answer",
    description = "Waits up to `wait` seconds for the job to finish. Done, it answers as its \
        request would have, with the same status; an error is the request's own. Until then \
        it answers 202. A job's answer is kept for an hour after it is done.",
    params(("id" = String, Path, description = "The job's id, from the 202"), JobWait),
    responses(
        (status = 200, description = "Done: the request's answer", body = JobOutcome),
        (status = 202, description = "Not done yet", body = Job),
        ApiErrors,
    ),
)]
pub async fn get(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    q: Result<Params<JobWait>, ApiError>,
) -> Response {
    if !is_token(&id) {
        return unknown(&id);
    }
    let Params(q) = match q {
        Ok(q) => q,
        Err(e) => return no_store(e),
    };
    let wait = Duration::from_secs(q.wait.unwrap_or(0)).min(MAX_WAIT);
    match app.dispatcher.look(&id, wait).await {
        Ok(None) => unknown(&id),
        Ok(Some(Looked { kind, state })) => match state {
            Looking::Pending(status) => {
                let job = Job { id, kind, status };
                no_store((StatusCode::ACCEPTED, Json(job)))
            }
            Looking::Done(outcome) => no_store(respond(kind, *outcome)),
        },
        Err(e) => no_store(ApiError::from(e)),
    }
}

#[utoipa::path(
    delete, path = "/jobs/{id}", operation_id = "cancelJob", tag = "jobs",
    summary = "Cancel an async request",
    description = "Withdraws the job if no worker has started it (`cancelled`); one that is \
        `running` or `done` is left as it is.",
    params(("id" = String, Path, description = "The job's id, from the 202")),
    responses((status = 200, description = "What became of it", body = Job), ApiErrors),
)]
pub async fn cancel(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    if !is_token(&id) {
        return unknown(&id);
    }
    match app.dispatcher.cancel(&id).await {
        Ok(None) => unknown(&id),
        Ok(Some((kind, status))) => no_store(Json(Job { id, kind, status })),
        Err(e) => no_store(ApiError::from(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respond_async_is_found_among_preferences() {
        assert!(prefers_async("respond-async"));
        assert!(prefers_async("Respond-Async; x=1"));
        assert!(prefers_async("wait=5, respond-async"));
        assert!(!prefers_async("respond-asyncx"));
        assert!(!prefers_async("return=minimal"));
    }
}
