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
//! let step = client.apply_tactic(&session.states[0].id, "simp").await?;
//! assert!(step.complete);
//! let verdict = client
//!     .check_session(&session.id, "theorem t (n : Nat) : n + 0 = n := by simp")
//!     .await?;
//! assert!(verdict.ok);
//!
//! // Keep accepted code as a module, and build on it without re-checking it.
//! let base = client.create_snapshot("import Mathlib\ndef double (n : Nat) := 2 * n").await?;
//! let base = base.snapshot.expect("kept");
//! let next = client.check(&format!("{}\nexample : double 2 = 4 := rfl", base.header)).await?;
//! assert!(next.ok);
//! // The snapshot keeps the module; deleting it lets the module go.
//! client.delete_snapshot(&base.id).await?;
//!
//! // A long check as a job, which outlives the request and its proxies.
//! let job = client.submit_check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num").await?;
//! let result = client.wait_job(&job).await?;
//! assert!(result.ok);
//!
//! let hits = client
//!     .search(&pisa_client::Query {
//!         description: Some("trace of a density matrix is one".into()),
//!         ..Default::default()
//!     })
//!     .await?
//!     .hits;
//! # Ok(()) }
//! ```

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::time::Duration;

use pisa_protocol::api::{
    ApplyTacticRequest, CheckBatchItem, CheckBatchRequest, CheckBatchResponse, CheckRequest,
    CheckSessionRequest, CreateSnapshotRequest, ErrorResponse, RebuildSnapshotsRequest,
    StartSessionRequest, UpdateSnapshotRequest,
};
use pisa_protocol::search::{
    GetDeclarationResponse, ResolveRequest, ResolveResponse, SearchRequest, SearchResponse,
};
use reqwest::Url;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use pisa_protocol::api::{
    self, AcceptPair, AcceptResponse, Allow, AmendedHoles, Amendment, Amends, ApplyTacticResponse,
    AtPosition, AuditAssumption, AuditComparison, AuditFork, AuditLink, AuditReview,
    AuditSnapshotsResponse, AuditSummary, AuditSwap, AuditTarget, AuditThrough, AuditUnproved,
    AuditUnsettled, Change, CheckResult, CreateSnapshotResponse, Declaration, DeclarationChange,
    DeleteSnapshotResponse, Detail, Document, DocumentGoals, DocumentProbes, ErrorKind, Export,
    GoalsAt, GoalsMode, Health, HealthStatus, Inherited, JobKind, JobStatus, LeftOut,
    LeftOutReason, LinePos, ListAcceptancesResponse, ListDependentsResponse, ListSnapshotsResponse,
    Message, NameRunRequest, NamedRun, PairError, PairErrorKind, Pos, ProbeAt, ProbeResult,
    ProofStatus, RebuildCreated, RebuildFailed, RebuildItem, RebuildReplaced, RebuildSkipped,
    RebuildSnapshotsResponse, Refutation, Refutes, Replacement, Replaces, ResubmitItem,
    ResubmitSnapshotsResponse, ReviewDeclaration, ReviewOrigin, RunImport, Session, Severity,
    Snapshot, SnapshotDetail, Sorry, State, Superseded, TacticInfo, TermInfo, VerifyFailure,
    VerifySnapshotResponse,
};
pub use pisa_protocol::search::{
    DeclarationDetail, Description, Flags, Hit, Query, QueryResult, Resolution, SearchHealth,
    SnapshotScope, Suggestion,
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
    Pending(api::Job),
    /// Done: what its request would have answered.
    Done(T),
}

/// Options for [`Client::check_with`].
#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    /// Extra detail, e.g. [`Detail::Tactics`] for the goals before and after
    /// every tactic.
    pub detail: Vec<Detail>,
    /// Up to 64 positions to report on, in [`CheckResult::at`]: the tactic
    /// at each, and with [`Detail::Terms`] the term there and the type Lean
    /// expected it to have. Ask about them all in one check: each check
    /// elaborates the code again.
    pub at: Vec<Pos>,
}

/// Options for [`Client::create_snapshot_with`].
#[derive(Debug, Clone, Default)]
pub struct CreateSnapshotOptions {
    /// Whether to run the kernel re-check, which runs by default for the code
    /// itself. `true` also re-checks the imports, so the snapshot ends
    /// `verified` or the check has an error. `false` skips it, but for
    /// module-system code, and the snapshot stays not `verified`.
    pub verify: Option<bool>,
    /// Labels for the new snapshot.
    pub labels: BTreeMap<String, String>,
    /// An idempotency key: a retried create with the same key returns the
    /// snapshot the first one made, as it is, ignoring the other options.
    pub key: Option<String>,
    /// The snapshot this code replaces. It is kept only if it keeps that
    /// snapshot's statements; otherwise the check is not `ok`, its `reasons`
    /// say what changed, and there is no snapshot. It must be a head, one
    /// that no snapshot replaces, unless `fork` is set.
    pub replaces: Option<Replaces>,
    /// Lets `replaces` name a snapshot that another replaces already, which
    /// forks the run. Otherwise the check's error is a `conflict`, and the
    /// response's `heads` are the snapshots to replace instead.
    pub fork: bool,
    /// A spec's holes: the definitions it leaves for the snapshots that
    /// replace it to change, which those inherit and may change without
    /// `allow`. Not with `replaces`; empty sends none.
    pub holes: Vec<String>,
    /// The spec this code, a spec of its own, amends, and why: the response's
    /// `amendment` says what changed. Not with `replaces`;
    /// [`resubmit_snapshots`](Client::resubmit_snapshots) carries the amended
    /// spec's run over.
    pub amends: Option<Amends>,
    /// A spec's target this code refutes, by proving `¬` its statement on
    /// the spec: the response's `refutation` names the theorem. Alone among
    /// `replaces`, `holes` and `amends`.
    pub refutes: Option<Refutes>,
    /// [`Detail::Declarations`] for every field of the check's declarations.
    pub detail: Vec<Detail>,
    /// A name for the run this snapshot starts, unique in its version, so
    /// that code can `import PisaRun.<name>`, which imports the run's head.
    /// Not with `replaces`. A name another run has is a `conflict` in the
    /// check's error, unless the create amends that run's spec, which takes
    /// the name over.
    pub name: Option<String>,
}

