//! Async client for the pisa Lean server.
//!
//! ```no_run
//! # async fn demo() -> Result<(), pisa_client::Error> {
//! let client = pisa_client::Client::new("http://localhost:8000")?;
//!
//! let result = client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num").await?;
//! assert!(result.ok);
//!
//! let session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry").await?;
//! let step = client.tactic(&session.states[0].state, "simp").await?;
//! assert!(step.complete);
//! let verdict = client
//!     .check_session(&session.session, "theorem t (n : Nat) : n + 0 = n := by simp")
//!     .await?;
//! assert!(verdict.ok);
//!
//! // Keep accepted code as a module, and build on it without re-checking it.
//! let base = client.create_snapshot("import Mathlib\ndef double (n : Nat) := 2 * n").await?;
//! let header = base.header.expect("kept");
//! let next = client.check(&format!("{header}\nexample : double 2 = 4 := rfl")).await?;
//! assert!(next.ok);
//! // The entry keeps the module; deleting it lets the module go.
//! client.delete_snapshot(&base.entry.expect("kept")).await?;
//!
//! // A long check as a job, which outlives the request and its proxies.
//! let job = client.submit_check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num").await?;
//! let result = client.wait_job(&job).await?;
//! assert!(result.ok);
//!
//! let search = pisa_client::SearchClient::new("http://localhost:8000")?;
//! let hits = search
//!     .search(&pisa_client::Query { nl: Some("trace of a density matrix is one".into()), ..Default::default() })
//!     .await?
//!     .hits;
//! # Ok(()) }
//! ```

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::time::Duration;

use pisa_protocol::api::{
    BatchItem, BatchRequest, BatchResponse, CheckRequest, Deleted, ErrorResponse, LabelsRequest,
    SessionCheckRequest, SnapshotRequest, StartRequest, TacticRequest,
};
use pisa_protocol::search::{
    DeclResponse, ResolveRequest, ResolveResponse, SearchRequest, SearchResponse,
};
use reqwest::Url;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use pisa_protocol::api::{
    CheckResult, Declaration, Detail, ErrorKind, JobKind, JobState, JobStatus, Message, Pos,
    Severity, SnapshotFailure, SnapshotInfo, SnapshotList, SnapshotResponse as Snapshot,
    SnapshotSummary, SnapshotVerified, Sorry, StartResponse as Session, StateInfo, TacticInfo,
    TacticResponse as Step,
};
pub use pisa_protocol::search::{
    DeclDetail, Flags, Hit, Informal, Query, QueryResult, Resolution, SnapshotHit, SnapshotScope,
    Suggestion,
};

const DEFAULT_TIMEOUT: u64 = 60;

/// How long each look at a job waits for it: the server's longest.
const JOB_WAIT: Duration = Duration::from_secs(60);

/// A request sent with `Prefer: respond-async`: its job, whose answer is a
/// `T`. Anyone with its id can read the answer or cancel it, so keep it
/// secret; the server keeps the answer for an hour after the job is done.
#[derive(Debug)]
pub struct Job<T> {
    pub id: String,
    pub kind: JobKind,
    answer: PhantomData<fn() -> T>,
}

impl<T> Job<T> {
    /// A job submitted earlier, e.g. by another process, by its id and kind.
    pub fn new(id: impl Into<String>, kind: JobKind) -> Self {
        Job {
            id: id.into(),
            kind,
            answer: PhantomData,
        }
    }
}

impl<T> Clone for Job<T> {
    fn clone(&self) -> Self {
        Job::new(self.id.clone(), self.kind)
    }
}

/// Where a job stands.
#[derive(Debug, Clone)]
pub enum JobPoll<T> {
    /// Queued or running.
    Pending(JobStatus),
    /// Done: what its request would have answered.
    Done(T),
}

