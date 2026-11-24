//! Async requests. An endpoint that may run long, asked with
//! `Prefer: respond-async`, queues its job and answers 202 with it at once;
//! `/jobs/{id}` then answers, for an hour after the job is done (a day for a
//! snapshot's create and a run's acceptance), with the
//! status and the answer the request would have had. Without the header the
//! endpoint waits for the answer.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::header::{CACHE_CONTROL, LOCATION};
use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use pisa_db::queue::ExpiredOutcome;
use pisa_protocol::api::{
    ApiErrors, ErrorKind, ErrorResponse, GetJobResponse, Job, JobKind, JobStatus, JobWait,
};
use pisa_protocol::wire::JobResult;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::dispatch::{DispatchError, ExpiredJob, Looked, Looking, Prepared, Withdrawn, kind_name};
use crate::http::{ApiError, App, check_result};
use crate::{sessions, snapshots};

/// The longest `/jobs/{id}` waits for a job to finish.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// How long past its `wait` a look may take reading the database, before it
/// answers the last status it read.
pub const POLL_MARGIN: Duration = Duration::from_secs(5);

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
    let job = Job {
        id,
        kind,
        status,
        progress: None,
    };
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
    match answer(kind, outcome) {
        Ok(body) => Json(body).into_response(),
        Err(e) => e.into_response(),
    }
}

/// A done job's answer: the status and the answer its request would have
/// had.
fn done(id: String, kind: JobKind, outcome: Result<JobResult, DispatchError>) -> Response {
    let (http_status, body) = match answer(kind, outcome) {
        Ok(body) => (StatusCode::OK.as_u16(), body),
        Err(ApiError(error)) => {
            let status = error.kind.status();
            let body = serde_json::to_value(ErrorResponse { error }).unwrap_or_default();
            (status, body)
        }
    };
    let done = GetJobResponse {
        id,
        kind,
        status: JobStatus::Done,
        http_status,
        body,
    };
    no_store(Json(done))
}

/// What a request of `kind` answers, given its job's outcome, or its error.
fn answer(
    kind: JobKind,
    outcome: Result<JobResult, DispatchError>,
) -> Result<serde_json::Value, ApiError> {
    let outcome = outcome.map_err(ApiError::from);
    match kind {
        JobKind::Check => json(outcome.map(check_result)),
        JobKind::StartSession => json(outcome.and_then(sessions::started)),
        JobKind::ApplyTactic => json(outcome.and_then(sessions::applied)),
        JobKind::CheckSession => json(outcome.and_then(sessions::checked)),
        JobKind::CreateSnapshot => json(outcome.and_then(snapshots::created)),
        JobKind::VerifySnapshot => json(outcome.and_then(snapshots::verified)),
        JobKind::RebuildSnapshots => json(outcome.and_then(snapshots::rebuilt)),
        JobKind::AcceptRun => json(outcome.and_then(snapshots::judged)),
        JobKind::ResubmitSnapshots => json(outcome.and_then(snapshots::resubmitted)),
    }
}