/// Options for [`Client::delete_snapshot_with`].
#[derive(Debug, Clone, Default)]
pub struct DeleteSnapshotOptions {
    /// Delete the snapshots that replace the snapshot too, directly or
    /// through others, and splice nothing: a whole branch of a run goes. What
    /// the snapshot replaced is a head again if nothing else replaces it.
    pub with_replacers: bool,
}

/// Options for [`Client::audit_snapshots_against_with`].
#[derive(Debug, Clone, Default)]
pub struct AuditOptions {
    /// Definitions the heads may change besides the spec's holes.
    pub allow: Vec<String>,
    /// Also list, for each spec snapshot and head, what a reviewer reads:
    /// [`AuditSnapshotsResponse::review`].
    pub review: bool,
}

/// Options for [`Client::accept_run`].
#[derive(Debug, Clone)]
pub struct AcceptOptions {
    /// Definitions the heads may change besides the spec's holes, as for the
    /// audit.
    pub allow: Vec<String>,
    /// Also run the external kernels Lean bundles (true by default).
    pub paranoid: bool,
    /// Each pair's limit in seconds; the server's default (600) unless set,
    /// not the client's timeout.
    pub timeout: Option<u64>,
    /// Seconds more for the first pair, which builds the run's modules; the
    /// server's default (1800) unless set.
    pub build_timeout: Option<u64>,
}

impl Default for AcceptOptions {
    fn default() -> Self {
        AcceptOptions {
            allow: Vec::new(),
            paranoid: true,
            timeout: None,
            build_timeout: None,
        }
    }
}

/// Options for [`Client::document_goals`].
#[derive(Debug, Clone, Default)]
pub struct GoalsOptions {
    /// Answer only if the document is at this revision, else a `conflict`.
    pub revision: Option<u64>,
    /// The goals before the tactic at each position (the default), or after.
    pub mode: GoalsMode,
}

/// Options for [`Client::rebuild_snapshots`].
#[derive(Debug, Clone, Default)]
pub struct RebuildOptions {
    /// Only plan: create each item's `code` with its `labels`, `key` and
    /// `replaces`, a level at a time.
    pub dry_run: bool,
    /// Rebuild even though `to`, created without `replaces`, changes what
    /// `from` states.
    pub allow_changes: bool,
    /// Once every item is created, delete the old snapshots whose copies
    /// replace them, from the top down. `from` stays.
    pub retire: bool,
    /// The kernel re-check for each create; see
    /// [`CreateSnapshotOptions::verify`].
    pub verify: Option<bool>,
}

/// Options for [`Client::settle_run`].
#[derive(Debug, Clone, Default)]
pub struct SettleOptions {
    /// As the audit's `allow`.
    pub allow: Vec<String>,
    /// Only plan.
    pub dry_run: bool,
    /// The kernel re-check for each create; see
    /// [`CreateSnapshotOptions::verify`].
    pub verify: Option<bool>,
}

/// Options for [`Client::resubmit_snapshots`].
#[derive(Debug, Clone, Default)]
pub struct ResubmitOptions {
    /// Only plan: compare each head with the amending spec.
    pub dry_run: bool,
    /// The new snapshots' labels; by default each head's.
    pub labels: Option<BTreeMap<String, String>>,
    /// The kernel re-check for each create; see
    /// [`CreateSnapshotOptions::verify`].
    pub verify: Option<bool>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid server URL: {0}")]
    Url(String),
    /// The request was refused before it was sent, as one the server would
    /// misread, e.g. an audit against no snapshot.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The server could not be reached or sent something unreadable.
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The server refused the request; `kind` says why. A submit that the
    /// server answered at once, not by queueing a job, is `internal` with the
    /// answer's status. Kinds `lean`,
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