/// Options for [`Client::create_snapshot_with`].
#[derive(Debug, Clone, Default)]
pub struct SnapshotOptions {
    /// Whether to run the kernel re-check; see
    /// [`create_snapshot_with_verify`](Client::create_snapshot_with_verify).
    pub verify: Option<bool>,
    /// Labels for the new entry.
    pub labels: BTreeMap<String, String>,
    /// An idempotency key: a retried create with the same key returns the
    /// entry the first one made, as it is, ignoring the other options.
    pub key: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid server URL: {0}")]
    Url(String),
    /// The server could not be reached or sent something unreadable.
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The server refused the request; `kind` says why. Kinds `lean`,
    /// `policy`, `timeout`, `out_of_memory` and `crash` mean the input itself
    /// failed, which during proof search is an ordinary dead end.
    #[error("{kind:?} ({status}): {message}")]
    Server {
        status: u16,
        kind: ErrorKind,
        message: String,
    },
}

impl Error {
    pub fn kind(&self) -> Option<ErrorKind> {
        match self {
            Error::Server { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

/// A server's base URL and the HTTP client that talks to it.
#[derive(Debug, Clone)]
struct Endpoint {
    http: reqwest::Client,
    base: Url,
}

impl Endpoint {
    fn new(base_url: &str) -> Result<Self, Error> {
        let base = Url::parse(base_url).map_err(|e| Error::Url(e.to_string()))?;
        Ok(Endpoint {
            http: reqwest::Client::new(),
            base,
        })
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        // Path segments are pushed individually so keys are percent-encoded.
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| Error::Url(self.base.to_string()))?
            .pop_if_empty()
            .extend(path.split('/'));
        Ok(url)
    }

    async fn health(&self) -> Result<(), Error> {
        let r = self.http.get(self.url("health")?).send().await?;
        server_error(r).await.map(drop)
    }

    async fn get<T: DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let r = self.http.get(url).send().await?;
        Ok(server_error(r).await?.json().await?)
    }

    async fn delete<T: DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let r = self.http.delete(url).send().await?;
        Ok(server_error(r).await?.json().await?)
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, Error> {
        let r = self.http.post(self.url(path)?).json(body).send().await?;
        Ok(server_error(r).await?.json().await?)
    }

    /// POSTs `body`, if any, with `Prefer: respond-async`; returns the job
    /// the server queued.
    async fn submit<B: Serialize, T>(&self, url: Url, body: Option<&B>) -> Result<Job<T>, Error> {
        let mut req = self.http.post(url).header("prefer", "respond-async");
        if let Some(body) = body {
            req = req.json(body);
        }
        let status: JobStatus = server_error(req.send().await?).await?.json().await?;
        Ok(Job::new(status.job, status.kind))
    }

    async fn post_url<T: DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let r = self.http.post(url).send().await?;
        Ok(server_error(r).await?.json().await?)
    }

    async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        url: Url,
        body: &B,
    ) -> Result<T, Error> {
        let r = self.http.patch(url).json(body).send().await?;
        Ok(server_error(r).await?.json().await?)
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    endpoint: Endpoint,
    version: Option<String>,
    timeout: u64,
}

impl Client {
    pub fn new(base_url: &str) -> Result<Self, Error> {
        Ok(Client {
            endpoint: Endpoint::new(base_url)?,
            version: None,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Checks against this worker pool version or alias instead of the
    /// server's default.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Lean's time limit per request, in seconds (default 60).
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout = seconds;
        self
    }

    pub async fn health(&self) -> Result<(), Error> {
        self.endpoint.health().await
    }

    /// Checks one piece of code. `ok` is the verdict; a check that could not
    /// complete has `error` set rather than failing.
    pub async fn check(&self, code: &str) -> Result<CheckResult, Error> {
        self.check_with_detail(code, &[]).await
    }

    /// Like [`Client::check`], also returning the `detail` asked for, e.g.
    /// [`Detail::Tactics`] for the goals before and after every tactic.
    pub async fn check_with_detail(
        &self,
        code: &str,
        detail: &[Detail],
    ) -> Result<CheckResult, Error> {
        self.post("check", &self.check_request(code, detail)).await
    }

    /// Like [`Client::check`], as a job: see [`Client::wait_job`].
    pub async fn submit_check(&self, code: &str) -> Result<Job<CheckResult>, Error> {
        self.submit_check_with_detail(code, &[]).await
    }

    /// Like [`Client::check_with_detail`], as a job.
    pub async fn submit_check_with_detail(
        &self,
        code: &str,
        detail: &[Detail],
    ) -> Result<Job<CheckResult>, Error> {
        let req = self.check_request(code, detail);
        self.endpoint
            .submit(self.endpoint.url("check")?, Some(&req))
            .await
    }

    fn check_request(&self, code: &str, detail: &[Detail]) -> CheckRequest {
        CheckRequest {
            code: code.to_string(),
            version: self.version.clone(),
            timeout: self.timeout,
            detail: detail.to_vec(),
        }
    }

    /// Checks `(id, code)` pairs in one request. Results come back in order;
    /// each item's failures are in its own `error`.
    pub async fn check_batch<I, S, C>(&self, items: I) -> Result<Vec<CheckResult>, Error>
    where
        I: IntoIterator<Item = (S, C)>,
        S: Into<String>,
        C: Into<String>,
    {
        self.check_batch_with_detail(items, &[]).await
    }

    /// Like [`Client::check_batch`], returning the `detail` asked for with
    /// every item.
    pub async fn check_batch_with_detail<I, S, C>(
        &self,
        items: I,
        detail: &[Detail],
    ) -> Result<Vec<CheckResult>, Error>
    where
        I: IntoIterator<Item = (S, C)>,
        S: Into<String>,
        C: Into<String>,
    {
        let req = BatchRequest {
            items: items
                .into_iter()
                .map(|(id, code)| BatchItem {
                    id: id.into(),
                    code: code.into(),
                })
                .collect(),
            version: self.version.clone(),
            timeout: self.timeout,
            detail: detail.to_vec(),
        };
        let resp: BatchResponse = self.post("check/batch", &req).await?;
        Ok(resp.results)
    }

    /// Runs code containing `sorry` and returns one proof state per `sorry`.
    pub async fn start_session(&self, code: &str) -> Result<Session, Error> {
        self.post("sessions", &self.start_request(code)).await
    }

    /// Like [`Client::start_session`], as a job.
    pub async fn submit_session(&self, code: &str) -> Result<Job<Session>, Error> {
        let req = self.start_request(code);
        self.endpoint
            .submit(self.endpoint.url("sessions")?, Some(&req))
            .await
    }

    fn start_request(&self, code: &str) -> StartRequest {
        StartRequest {
            code: code.to_string(),
            version: self.version.clone(),
            timeout: self.timeout,
        }
    }

    /// Applies one tactic to a proof state. A tactic Lean rejects is an
    /// [`Error::Server`] of kind [`ErrorKind::Lean`].
    pub async fn tactic(&self, state: &str, tactic: &str) -> Result<Step, Error> {
        self.post(
            &format!("states/{state}/tactic"),
            &self.tactic_request(tactic),
        )
        .await
    }

    /// Like [`Client::tactic`], as a job.
    pub async fn submit_tactic(&self, state: &str, tactic: &str) -> Result<Job<Step>, Error> {
        let url = self.endpoint.url(&format!("states/{state}/tactic"))?;
        let req = self.tactic_request(tactic);
        self.endpoint.submit(url, Some(&req)).await
    }

    fn tactic_request(&self, tactic: &str) -> TacticRequest {
        TacticRequest {
            tactic: tactic.to_string(),
            timeout: self.timeout,
        }
    }

    /// Re-checks a completed proof from scratch and confirms it proves the
    /// statements the session started from.
    pub async fn check_session(&self, session: &str, code: &str) -> Result<CheckResult, Error> {
        let req = self.session_check_request(code);
        self.post(&format!("sessions/{session}/check"), &req).await
    }

    /// Like [`Client::check_session`], as a job.
    pub async fn submit_session_check(
        &self,
        session: &str,
        code: &str,
    ) -> Result<Job<CheckResult>, Error> {
        let url = self.endpoint.url(&format!("sessions/{session}/check"))?;
        let req = self.session_check_request(code);
        self.endpoint.submit(url, Some(&req)).await
    }

    fn session_check_request(&self, code: &str) -> SessionCheckRequest {
        SessionCheckRequest {
            code: code.to_string(),
            timeout: self.timeout,
        }
    }

    /// Checks code like [`check`](Self::check) and, unless Lean reports
    /// errors, keeps it as a module that later code can import, with a new
    /// `entry` that holds it until the entry is deleted. Start that code with
    /// the returned `header` to reuse the Lean process that already has the
    /// snapshot loaded. Code with `sorry` is kept but `dirty`. The kernel
    /// re-check runs only if the code uses macros, its own or an import's;
    /// see [`verify_snapshot`](Self::verify_snapshot).
    pub async fn create_snapshot(&self, code: &str) -> Result<Snapshot, Error> {
        self.create_snapshot_with(code, &SnapshotOptions::default())
            .await
    }

    /// Like [`create_snapshot`](Self::create_snapshot), choosing whether to
    /// run the kernel re-check. `true` also re-checks the imports, so the
    /// snapshot ends `verified` or the result has an error. `false` skips it
    /// even for macros, and the snapshot stays not `verified`.
    pub async fn create_snapshot_with_verify(
        &self,
        code: &str,
        verify: bool,
    ) -> Result<Snapshot, Error> {
        let opts = SnapshotOptions {
            verify: Some(verify),
            ..Default::default()
        };
        self.create_snapshot_with(code, &opts).await
    }

    /// Like [`create_snapshot`](Self::create_snapshot), with options.
    pub async fn create_snapshot_with(
        &self,
        code: &str,
        opts: &SnapshotOptions,
    ) -> Result<Snapshot, Error> {
        self.post("snapshots", &self.snapshot_request(code, opts))
            .await
    }

    /// Like [`Client::create_snapshot`], as a job.
    pub async fn submit_snapshot(&self, code: &str) -> Result<Job<Snapshot>, Error> {
        self.submit_snapshot_with(code, &SnapshotOptions::default())
            .await
    }

    /// Like [`Client::create_snapshot_with`], as a job.
    pub async fn submit_snapshot_with(
        &self,
        code: &str,
        opts: &SnapshotOptions,
    ) -> Result<Job<Snapshot>, Error> {
        let req = self.snapshot_request(code, opts);
        self.endpoint
            .submit(self.endpoint.url("snapshots")?, Some(&req))
            .await
    }

    fn snapshot_request(&self, code: &str, opts: &SnapshotOptions) -> SnapshotRequest {
        SnapshotRequest {
            code: code.to_string(),
            version: self.version.clone(),
            timeout: self.timeout,
            verify: opts.verify,
            labels: opts.labels.clone(),
            key: opts.key.clone(),
        }
    }

    /// Re-checks an entry's module and everything it imports through the
    /// kernel, imports first, skipping what already passed. A module the
    /// kernel rejects is reported in `failure`, not as an error.
    pub async fn verify_snapshot(&self, entry: &str) -> Result<SnapshotVerified, Error> {
        self.endpoint.post_url(self.verify_url(entry)?).await
    }

    /// Like [`Client::verify_snapshot`], as a job.
    pub async fn submit_verify(&self, entry: &str) -> Result<Job<SnapshotVerified>, Error> {
        self.endpoint
            .submit(self.verify_url(entry)?, None::<&()>)
            .await
    }

    fn verify_url(&self, entry: &str) -> Result<Url, Error> {
        let mut url = self.snapshot_url(entry)?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(entry.to_string()))?
            .push("verify");
        Ok(url)
    }

    /// Where `job` stands, after waiting up to `wait` (at most 60 s) for it
    /// to finish. Done, it is what its request would have answered, errors
    /// included.
    pub async fn poll_job<T: DeserializeOwned>(
        &self,
        job: &Job<T>,
        wait: Duration,
    ) -> Result<JobPoll<T>, Error> {
        let mut url = self.job_url(&job.id)?;
        url.query_pairs_mut()
            .append_pair("wait", &wait.as_secs().to_string());
        let r = self.endpoint.http.get(url).send().await?;
        if r.status() == reqwest::StatusCode::ACCEPTED {
            return Ok(JobPoll::Pending(r.json().await?));
        }
        Ok(JobPoll::Done(server_error(r).await?.json().await?))
    }

    /// Waits for `job` to finish, however long it takes, and returns what
    /// its request would have answered. A few dropped connections in a row
    /// are tried again.
    pub async fn wait_job<T: DeserializeOwned>(&self, job: &Job<T>) -> Result<T, Error> {
        let mut dropped = 0;
        loop {
            match self.poll_job(job, JOB_WAIT).await {
                Ok(JobPoll::Done(answer)) => return Ok(answer),
                Ok(JobPoll::Pending(_)) => dropped = 0,
                Err(Error::Http(e)) if dropped < 3 && !e.is_decode() && !e.is_status() => {
                    dropped += 1;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Withdraws `job` if no worker has started it. A job that is running
    /// or done stays as it is; the status says which.
    pub async fn cancel_job<T>(&self, job: &Job<T>) -> Result<JobStatus, Error> {
        self.endpoint.delete(self.job_url(&job.id)?).await
    }

    fn job_url(&self, id: &str) -> Result<Url, Error> {
        let mut url = self.endpoint.url("jobs")?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(id.to_string()))?
            .push(id);
        Ok(url)
    }

    /// An entry with its labels, and its module's code, imports and check
    /// result.
    pub async fn snapshot(&self, entry: &str) -> Result<SnapshotInfo, Error> {
        self.endpoint.get(self.snapshot_url(entry)?).await
    }

    /// Entries newest first, as summaries without code or check results: those
    /// with every `(key, value)` label in `labels` (up to 64), or all of them.
    /// A page has up to `limit` (1 to 1000, default 100). Keep passing its
    /// `next` as `after` while it has one; a full page has one, and the page
    /// after it may be empty.
    pub async fn list_snapshots(
        &self,
        labels: &[(&str, &str)],
        limit: Option<usize>,
        after: Option<&str>,
    ) -> Result<SnapshotList, Error> {
        let mut url = self.endpoint.url("snapshots")?;
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in labels {
                query.append_pair("label", &format!("{key}:{value}"));
            }
            if let Some(limit) = limit {
                query.append_pair("limit", &limit.to_string());
            }
            if let Some(after) = after {
                query.append_pair("after", after);
            }
            if let Some(v) = &self.version {
                query.append_pair("version", v);
            }
        }
        self.endpoint.get(url).await
    }

    /// Removes the labels `remove` names from an entry, then sets those in
    /// `set`, and returns the entry's summary.
    pub async fn set_snapshot_labels(
        &self,
        entry: &str,
        set: &BTreeMap<String, String>,
        remove: &[String],
    ) -> Result<SnapshotSummary, Error> {
        let req = LabelsRequest {
            set: set.clone(),
            remove: remove.to_vec(),
        };
        self.endpoint.patch(self.snapshot_url(entry)?, &req).await
    }

    /// Deletes an entry and returns the names of the modules that went with
    /// it: its module, if no other entry or kept module needs it, and so on
    /// up its imports.
    pub async fn delete_snapshot(&self, entry: &str) -> Result<Vec<String>, Error> {
        let deleted: Deleted = self.endpoint.delete(self.snapshot_url(entry)?).await?;
        Ok(deleted.removed)
    }

    fn snapshot_url(&self, entry: &str) -> Result<Url, Error> {
        let mut url = self.endpoint.url("snapshots")?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(entry.to_string()))?
            .push(entry);
        if let Some(v) = &self.version {
            url.query_pairs_mut().append_pair("version", v);
        }
        Ok(url)
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, Error> {
        self.endpoint.post(path, body).await
    }
}

/// A client for declaration search on a `pisa-server`. Unknown names and
/// empty results are answers, not errors.
#[derive(Debug, Clone)]
pub struct SearchClient {
    endpoint: Endpoint,
    env_id: Option<String>,
}

impl SearchClient {
    pub fn new(base_url: &str) -> Result<Self, Error> {
        Ok(SearchClient {
            endpoint: Endpoint::new(base_url)?,
            env_id: None,
        })
    }

    /// Searches only an index built from this environment (a worker pool
    /// version); a server with another answers [`ErrorKind::NotFound`].
    pub fn with_env(mut self, env_id: impl Into<String>) -> Self {
        self.env_id = Some(env_id.into());
        self
    }

    pub async fn health(&self) -> Result<(), Error> {
        self.endpoint.health().await
    }

    pub async fn search(&self, query: &Query) -> Result<QueryResult, Error> {
        let mut results = self.search_batch(vec![query.clone()]).await?;
        Ok(results.pop().unwrap_or_default())
    }

    /// Runs up to 64 queries in one request; results come back in order.
    pub async fn search_batch(&self, queries: Vec<Query>) -> Result<Vec<QueryResult>, Error> {
        let req = SearchRequest {
            queries,
            env_id: self.env_id.clone(),
        };
        let resp: SearchResponse = self.endpoint.post("v1/search", &req).await?;
        Ok(resp.results)
    }

    /// Says whether each name exists, following deprecations and suggesting
    /// close names for those that don't.
    pub async fn resolve<I, S>(&self, names: I) -> Result<Vec<Resolution>, Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let req = ResolveRequest {
            names: names.into_iter().map(Into::into).collect(),
            env_id: self.env_id.clone(),
        };
        let resp: ResolveResponse = self.endpoint.post("v1/resolve", &req).await?;
        Ok(resp.results)
    }

    /// One declaration with its description, dependencies and users. An
    /// unknown name is [`ErrorKind::NotFound`], with a suggestion if any.
    pub async fn decl(&self, name: &str) -> Result<DeclDetail, Error> {
        let mut url = self.endpoint.url("v1/decl")?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(name.to_string()))?
            .push(name);
        let resp: DeclResponse = self.endpoint.get(url).await?;
        Ok(resp.decl)
    }
}

async fn server_error(r: reqwest::Response) -> Result<reqwest::Response, Error> {
    let status = r.status();
    if status.is_success() {
        return Ok(r);
    }
    let text = r.text().await.unwrap_or_default();
    let (kind, message) = match serde_json::from_str::<ErrorResponse>(&text) {
        Ok(e) => (e.error.kind, e.error.message),
        Err(_) => (ErrorKind::Internal, text),
    };
    Err(Error::Server {
        status: status.as_u16(),
        kind,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_keep_base_path_and_encode_segments() {
        let c = Endpoint::new("http://h:8000/pisa/").unwrap();
        assert_eq!(
            c.url("check/batch").unwrap().as_str(),
            "http://h:8000/pisa/check/batch"
        );
        assert_eq!(
            c.url("states/v~h~a b/tactic").unwrap().as_str(),
            "http://h:8000/pisa/states/v~h~a%20b/tactic"
        );
        assert!(Client::new("not a url").is_err());
    }
}