fn json<T: Serialize>(answer: Result<T, ApiError>) -> Result<serde_json::Value, ApiError> {
    serde_json::to_value(answer?).map_err(|e| ApiError::new(ErrorKind::Internal, e.to_string()))
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

/// A job in the caller's scope that is gone, but that a tombstone says was
/// there: what it was and how it ended, and where to read how to get it
/// back.
fn expired(id: &str, kind: JobKind, job: ExpiredJob) -> Response {
    no_store(ApiError::new(
        ErrorKind::NotFound,
        expired_message(id, kind, job),
    ))
}

fn expired_message(id: &str, kind: JobKind, job: ExpiredJob) -> String {
    let at = rfc3339(job.finished_micros);
    let capacity = ErrorKind::Capacity.status();
    let ended = match (job.outcome, job.http_status) {
        (ExpiredOutcome::Done, Some(status)) => format!("answered {status} at {at}"),
        (ExpiredOutcome::Done, None) => format!("answered at {at}"),
        (ExpiredOutcome::Lost, Some(status)) if status == capacity => {
            format!("was given up at {at}, as no worker took it")
        }
        (ExpiredOutcome::Lost, _) => format!("was lost at {at}"),
        (ExpiredOutcome::Cancelled, _) => format!("was cancelled at {at}"),
    };
    let kind = kind_name(kind);
    let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    format!(
        "job {id} expired: {article} {kind} that {ended}; see docs/api.md, Recovering a lost job"
    )
}

/// `micros` since the Unix epoch, as RFC 3339 in UTC.
fn rfc3339(micros: i64) -> String {
    let seconds = micros.div_euclid(1_000_000);
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Whether `id` can be a job's id: 32 hex digits.
fn is_token(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[utoipa::path(
    get, path = "/jobs/{id}", operation_id = "getJob", tag = "jobs",
    summary = "An async request's answer",
    description = "Waits up to `wait` seconds for the job to finish. Done, it answers 200 with \
        the status its request would have answered with (`http_status`) and that answer \
        (`body`), an error included, e.g. a 429 when no worker took it in time. Until then it \
        answers 202. It answers within `wait` and five seconds even if the database does \
        not: the last status it read, else a 503. A 404 or a 503 is the look's own. A job's \
        answer is kept for an hour after it is done, a snapshot's create and a run's acceptance \
        for a day. After that, in the scope it was made in, the 404 says what the job was and how \
        it ended, for 30 days; see docs/api.md, Recovering a lost job.",
    params(("id" = String, Path, description = "The job's id, from the 202"), JobWait),
    responses(
        (status = 200, description = "Done: the request's status and answer", body = GetJobResponse),
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
            Looking::Pending(status, progress) => {
                let job = Job {
                    id,
                    kind,
                    status,
                    progress,
                };
                no_store((StatusCode::ACCEPTED, Json(job)))
            }
            Looking::Done(outcome) => done(id, kind, *outcome),
            Looking::Expired(job) => expired(&id, kind, job),
        },
        Err(e) => no_store(ApiError::from(e)),
    }
}

#[utoipa::path(
    delete, path = "/jobs/{id}", operation_id = "cancelJob", tag = "jobs",
    summary = "Cancel an async request",
    description = "Withdraws the job if no worker has started it (`cancelled`); one that is \
        `running` or `done` is left as it is. An expired job is the same 404 as `GET`.",
    params(("id" = String, Path, description = "The job's id, from the 202")),
    responses((status = 200, description = "What became of it", body = Job), ApiErrors),
)]
pub async fn cancel(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    if !is_token(&id) {
        return unknown(&id);
    }
    match app.dispatcher.cancel(&id).await {
        Ok(None) => unknown(&id),
        Ok(Some((kind, Withdrawn::Status(status)))) => no_store(Json(Job {
            id,
            kind,
            status,
            progress: None,
        })),
        Ok(Some((kind, Withdrawn::Expired(job)))) => expired(&id, kind, job),
        Err(e) => no_store(ApiError::from(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_rfc_3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_773_750_245_000_000), "2026-03-17T12:24:05Z");
        assert_eq!(rfc3339(951_782_400_000_000), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn an_expired_job_says_what_it_was_and_how_it_ended() {
        let says = |kind, outcome, http_status| {
            let job = ExpiredJob {
                outcome,
                http_status,
                finished_micros: 1_773_750_245_000_000,
            };
            expired_message("abc", kind, job)
        };
        let see = "; see docs/api.md, Recovering a lost job";
        assert_eq!(
            says(JobKind::CreateSnapshot, ExpiredOutcome::Done, Some(200)),
            format!(
                "job abc expired: a create_snapshot that answered 200 at 2026-03-17T12:24:05Z{see}"
            )
        );
        assert_eq!(
            says(JobKind::AcceptRun, ExpiredOutcome::Done, Some(200)),
            format!(
                "job abc expired: an accept_run that answered 200 at 2026-03-17T12:24:05Z{see}"
            )
        );
        assert_eq!(
            says(JobKind::Check, ExpiredOutcome::Lost, Some(429)),
            format!(
                "job abc expired: a check that was given up at 2026-03-17T12:24:05Z, as no \
                 worker took it{see}"
            )
        );
        assert_eq!(
            says(JobKind::Check, ExpiredOutcome::Lost, Some(503)),
            format!("job abc expired: a check that was lost at 2026-03-17T12:24:05Z{see}")
        );
        assert_eq!(
            says(JobKind::Check, ExpiredOutcome::Cancelled, None),
            format!("job abc expired: a check that was cancelled at 2026-03-17T12:24:05Z{see}")
        );
    }

    #[test]
    fn respond_async_is_found_among_preferences() {
        assert!(prefers_async("respond-async"));
        assert!(prefers_async("Respond-Async; x=1"));
        assert!(prefers_async("wait=5, respond-async"));
        assert!(!prefers_async("respond-asyncx"));
        assert!(!prefers_async("return=minimal"));
    }
}