    async fn health(&self) -> Result<Health, Error> {
        let r = self.http.get(self.url("health")?).send().await?;
        let status = r.status();
        if status != reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(server_error(r).await?.json().await?);
        }
        // A draining server answers 503 with its health: an answer, where a
        // 503 from anything else, such as a proxy, is an error.
        let text = r.text().await?;
        serde_json::from_str(&text).map_err(|_| server_failure(status, &text))
    }

    async fn get<T: DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let r = self.http.get(url).send().await?;
        Ok(server_error(r).await?.json().await?)
    }

    async fn delete<T: DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let r = self.http.delete(url).send().await?;
        Ok(server_error(r).await?.json().await?)
    }

    /// A DELETE whose success has no body, such as a 204.
    async fn delete_empty(&self, url: Url) -> Result<(), Error> {
        server_error(self.http.delete(url).send().await?).await?;
        Ok(())
    }

    /// A GET whose answer isn't JSON, such as a tar.gz.
    async fn get_bytes(&self, url: Url) -> Result<Vec<u8>, Error> {
        let r = self.http.get(url).send().await?;
        Ok(server_error(r).await?.bytes().await?.to_vec())
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
    /// the server queued. Any other success, i.e. an answer at once, is an
    /// [`Error::Server`] of kind [`ErrorKind::Internal`], as is an answer
    /// that isn't pisa's.
    async fn submit<B: Serialize, T>(&self, url: Url, body: Option<&B>) -> Result<Job<T>, Error> {
        let mut req = self.http.post(url).header("prefer", "respond-async");
        if let Some(body) = body {
            req = req.json(body);
        }
        let r = server_error(req.send().await?).await?;
        let status = r.status();
        if status != reqwest::StatusCode::ACCEPTED {
            return Err(Error::Server {
                status: status.as_u16(),
                kind: ErrorKind::Internal,
                message: "the server answered at once instead of queueing a job, as it does \
                    for a dry run"
                    .into(),
            });
        }
        let job: api::Job = r.json().await?;
        Ok(Job::new(job.id, job.kind))
    }

    async fn post_to<B: Serialize, T: DeserializeOwned>(
        &self,
        url: Url,
        body: &B,
    ) -> Result<T, Error> {
        let r = self.http.post(url).json(body).send().await?;
        Ok(server_error(r).await?.json().await?)
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

    async fn put<B: Serialize, T: DeserializeOwned>(&self, url: Url, body: &B) -> Result<T, Error> {
        let r = self.http.put(url).json(body).send().await?;
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

    /// Checks, starts sessions, creates, lists, audits and searches against
    /// this worker pool version or alias instead of the server's default.
    /// Session, state and snapshot ids carry their own version, so calls that
    /// take one don't send it. Search answers for the version of its index
    /// and of its snapshots, and any other is [`ErrorKind::NotFound`].
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Sends every request in `scope`, as the `Pisa-Scope` header: what a
    /// request creates carries these labels, and what it names or reaches
    /// must, else it is [`ErrorKind::NotFound`]. Run names and idempotency
    /// keys are unique within a scope. An empty scope sends no header.
    pub fn with_scope(mut self, scope: &BTreeMap<String, String>) -> Result<Self, Error> {
        let scope = pisa_protocol::scope::Scope::new(scope.clone()).map_err(Error::Invalid)?;
        let mut headers = reqwest::header::HeaderMap::new();
        if !scope.is_empty() {
            let value = reqwest::header::HeaderValue::from_str(&scope.header())
                .map_err(|e| Error::Invalid(format!("the scope can't be a header: {e}")))?;
            headers.insert(pisa_protocol::scope::HEADER, value);
        }
        self.endpoint.http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        Ok(self)
    }

    /// Lean's time limit per request, in seconds (default 60).
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout = seconds;
        self
    }

    /// The server's health: whether it is up or draining, and its workers. A
    /// draining server's 503 is returned, not an error.
    pub async fn health(&self) -> Result<Health, Error> {
        self.endpoint.health().await
    }

    /// Checks one piece of code. `ok` is the verdict; a check that could not
    /// complete has `error` set rather than failing.
    pub async fn check(&self, code: &str) -> Result<CheckResult, Error> {
        self.check_with(code, &CheckOptions::default()).await
    }

    /// Like [`Client::check`], also returning what `options` asks for.
    pub async fn check_with(
        &self,
        code: &str,
        options: &CheckOptions,
    ) -> Result<CheckResult, Error> {
        self.post("check", &self.check_request(code, options)).await
    }

    /// Like [`Client::check`], as a job: see [`Client::wait_job`].
    pub async fn submit_check(&self, code: &str) -> Result<Job<CheckResult>, Error> {
        self.submit_check_with(code, &CheckOptions::default()).await
    }

    /// Like [`Client::check_with`], as a job.
    pub async fn submit_check_with(
        &self,
        code: &str,
        options: &CheckOptions,
    ) -> Result<Job<CheckResult>, Error> {
        let req = self.check_request(code, options);
        self.endpoint
            .submit(self.endpoint.url("check")?, Some(&req))
            .await
    }

    fn check_request(&self, code: &str, options: &CheckOptions) -> CheckRequest {
        CheckRequest {
            code: code.to_string(),
            version: self.version.clone(),
            timeout: self.timeout,
            detail: options.detail.clone(),
            at: options.at.clone(),
        }
    }

    /// Checks several pieces of code in one request, up to 128. Results come
    /// back in the order of `codes`; each one's failures are in its own
    /// `error`.
    pub async fn check_batch<I, C>(&self, codes: I) -> Result<Vec<CheckResult>, Error>
    where
        I: IntoIterator<Item = C>,
        C: AsRef<str>,
    {
        self.check_batch_with(codes, &CheckOptions::default()).await
    }

    /// Like [`Client::check_batch`], returning what `options` asks for with
    /// every piece of code, its positions in that code.
    pub async fn check_batch_with<I, C>(
        &self,
        codes: I,
        options: &CheckOptions,
    ) -> Result<Vec<CheckResult>, Error>
    where
        I: IntoIterator<Item = C>,
        C: AsRef<str>,
    {
        let req = CheckBatchRequest {
            items: codes
                .into_iter()
                .map(|code| CheckBatchItem {
                    code: code.as_ref().to_string(),
                })
                .collect(),
            version: self.version.clone(),
            timeout: self.timeout,
            detail: options.detail.clone(),
            at: options.at.clone(),
        };
        let resp: CheckBatchResponse = self.post("check/batch", &req).await?;
        Ok(resp.results)
    }

    /// Runs code containing `sorry` and returns one proof state per `sorry`.
    pub async fn start_session(&self, code: &str) -> Result<Session, Error> {
        self.post("sessions", &self.start_session_request(code))
            .await
    }

    /// Like [`Client::start_session`], as a job.
    pub async fn submit_start_session(&self, code: &str) -> Result<Job<Session>, Error> {
        let req = self.start_session_request(code);
        self.endpoint
            .submit(self.endpoint.url("sessions")?, Some(&req))
            .await
    }

    fn start_session_request(&self, code: &str) -> StartSessionRequest {
        StartSessionRequest {
            code: code.to_string(),
            version: self.version.clone(),
            timeout: self.timeout,
        }
    }

    /// Applies one tactic to a proof state. A tactic Lean rejects is an
    /// [`Error::Server`] of kind [`ErrorKind::Lean`].
    pub async fn apply_tactic(
        &self,
        state_id: &str,
        tactic: &str,
    ) -> Result<ApplyTacticResponse, Error> {
        self.post(
            &format!("states/{state_id}/tactic"),
            &self.apply_tactic_request(tactic),
        )
        .await
    }

    /// Like [`Client::apply_tactic`], as a job.
    pub async fn submit_apply_tactic(
        &self,
        state_id: &str,
        tactic: &str,
    ) -> Result<Job<ApplyTacticResponse>, Error> {
        let url = self.endpoint.url(&format!("states/{state_id}/tactic"))?;
        let req = self.apply_tactic_request(tactic);
        self.endpoint.submit(url, Some(&req)).await
    }

    fn apply_tactic_request(&self, tactic: &str) -> ApplyTacticRequest {
        ApplyTacticRequest {
            tactic: tactic.to_string(),
            timeout: self.timeout,
        }
    }

    /// Re-checks a completed proof from scratch and confirms it proves the
    /// statements the session started from.
    pub async fn check_session(&self, session_id: &str, code: &str) -> Result<CheckResult, Error> {
        self.check_session_with(session_id, code, &[]).await
    }

    /// Like [`Client::check_session`], with [`Detail::Declarations`] for
    /// every field of the declarations.
    pub async fn check_session_with(
        &self,
        session_id: &str,
        code: &str,
        detail: &[Detail],
    ) -> Result<CheckResult, Error> {
        let req = self.check_session_request(code, detail);
        self.post(&format!("sessions/{session_id}/check"), &req)
            .await
    }

    /// Like [`Client::check_session`], as a job.
    pub async fn submit_check_session(
        &self,
        session_id: &str,
        code: &str,
    ) -> Result<Job<CheckResult>, Error> {
        let url = self.endpoint.url(&format!("sessions/{session_id}/check"))?;
        let req = self.check_session_request(code, &[]);
        self.endpoint.submit(url, Some(&req)).await
    }

    fn check_session_request(&self, code: &str, detail: &[Detail]) -> CheckSessionRequest {
        CheckSessionRequest {
            code: code.to_string(),
            timeout: self.timeout,
            detail: detail.to_vec(),
        }
    }

    /// Checks code like [`check`](Self::check) and, unless Lean reports
    /// errors, keeps it as a module that later code can import, with a new
    /// snapshot that holds it until the snapshot is deleted. Start that code
    /// with the snapshot's `header` to reuse the Lean process that already
    /// has the module loaded. Code with `sorry` is kept, with that `status`.
    /// The kernel re-check runs on the code; see
    /// [`verify_snapshot`](Self::verify_snapshot) for its imports.
    pub async fn create_snapshot(&self, code: &str) -> Result<CreateSnapshotResponse, Error> {
        self.create_snapshot_with(code, &CreateSnapshotOptions::default())
            .await
    }

    /// Like [`create_snapshot`](Self::create_snapshot), with options.
    pub async fn create_snapshot_with(
        &self,
        code: &str,
        opts: &CreateSnapshotOptions,
    ) -> Result<CreateSnapshotResponse, Error> {
        self.post("snapshots", &self.create_snapshot_request(code, opts))
            .await
    }

    /// Like [`Client::create_snapshot`], as a job.
    pub async fn submit_create_snapshot(
        &self,
        code: &str,
    ) -> Result<Job<CreateSnapshotResponse>, Error> {
        self.submit_create_snapshot_with(code, &CreateSnapshotOptions::default())
            .await
    }

    /// Like [`Client::create_snapshot_with`], as a job.
    pub async fn submit_create_snapshot_with(
        &self,
        code: &str,
        opts: &CreateSnapshotOptions,
    ) -> Result<Job<CreateSnapshotResponse>, Error> {
        let req = self.create_snapshot_request(code, opts);
        self.endpoint
            .submit(self.endpoint.url("snapshots")?, Some(&req))
            .await
    }

    fn create_snapshot_request(
        &self,
        code: &str,
        opts: &CreateSnapshotOptions,
    ) -> CreateSnapshotRequest {
        // A create that names a spec runs on the spec's version, which its id
        // carries: another is refused.
        let names_one = opts.replaces.is_some() || opts.amends.is_some() || opts.refutes.is_some();
        CreateSnapshotRequest {
            code: code.to_string(),
            version: if names_one {
                None
            } else {
                self.version.clone()
            },
            timeout: self.timeout,
            verify: opts.verify,
            labels: opts.labels.clone(),
            key: opts.key.clone(),
            replaces: opts.replaces.clone(),
            fork: opts.fork,
            holes: (!opts.holes.is_empty()).then(|| opts.holes.clone()),
            amends: opts.amends.clone(),
            refutes: opts.refutes.clone(),
            detail: opts.detail.clone(),
            name: opts.name.clone(),
        }
    }

    /// Re-checks a snapshot's module and everything it imports through the
    /// kernel, imports first, skipping what already passed, with the client's
    /// timeout for each module. A module the kernel rejects, or that runs out
    /// of time, is reported in `failure`, not as an error.
    pub async fn verify_snapshot(&self, id: &str) -> Result<VerifySnapshotResponse, Error> {
        self.endpoint.post_url(self.verify_url(id)?).await
    }

    /// Like [`Client::verify_snapshot`], but re-checks the modules that are
    /// verified already too, which describes their records again: what a
    /// record made before `own_axioms` and `inherits` were recorded needs to
    /// be read through links.
    pub async fn verify_snapshot_again(&self, id: &str) -> Result<VerifySnapshotResponse, Error> {
        let mut url = self.verify_url(id)?;
        url.query_pairs_mut().append_pair("again", "true");
        self.endpoint.post_url(url).await
    }

    /// Like [`Client::verify_snapshot`], as a job.
    pub async fn submit_verify_snapshot(
        &self,
        id: &str,
    ) -> Result<Job<VerifySnapshotResponse>, Error> {
        self.endpoint
            .submit(self.verify_url(id)?, None::<&()>)
            .await
    }

    /// `POST /snapshots/{id}/verify`, with this client's timeout for each
    /// module.
    fn verify_url(&self, id: &str) -> Result<Url, Error> {
        let mut url = self.snapshot_url(id)?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(id.to_string()))?
            .push("verify");
        url.query_pairs_mut()
            .append_pair("timeout", &self.timeout.to_string());
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
    pub async fn cancel_job<T>(&self, job: &Job<T>) -> Result<api::Job, Error> {
        self.endpoint.delete(self.job_url(&job.id)?).await
    }

    fn job_url(&self, id: &str) -> Result<Url, Error> {
        let mut url = self.endpoint.url("jobs")?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(id.to_string()))?
            .push(id);
        Ok(url)
    }

    /// A snapshot with its module's code, check result and exports.
    pub async fn get_snapshot(&self, id: &str) -> Result<SnapshotDetail, Error> {
        self.get_snapshot_with(id, &[]).await
    }

    /// Like [`Client::get_snapshot`], with [`Detail::Declarations`] for every
    /// field of the check's declarations.
    pub async fn get_snapshot_with(
        &self,
        id: &str,
        detail: &[Detail],
    ) -> Result<SnapshotDetail, Error> {
        let mut url = self.snapshot_url(id)?;
        if detail.contains(&Detail::Declarations) {
            url.query_pairs_mut().append_pair("detail", "declarations");
        }
        self.endpoint.get(url).await
    }

    /// Snapshots newest first: those with every label in `labels` (up to
    /// 64), or all of them. With `heads`, only those that no other snapshot
    /// replaces. A page has up to `limit` (1 to 1000, default 100). Keep
    /// passing its `next` as `after` while it has one; a full page has one,
    /// and the page after it may be empty.
    pub async fn list_snapshots(
        &self,
        labels: &BTreeMap<String, String>,
        limit: Option<usize>,
        after: Option<&str>,
        heads: bool,
    ) -> Result<ListSnapshotsResponse, Error> {
        let mut url = self.endpoint.url("snapshots")?;
        add_labels(&mut url, labels);
        {
            let mut query = url.query_pairs_mut();
            if let Some(limit) = limit {
                query.append_pair("limit", &limit.to_string());
            }
            if let Some(after) = after {
                query.append_pair("after", after);
            }
            if heads {
                query.append_pair("heads", "true");
            }
            if let Some(v) = &self.version {
                query.append_pair("version", v);
            }
        }
        self.endpoint.get(url).await
    }

    /// The snapshots of every module that imports snapshot `id`'s, directly
    /// or through others, parents first: those with every label in `labels`,
    /// or all of them.
    pub async fn list_dependents(
        &self,
        id: &str,
        labels: &BTreeMap<String, String>,
    ) -> Result<ListDependentsResponse, Error> {
        let mut url = self.snapshot_url(id)?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(id.to_string()))?
            .push("dependents");
        add_labels(&mut url, labels);
        self.endpoint.get(url).await
    }

    /// Rebuilds `from`'s dependents with every label in `labels` on `to`:
    /// the server plans the creates that copy their heads, by level, and
    /// unless `opts.dry_run` runs them, a level at a time, and with `retire`
    /// then deletes the old snapshots whose copies replace them, though not
    /// `from`. Sending it again replays the creates that are done. A run can
    /// take minutes;
    /// [`submit_rebuild_snapshots`](Self::submit_rebuild_snapshots) runs it
    /// as a job.
    pub async fn rebuild_snapshots(
        &self,
        from: &str,
        to: &str,
        labels: &BTreeMap<String, String>,
        opts: &RebuildOptions,
    ) -> Result<RebuildSnapshotsResponse, Error> {
        let req = self.rebuild_snapshots_request(from, to, labels, opts);
        self.post("snapshots/rebuild", &req).await
    }

    /// Like [`Client::rebuild_snapshots`], as a job. A dry run isn't one: the
    /// server answers it at once, which is an [`Error::Server`] of kind
    /// [`ErrorKind::Internal`] here, with status 200; use
    /// [`rebuild_snapshots`](Self::rebuild_snapshots) for one.
    pub async fn submit_rebuild_snapshots(
        &self,
        from: &str,
        to: &str,
        labels: &BTreeMap<String, String>,
        opts: &RebuildOptions,
    ) -> Result<Job<RebuildSnapshotsResponse>, Error> {
        let req = self.rebuild_snapshots_request(from, to, labels, opts);
        self.endpoint
            .submit(self.endpoint.url("snapshots/rebuild")?, Some(&req))
            .await
    }

    /// Carries the run of spec `from` over to `to`, the spec that amends it:
    /// the server compares each head of the run with `to`, and unless
    /// `opts.dry_run` creates each head that keeps `to`'s statements again,
    /// replacing `to`. Sending it again replays the creates that are done.
    /// [`submit_resubmit_snapshots`](Self::submit_resubmit_snapshots) runs it
    /// as a job.
    pub async fn resubmit_snapshots(
        &self,
        from: &str,
        to: &str,
        opts: &ResubmitOptions,
    ) -> Result<ResubmitSnapshotsResponse, Error> {
        let req = self.resubmit_request(from, to, opts);
        self.post("snapshots/resubmit", &req).await
    }

    /// Like [`Client::resubmit_snapshots`], as a job; a dry run isn't one,
    /// as for [`submit_rebuild_snapshots`](Self::submit_rebuild_snapshots).
    pub async fn submit_resubmit_snapshots(
        &self,
        from: &str,
        to: &str,
        opts: &ResubmitOptions,
    ) -> Result<Job<ResubmitSnapshotsResponse>, Error> {
        let req = self.resubmit_request(from, to, opts);
        self.endpoint
            .submit(self.endpoint.url("snapshots/resubmit")?, Some(&req))
            .await
    }

    /// Settles the run that spec snapshots `against` start: rebuilds each of
    /// its heads whose targets hold only through links onto the heads they
    /// are read through, in one plan, as the audit's `unsettled` lists them
    /// (under `opts.allow`). A head with blockers refuses the plan, with them
    /// in `changes`. [`submit_settle_run`](Self::submit_settle_run) runs it
    /// as a job.
    pub async fn settle_run<I, S>(
        &self,
        against: I,
        opts: &SettleOptions,
    ) -> Result<RebuildSnapshotsResponse, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let req = self.settle_request(against, opts);
        self.post("snapshots/settle", &req).await
    }

    /// Like [`Client::settle_run`], as a job; a dry run isn't one, as for
    /// [`submit_rebuild_snapshots`](Self::submit_rebuild_snapshots).
    pub async fn submit_settle_run<I, S>(
        &self,
        against: I,
        opts: &SettleOptions,
    ) -> Result<Job<RebuildSnapshotsResponse>, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let req = self.settle_request(against, opts);
        self.endpoint
            .submit(self.endpoint.url("snapshots/settle")?, Some(&req))
            .await
    }

    fn settle_request<I, S>(&self, against: I, opts: &SettleOptions) -> api::SettleRunRequest
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        api::SettleRunRequest {
            against: against
                .into_iter()
                .map(|s| s.as_ref().to_string())
                .collect(),
            allow: opts.allow.clone(),
            dry_run: opts.dry_run,
            verify: opts.verify,
            timeout: self.timeout,
        }
    }

    fn resubmit_request(
        &self,
        from: &str,
        to: &str,
        opts: &ResubmitOptions,
    ) -> api::ResubmitSnapshotsRequest {
        api::ResubmitSnapshotsRequest {
            from: from.to_string(),
            to: to.to_string(),
            dry_run: opts.dry_run,
            labels: opts.labels.clone(),
            verify: opts.verify,
            timeout: self.timeout,
        }
    }

    fn rebuild_snapshots_request(
        &self,
        from: &str,
        to: &str,
        labels: &BTreeMap<String, String>,
        opts: &RebuildOptions,
    ) -> RebuildSnapshotsRequest {
        RebuildSnapshotsRequest {
            from: from.to_string(),
            to: to.to_string(),
            labels: labels.clone(),
            dry_run: opts.dry_run,
            allow_changes: opts.allow_changes,
            retire: opts.retire,
            verify: opts.verify,
            timeout: self.timeout,
        }
    }

    /// What the snapshots with every label in `labels` rest on: their
    /// declarations by status, each assumption with what uses it, and what
    /// isn't proved; see [`AuditSnapshotsResponse`].
    pub async fn audit_snapshots(
        &self,
        labels: &BTreeMap<String, String>,
    ) -> Result<AuditSnapshotsResponse, Error> {
        let mut url = self.endpoint.url("snapshots/audit")?;
        add_labels(&mut url, labels);
        if let Some(v) = &self.version {
            url.query_pairs_mut().append_pair("version", v);
        }
        self.endpoint.get(url).await
    }

    /// Audits a run against its spec, the snapshots `against` names, all of
    /// one version: the heads it reaches through the snapshots that replace
    /// them, the forks on the way, each spec declaration's status on each
    /// head, the links that allowed changes, and whether the run is `clean`,
    /// with no definition changed but the spec's holes and those `allow`
    /// names; see
    /// [`AuditSnapshotsResponse`]. An empty `against` is an
    /// [`Error::Invalid`]: without ids, the server would audit by labels.
    pub async fn audit_snapshots_against<I, S>(
        &self,
        against: I,
        allow: &[&str],
    ) -> Result<AuditSnapshotsResponse, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let against: Vec<S> = against.into_iter().collect();
        if against.is_empty() {
            return Err(Error::Invalid(
                "against names no snapshot: audit a run against its spec snapshots, or a \
                 project by its labels"
                    .into(),
            ));
        }
        let mut url = self.endpoint.url("snapshots/audit")?;
        for id in against {
            url.query_pairs_mut().append_pair("against", id.as_ref());
        }
        for name in allow {
            url.query_pairs_mut().append_pair("allow", name);
        }
        self.endpoint.get(url).await
    }

    /// [`Client::audit_snapshots_against`] with `options`: with `review`, the
    /// audit lists for each spec snapshot and head what a reviewer reads.
    pub async fn audit_snapshots_against_with<I, S>(
        &self,
        against: I,
        options: &AuditOptions,
    ) -> Result<AuditSnapshotsResponse, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut url = self.run_url("snapshots/audit", against, &options.allow)?;
        if options.review {
            url.query_pairs_mut().append_pair("review", "true");
        }
        self.endpoint.get(url).await
    }

    /// The run that spec snapshots `against` start, as a Lake project for
    /// `lake comparator`, in `pisa-run/` of a tar.gz: its bytes. `allow` is
    /// as for the audit.
    pub async fn export_run<I, S>(&self, against: I, allow: &[&str]) -> Result<Vec<u8>, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let allow: Vec<String> = allow.iter().map(|s| s.to_string()).collect();
        let url = self.run_url("snapshots/export", against, &allow)?;
        self.endpoint.get_bytes(url).await
    }

    /// Judges the run that spec snapshots `against` start: audits it and
    /// runs `lake comparator` on the pairs the audit compared, with this
    /// client's timeout for each pair, and stores the result; see
    /// [`AcceptResponse`].
    pub async fn accept_run<I, S>(
        &self,
        against: I,
        options: &AcceptOptions,
    ) -> Result<AcceptResponse, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let body = self.accept_request(against, options)?;
        self.endpoint.post("snapshots/accept", &body).await
    }

    /// Like [`Client::accept_run`], as a job.
    pub async fn submit_accept_run<I, S>(
        &self,
        against: I,
        options: &AcceptOptions,
    ) -> Result<Job<AcceptResponse>, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let body = self.accept_request(against, options)?;
        let url = self.endpoint.url("snapshots/accept")?;
        self.endpoint.submit(url, Some(&body)).await
    }

    /// An acceptance [`Client::accept_run`] stored, by its id.
    pub async fn get_acceptance(&self, id: &str) -> Result<AcceptResponse, Error> {
        let mut url = self.endpoint.url("snapshots/accept")?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(id.to_string()))?
            .push(id);
        self.endpoint.get(url).await
    }

    /// The run named `name`: its root, its heads, and, when they are on one
    /// module, what `import PisaRun.<name>` resolves to now.
    pub async fn get_run(&self, name: &str) -> Result<NamedRun, Error> {
        let mut url = self.named_run_url(name)?;
        if let Some(v) = &self.version {
            url.query_pairs_mut().append_pair("version", v);
        }
        self.endpoint.get(url).await
    }

    /// Names `name` the run snapshot `snapshot_id` is in, as a create's
    /// `name` names the run it starts: a name the run had goes, and one
    /// another run with snapshots has is a conflict. Returns the run.
    pub async fn name_run(&self, name: &str, snapshot_id: &str) -> Result<NamedRun, Error> {
        let req = NameRunRequest {
            snapshot_id: snapshot_id.to_string(),
        };
        self.endpoint.put(self.named_run_url(name)?, &req).await
    }

    fn named_run_url(&self, name: &str) -> Result<Url, Error> {
        let mut url = self.endpoint.url("runs")?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(name.to_string()))?
            .push(name);
        Ok(url)
    }

    /// The acceptances that judged spec snapshot `spec`, newest first, at
    /// most 100; each says `stale` once the run's heads have moved on.
    pub async fn list_acceptances(&self, spec: &str) -> Result<ListAcceptancesResponse, Error> {
        let mut url = self.endpoint.url("snapshots/accept")?;
        url.query_pairs_mut().append_pair("spec", spec);
        self.endpoint.get(url).await
    }

    fn accept_request<I, S>(
        &self,
        against: I,
        options: &AcceptOptions,
    ) -> Result<api::AcceptRequest, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let against: Vec<String> = against
            .into_iter()
            .map(|s| s.as_ref().to_string())
            .collect();
        if against.is_empty() {
            return Err(Error::Invalid(
                "against names no snapshot: a run is named by its spec snapshots".into(),
            ));
        }
        Ok(api::AcceptRequest {
            against,
            allow: options.allow.clone(),
            paranoid: options.paranoid,
            timeout: options.timeout,
            build_timeout: options.build_timeout,
        })
    }

    /// `path`, with a run's spec snapshots and `allow` in its query. An empty
    /// `against` is an [`Error::Invalid`].
    fn run_url<I, S>(&self, path: &str, against: I, allow: &[String]) -> Result<Url, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut url = self.endpoint.url(path)?;
        let mut none = true;
        for id in against {
            url.query_pairs_mut().append_pair("against", id.as_ref());
            none = false;
        }
        if none {
            return Err(Error::Invalid(
                "against names no snapshot: a run is named by its spec snapshots".into(),
            ));
        }
        for name in allow {
            url.query_pairs_mut().append_pair("allow", name);
        }
        Ok(url)
    }

    /// Opens `code` as a document on Lean Beam, which keeps it elaborated for
    /// goals and probes until [`Client::close_document`] or until it expires.
    pub async fn open_document(&self, code: &str) -> Result<Document, Error> {
        let body = api::OpenDocumentRequest {
            code: code.to_string(),
            version: self.version.clone(),
            timeout: self.timeout,
        };
        self.endpoint.post("documents", &body).await
    }

    /// Replaces a document's code, which is at `revision`; answers the next
    /// revision.
    pub async fn edit_document(
        &self,
        id: &str,
        revision: u64,
        code: &str,
    ) -> Result<Document, Error> {
        let body = api::EditDocumentRequest {
            code: code.to_string(),
            revision,
            timeout: self.timeout,
        };
        self.endpoint
            .patch(self.document_url(id, None)?, &body)
            .await
    }

    /// Closes a document and deletes its code.
    pub async fn close_document(&self, id: &str) -> Result<(), Error> {
        self.endpoint
            .delete_empty(self.document_url(id, None)?)
            .await
    }

    /// The goals at each of `at`, lines from 1; a position with only a line
    /// is the line's first tactic.
    pub async fn document_goals(
        &self,
        id: &str,
        at: &[LinePos],
        options: &GoalsOptions,
    ) -> Result<DocumentGoals, Error> {
        let body = api::DocumentGoalsRequest {
            revision: options.revision,
            at: at.to_vec(),
            mode: options.mode,
            timeout: self.timeout,
        };
        let url = self.document_url(id, Some("goals"))?;
        self.endpoint.post_to(url, &body).await
    }

    /// Runs each of `candidates` at `at` on the document as it is, `revision`
    /// if given: nothing a candidate does stays.
    pub async fn probe_document(
        &self,
        id: &str,
        at: ProbeAt,
        candidates: &[&str],
        revision: Option<u64>,
    ) -> Result<DocumentProbes, Error> {
        let body = api::DocumentProbesRequest {
            revision,
            at,
            candidates: candidates.iter().map(|c| c.to_string()).collect(),
            timeout: self.timeout,
            budget: None,
        };
        let url = self.document_url(id, Some("probes"))?;
        self.endpoint.post_to(url, &body).await
    }

    fn document_url(&self, id: &str, then: Option<&str>) -> Result<Url, Error> {
        let mut url = self.endpoint.url("documents")?;
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| Error::Url(id.to_string()))?;
        segments.push(id);
        if let Some(then) = then {
            segments.push(then);
        }
        drop(segments);
        Ok(url)
    }

    /// Changes a snapshot's labels: each label `labels` maps to `None` is
    /// removed, each it maps to `Some` is set, and the rest stay. Returns the
    /// snapshot.
    pub async fn update_snapshot(
        &self,
        id: &str,
        labels: &BTreeMap<String, Option<String>>,
    ) -> Result<Snapshot, Error> {
        let req = UpdateSnapshotRequest {
            labels: labels.clone(),
        };
        self.endpoint.patch(self.snapshot_url(id)?, &req).await
    }

    /// Deletes a snapshot. `removed` names the modules that went with it:
    /// its module, if no other snapshot or kept module needs it, and so on up
    /// its imports. The snapshots that replace it replace what it replaced
    /// instead; `deleted` lists the snapshot alone.
    pub async fn delete_snapshot(&self, id: &str) -> Result<DeleteSnapshotResponse, Error> {
        self.delete_snapshot_with(id, &DeleteSnapshotOptions::default())
            .await
    }

    /// Like [`delete_snapshot`](Self::delete_snapshot), with options. With
    /// [`with_replacers`](DeleteSnapshotOptions::with_replacers), `deleted`
    /// lists every snapshot that went, the asked one first.
    pub async fn delete_snapshot_with(
        &self,
        id: &str,
        opts: &DeleteSnapshotOptions,
    ) -> Result<DeleteSnapshotResponse, Error> {
        let mut url = self.snapshot_url(id)?;
        if opts.with_replacers {
            url.query_pairs_mut().append_pair("with_replacers", "true");
        }
        self.endpoint.delete(url).await
    }

    fn snapshot_url(&self, id: &str) -> Result<Url, Error> {
        let mut url = self.endpoint.url("snapshots")?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(id.to_string()))?
            .push(id);
        Ok(url)
    }

    /// Searches declarations, as [`Query`] says. Unknown names and empty
    /// results are answers, not errors.
    pub async fn search(&self, query: &Query) -> Result<QueryResult, Error> {
        let mut results = self.search_batch(vec![query.clone()]).await?;
        Ok(results.pop().unwrap_or_default())
    }

    /// Runs up to 64 queries in one request; results come back in order.
    pub async fn search_batch(&self, queries: Vec<Query>) -> Result<Vec<QueryResult>, Error> {
        let req = SearchRequest {
            queries,
            version: self.version.clone(),
        };
        let resp: SearchResponse = self.post("search", &req).await?;
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
            version: self.version.clone(),
        };
        let resp: ResolveResponse = self.post("resolve", &req).await?;
        Ok(resp.results)
    }

    /// One declaration with its description, dependencies and users: the
    /// one in snapshot module `module`, as a snapshot hit's `module` names
    /// it, if it has one, else the bundle's. An unknown name is
    /// [`ErrorKind::NotFound`], with a suggestion if any.
    pub async fn get_declaration(
        &self,
        name: &str,
        module: Option<&str>,
    ) -> Result<DeclarationDetail, Error> {
        let mut url = self.endpoint.url("declarations")?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(name.to_string()))?
            .push(name);
        if let Some(m) = module {
            url.query_pairs_mut().append_pair("module", m);
        }
        if let Some(v) = &self.version {
            url.query_pairs_mut().append_pair("version", v);
        }
        let resp: GetDeclarationResponse = self.endpoint.get(url).await?;
        Ok(resp.declaration)
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, Error> {
        self.endpoint.post(path, body).await
    }
}

/// Adds a `label=key:value` filter to `url` for each of `labels`.
fn add_labels(url: &mut Url, labels: &BTreeMap<String, String>) {
    if labels.is_empty() {
        return;
    }
    let mut query = url.query_pairs_mut();
    for (key, value) in labels {
        query.append_pair("label", &format!("{key}:{value}"));
    }
}

async fn server_error(r: reqwest::Response) -> Result<reqwest::Response, Error> {
    let status = r.status();
    if status.is_success() {
        return Ok(r);
    }
    let text = r.text().await.unwrap_or_default();
    Err(server_failure(status, &text))
}

/// The error for a non-2xx response with this body.
fn server_failure(status: reqwest::StatusCode, text: &str) -> Error {
    let (kind, message) = match serde_json::from_str::<ErrorResponse>(text) {
        Ok(e) => (e.error.kind, e.error.message),
        Err(_) => (ErrorKind::Internal, text.to_string()),
    };
    Error::Server {
        status: status.as_u16(),
        kind,
        message,
    }
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

    /// A create that names a spec runs on that spec's version, and an
    /// acceptance has its own time limits: neither takes the client's.
    #[test]
    fn a_spec_or_a_run_takes_none_of_the_clients_defaults() {
        let c = Client::new("http://h/")
            .unwrap()
            .with_version("a")
            .with_timeout(60);
        let named = [
            CreateSnapshotOptions {
                replaces: Some(Replaces {
                    snapshot_id: "b~x".into(),
                    allow: None,
                }),
                ..Default::default()
            },
            CreateSnapshotOptions {
                amends: Some(api::Amends {
                    snapshot_id: "b~x".into(),
                    reason: "r".into(),
                }),
                ..Default::default()
            },
            CreateSnapshotOptions {
                refutes: Some(api::Refutes {
                    snapshot_id: "b~x".into(),
                    target: "t".into(),
                }),
                ..Default::default()
            },
        ];
        for opts in &named {
            assert_eq!(
                c.create_snapshot_request("x", opts).version,
                None,
                "{opts:?}"
            );
        }
        let plain = c.create_snapshot_request("x", &CreateSnapshotOptions::default());
        assert_eq!(plain.version.as_deref(), Some("a"));
        let req = c
            .accept_request(["b~s"], &AcceptOptions::default())
            .unwrap();
        assert_eq!((req.timeout, req.build_timeout), (None, None));
        let asked = AcceptOptions {
            timeout: Some(900),
            ..Default::default()
        };
        assert_eq!(
            c.accept_request(["b~s"], &asked).unwrap().timeout,
            Some(900)
        );
    }
}
