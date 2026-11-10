//! The client-facing HTTP API.
//!
//! Field names are snake_case throughout. Every error has the same shape,
//! [`ErrorResponse`]. Checks report failures in-band: a check that times out
//! or is rejected still returns 200 with `ok: false` and an `error`, so one
//! bad item never fails a batch. Other endpoints use HTTP statuses, which
//! [`ErrorKind::status`] maps.
//!
//! | Endpoint | Request → response |
//! | --- | --- |
//! | `POST /check` | [`CheckRequest`] → [`CheckResult`] |
//! | `POST /check/batch` | [`CheckBatchRequest`] → [`CheckBatchResponse`] |
//! | `POST /sessions` | [`StartSessionRequest`] → [`Session`] |
//! | `POST /states/{id}/tactic` | [`ApplyTacticRequest`] → [`ApplyTacticResponse`] |
//! | `POST /sessions/{id}/check` | [`CheckSessionRequest`] → [`CheckResult`] |
//! | `POST /snapshots` | [`CreateSnapshotRequest`] → [`CreateSnapshotResponse`] |
//! | `GET /snapshots` | `?label=key:value&limit=&after=` → [`ListSnapshotsResponse`] |
//! | `GET /snapshots/{id}` | → [`SnapshotDetail`] |
//! | `GET /snapshots/{id}/dependents` | `?label=key:value` → [`ListDependentsResponse`] |
//! | `POST /snapshots/rebuild` | [`RebuildSnapshotsRequest`] → [`RebuildSnapshotsResponse`] |
//! | `GET /snapshots/audit` | `?label=key:value` or `?against=<id>` → [`AuditSnapshotsResponse`] |
//! | `PATCH /snapshots/{id}` | [`UpdateSnapshotRequest`] → [`Snapshot`] |
//! | `DELETE /snapshots/{id}` | → [`DeleteSnapshotResponse`] |
//! | `POST /snapshots/{id}/verify` | → [`VerifySnapshotResponse`] |
//! | `GET /snapshots/export` | `?against=<id>&allow=<name>` → a Lake project, as a tar.gz |
//! | `POST /snapshots/comparator` | [`ComparatorRequest`] → [`ComparatorResponse`] |
//! | `POST /documents` | [`OpenDocumentRequest`] → [`Document`] |
//! | `PATCH /documents/{id}` | [`EditDocumentRequest`] → [`Document`] |
//! | `POST /documents/{id}/goals` | [`DocumentGoalsRequest`] → [`DocumentGoals`] |
//! | `POST /documents/{id}/probes` | [`DocumentProbesRequest`] → [`DocumentProbes`] |
//! | `DELETE /documents/{id}` | → 204 |
//! | `GET /jobs/{id}` | `?wait=` ([`JobWait`]) → the request's answer, or [`Job`] (202) |
//! | `DELETE /jobs/{id}` | → [`Job`] |
//!
//! Snapshot ids carry their worker version, as session and state ids do, so
//! the endpoints that take one take no version; listings and audits take an
//! optional `?version=`.
//! Checks can ask for more in `detail` ([`Detail`]). Checks, sessions, and
//! snapshot creates and verifies run asynchronously with
//! `Prefer: respond-async` ([`PreferAsync`]), answering 202 with a
//! [`Job`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, IntoResponses, ToSchema};

use crate::repl;
use crate::search::Description;
pub use crate::verify::{Change, Declaration, DeclarationChange, ProofStatus};

fn default_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Lean ran past the request's time limit.
    Timeout,
    /// Lean exceeded its memory limit.
    OutOfMemory,
    /// The Lean process died.
    Crash,
    /// The verification policy refused the input before running it.
    Policy,
    /// Lean refused the input, e.g. an unknown tactic or a header that does
    /// not elaborate.
    Lean,
    /// No worker was free within the server's wait limit; retry later.
    Capacity,
    /// Workers kept disconnecting while running the job, or the metadata
    /// store failed or stayed contended; retry later.
    Unavailable,
    BadRequest,
    NotFound,
    /// The snapshot a create replaces is already replaced; see
    /// [`fork`](CreateSnapshotRequest::fork).
    Conflict,
    Internal,
}

impl ErrorKind {
    /// The HTTP status used when the error is not reported in-band.
    pub fn status(self) -> u16 {
        match self {
            ErrorKind::Timeout
            | ErrorKind::OutOfMemory
            | ErrorKind::Crash
            | ErrorKind::Policy
            | ErrorKind::Lean => 422,
            ErrorKind::Capacity => 429,
            ErrorKind::Unavailable => 503,
            ErrorKind::BadRequest => 400,
            ErrorKind::NotFound => 404,
            ErrorKind::Conflict => 409,
            ErrorKind::Internal => 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
        }
    }

    pub fn timeout(seconds: u64) -> Self {
        Error::new(
            ErrorKind::Timeout,
            format!("Lean ran for more than {seconds}s"),
        )
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

/// The body of every non-2xx response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorResponse {
    pub error: Error,
}

/// The errors an endpoint may answer with, each an [`ErrorResponse`] whose
/// `kind` gives its status ([`ErrorKind::status`]).
#[derive(IntoResponses)]
pub enum ApiErrors {
    /// `bad_request`: the request breaks the API's rules.
    #[response(status = 400)]
    BadRequest(ErrorResponse),
    /// `not_found`: an unknown or expired job, state, session, snapshot or
    /// declaration.
    #[response(status = 404)]
    NotFound(ErrorResponse),
    /// `lean` or `policy`: refused; `timeout`, `out_of_memory` or `crash`: a
    /// limit was hit.
    #[response(status = 422)]
    Unprocessable(ErrorResponse),
    /// `capacity`: no worker took the job in time; retry later.
    #[response(status = 429)]
    Capacity(ErrorResponse),
    /// `internal`.
    #[response(status = 500)]
    Internal(ErrorResponse),
    /// `unavailable`: e.g. search isn't ready; retry later.
    #[response(status = 503)]
    Unavailable(ErrorResponse),
}

/// The `Prefer` header of a request that may run asynchronously.
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
pub struct PreferAsync {
    /// `respond-async` for a job at once (202), to wait for at
    /// `/jobs/{id}`, rather than the answer.
    #[param(rename = "Prefer")]
    pub prefer: Option<String>,
}

/// What a request with `Prefer: respond-async` answers.
#[derive(IntoResponses)]
pub enum Accepted {
    /// Queued: wait for its answer at `/jobs/{id}`.
    #[response(
        status = 202,
        headers(
            ("Location" = String, description = "`/jobs/{id}`, relative to the server's root"),
            ("Preference-Applied" = String, description = "`respond-async`"),
        )
    )]
    Accepted(Job),
}

/// An async request, as `/jobs/{id}` reports it until it is done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Job {
    /// Its id: 32 hex digits, to keep secret, as anyone who has it can read
    /// the answer or cancel the job.
    pub id: String,
    pub kind: JobKind,
    pub status: JobStatus,
}

/// The endpoint an async request ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// `POST /check`
    Check,
    /// `POST /sessions`
    StartSession,
    /// `POST /states/{id}/tactic`
    ApplyTactic,
    /// `POST /sessions/{id}/check`
    CheckSession,
    /// `POST /snapshots`
    CreateSnapshot,
    /// `POST /snapshots/{id}/verify`
    VerifySnapshot,
    /// `POST /snapshots/rebuild`, run rather than planned
    RebuildSnapshots,
    /// `POST /snapshots/comparator`
    Comparator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Waiting for a worker.
    Queued,
    /// A worker is running it.
    Running,
    /// Its answer is waiting at `/jobs/{id}`.
    Done,
    /// Withdrawn before a worker started it.
    Cancelled,
}

/// `GET /jobs/{id}`'s query.
#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JobWait {
    /// How long to wait for the job to finish, in seconds: at most 60, and
    /// by default 0, which answers at once.
    #[serde(default)]
    pub wait: Option<u64>,
}

/// What `GET /jobs/{id}` answers once the job is done: what its request
/// would have. By `kind`: `check` and `check_session` a `CheckResult`,
/// `start_session` a `Session`, `apply_tactic` an `ApplyTacticResponse`,
/// `create_snapshot` a `CreateSnapshotResponse`, `verify_snapshot` a
/// `VerifySnapshotResponse` and `rebuild_snapshots` a
/// `RebuildSnapshotsResponse`. An error is the request's own, with its
/// status.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(untagged)]
pub enum JobOutcome {
    Check(CheckResult),
    Session(Session),
    ApplyTactic(ApplyTacticResponse),
    CreateSnapshot(Box<CreateSnapshotResponse>),
    VerifySnapshot(VerifySnapshotResponse),
    RebuildSnapshots(Box<RebuildSnapshotsResponse>),
}

/// `GET /health`: 200, or 503 once the server is stopping.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Health {
    /// `draining`, with a 503, once the server is stopping.
    pub status: HealthStatus,
    /// The live workers on the queue; `null` if it can't be read.
    #[schema(required = true)]
    pub workers: Option<u64>,
    /// Search's state; `null` if the server doesn't search.
    #[schema(required = true)]
    pub search: Option<crate::search::SearchHealth>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Ok,
    /// Stopping: load balancers should stop sending requests.
    Draining,
}

/// A position in the submitted code: lines count from 1, and columns from 0
/// in Unicode code points, not UTF-16 units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Pos {
    pub line: u64,
    pub column: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Trace,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Message {
    pub severity: Severity,
    pub pos: Pos,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
    /// What Lean said.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Sorry {
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<Pos>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
}

/// Extra detail a check can return, requested in `detail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    /// Every tactic Lean ran, with the goals before it: [`CheckResult::tactics`].
    Tactics,
    /// With `at`, also the term at each position and the type Lean expected
    /// it to have: [`AtPosition::term`]. Lean then prints every term of the
    /// code, which can take several times as long as the check.
    Terms,
    /// Every field of each of [`CheckResult::declarations`]. Without it a
    /// declaration has its `name`, `kind` and `status`, and only the axioms
    /// it uses outside the worker's allowed ones.
    Declarations,
}

/// The most positions a check can ask about in `at`.
pub const MAX_AT: usize = 64;

/// `POST /check`
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckRequest {
    pub code: String,
    /// Worker pool label or alias; defaults to the server's default.
    #[serde(default)]
    pub version: Option<String>,
    /// Lean's time limit in seconds.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<Detail>,
    /// Up to 64 positions in the code to report on: [`CheckResult::at`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at: Vec<Pos>,
}

/// `POST /check/batch`
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckBatchRequest {
    /// The code to check; each item gets a result, in this order.
    pub items: Vec<CheckBatchItem>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Applies to every item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<Detail>,
    /// Applies to every item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at: Vec<Pos>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckBatchItem {
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckBatchResponse {
    /// In request order.
    pub results: Vec<CheckResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CheckResult {
    /// True when the code elaborated without errors and passed every policy
    /// check: only standard axioms, no metaprogramming, allowed options, and
    /// for session checks, the session's original statements.
    pub ok: bool,
    /// Seconds Lean spent on the code.
    pub time: f64,
    /// Why the check could not complete (timeout, crash, policy, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
    /// Lean's messages, with positions in the submitted code.
    #[schema(required = true)]
    #[serde(default)]
    pub messages: Vec<Message>,
    #[schema(required = true)]
    #[serde(default)]
    pub sorries: Vec<Sorry>,
    /// Why `ok` is false, e.g. "axiom: t uses sorryAx".
    #[schema(required = true)]
    #[serde(default)]
    pub reasons: Vec<String>,
    /// What keeps the code from replacing the snapshot it replaces, or from
    /// keeping a session's statements, each also one of `reasons`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<DeclarationChange>,
    /// Declarations the code added, with the axioms each depends on.
    #[schema(required = true)]
    #[serde(default)]
    pub declarations: Vec<Declaration>,
    /// With `detail: ["tactics"]`, one entry each time a tactic in the code
    /// ran, in the order Lean ran them. A tactic run on several goals, as
    /// under `all_goals` or `<;>`, appears once per run, and so does every
    /// alternative `first` tried.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tactics: Vec<TacticInfo>,
    /// With `at`, what Lean recorded at each position, in request order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at: Vec<AtPosition>,
}

/// What Lean recorded at a position asked about in `at`. Each part is
/// absent when no syntax of its kind written in the code contains the
/// position; the innermost one is reported, the range that starts last,
/// then ends first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AtPosition {
    /// The position asked about.
    pub pos: Pos,
    /// The tactic at the position, with the goals before and after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tactic: Option<TacticInfo>,
    /// With `detail: ["terms"]`, the term at the position that Lean
    /// elaborated against an expected type. Lean gives the head of an
    /// application the application's: on `f` in `f x`, this is `f`,
    /// expecting what `f x` was expected to be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term: Option<TermInfo>,
}

/// A term written in the code, and the type Lean expected it to have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TermInfo {
    /// The term's source text.
    pub term: String,
    pub expected_type: String,
    /// The term as Lean elaborated it.
    pub expr: String,
    pub pos: Pos,
    pub end_pos: Pos,
}

/// One run of a tactic, with the goals before and after it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TacticInfo {
    /// The tactic's source text.
    pub tactic: String,
    pub goals_before: Vec<String>,
    /// The goals left after it ran. A tactic that failed, such as an
    /// alternative `first` dropped, leaves them as they were before.
    pub goals_after: Vec<String>,
    pub pos: Pos,
    pub end_pos: Pos,
}

/// `POST /sessions`: opens code containing `sorry` on Lean Beam; each
/// `sorry` becomes a proof state that tactics can be applied to, in the
/// code's own context.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StartSessionRequest {
    /// Code with `sorry`s, each of which becomes a state.
    pub code: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

/// A proof session: the states that its code's `sorry`s start from.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Session {
    /// Its id, `<version>~<32 hex>`, for checking the finished proof at
    /// `/sessions/{id}/check`. It is also its open document's, for
    /// `/documents/{id}/goals` and `/probes`.
    pub id: String,
    /// One per `sorry`, in source order.
    pub states: Vec<State>,
    /// Lean's errors and warnings about the code.
    #[schema(required = true)]
    #[serde(default)]
    pub messages: Vec<Message>,
}

/// A proof state: the goals still to prove. A state stays valid after
/// tactics are applied to it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct State {
    /// Its id, `<version>~<session hex>~<32 hex>`, for applying a tactic at
    /// `/states/{id}/tactic`. The same tactic on the same state gives the
    /// same state, and so the same id.
    pub id: String,
    /// The goals left, as Lean prints them. A session's first states have
    /// their `sorry`'s goal; a tactic can leave more, or none.
    pub goals: Vec<String>,
    /// Where the `sorry` is in the code. Only a session's first states have
    /// it, not the states that tactics lead to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<Pos>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
}

/// `POST /states/{id}/tactic`. A failing tactic is a 422 with kind `lean`,
/// with Lean's errors in its message, and leaves no state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplyTacticRequest {
    pub tactic: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApplyTacticResponse {
    /// The new state. Earlier states stay valid, so a session is a tree.
    pub state: State,
    /// No goals remain for this state's `sorry`, and the tactic doesn't say
    /// `sorry` or `admit`. This says nothing about the rest of the proof;
    /// check the assembled proof with the session check.
    pub complete: bool,
    #[schema(required = true)]
    #[serde(default)]
    pub messages: Vec<Message>,
}

/// `POST /sessions/{id}/check`: re-checks completed code from scratch
/// and confirms it proves the statements the session started from, with the
/// same imports.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckSessionRequest {
    pub code: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Only `declarations` applies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<Detail>,
}

/// The most candidates a probe can try.
pub const MAX_CANDIDATES: usize = 64;

/// `POST /documents`: opens code on Lean Beam, for the goals at positions in
/// it and candidate tactics probed there. A worker keeps it open, and the
/// code is stored so that another can reopen it. Nothing a document says is a
/// verdict: check finished code with `/check`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OpenDocumentRequest {
    pub code: String,
    #[serde(default)]
    pub version: Option<String>,
    /// Lean's time limit in seconds.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

/// `PATCH /documents/{id}`: replaces the document's code.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EditDocumentRequest {
    pub code: String,
    /// The revision the edit replaces; if the document is at another, the
    /// edit is a 409 `conflict`.
    pub revision: u64,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

/// An open document, as Lean elaborated its code.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Document {
    /// Its id, `<version>~<32 hex>`.
    pub id: String,
    /// 1 when opened, then one more for each edit.
    pub revision: u64,
    /// Lean's errors and warnings.
    #[schema(required = true)]
    #[serde(default)]
    pub messages: Vec<Message>,
    /// The code's `sorry`s in source order, each with its goals.
    #[schema(required = true)]
    #[serde(default)]
    pub sorries: Vec<Sorry>,
}

/// A position in a document's code, as for [`Pos`]. Without `column`, it is
/// the line's first tactic: its first character that isn't blank, after a
/// `·` or `.` bullet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LinePos {
    pub line: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
}

/// Goals before the tactic at a position, or after it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalsMode {
    #[default]
    Before,
    After,
}

/// `POST /documents/{id}/goals`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DocumentGoalsRequest {
    /// The revision asked about; if the document is at another, a 409
    /// `conflict`. Without it, the latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// 1 to [`MAX_AT`] positions.
    pub at: Vec<LinePos>,
    #[serde(default)]
    pub mode: GoalsMode,
    /// Lean's time limit in seconds, which reopening the document counts
    /// against.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DocumentGoals {
    pub revision: u64,
    /// One per position asked about, in order.
    pub at: Vec<GoalsAt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GoalsAt {
    /// The position, its column filled in.
    pub pos: Pos,
    /// The goals there, as Lean prints them: none in the imports or outside
    /// a proof.
    #[schema(required = true)]
    #[serde(default)]
    pub goals: Vec<String>,
}

/// Where probes run: at one of the document's `sorry`s, by its index in
/// [`Document::sorries`], or at a position in the code's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum ProbeAt {
    Sorry { sorry: usize },
    Pos(LinePos),
}

/// `POST /documents/{id}/probes`: runs each candidate at one position, each
/// on the document as it is: nothing a candidate does stays.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DocumentProbesRequest {
    /// As for goals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    pub at: ProbeAt,
    /// 1 to [`MAX_CANDIDATES`] tactics.
    pub candidates: Vec<String>,
    /// Lean's time limit in seconds, for all of them.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DocumentProbes {
    pub revision: u64,
    /// Where the candidates ran.
    pub pos: Pos,
    /// One per candidate, in order.
    pub results: Vec<ProbeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProbeResult {
    /// It ran without errors.
    pub ok: bool,
    /// It ran without errors, left no goals, and doesn't say `sorry` or
    /// `admit`. This is no verdict: Lean Beam checks neither termination nor
    /// what the tactic elaborated to, so check the finished code with
    /// `/check`.
    pub closed: bool,
    /// The goals it left, when it ran without errors.
    #[schema(required = true)]
    #[serde(default)]
    pub goals: Vec<String>,
    #[schema(required = true)]
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Why it didn't run: the policy refused it, or the time ran out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

/// `POST /snapshots`: checks code like `/check` and, if Lean accepts it,
/// keeps it as a module that later code can import, with a new snapshot of
/// it. Code that is already kept gets a new snapshot of its module without
/// being built again.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSnapshotRequest {
    pub code: String,
    /// Worker pool label or alias. By default, the version of the snapshot
    /// `replaces` names, if any, else the server's default. A version other
    /// than that snapshot's is a `bad_request`.
    #[serde(default)]
    pub version: Option<String>,
    /// Lean's time limit in seconds, for building the module, and again for
    /// each module the kernel re-check covers, once their imports are loaded.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Whether to re-check the code through the kernel. By default the code
    /// is re-checked, and its imports are not. `true` also re-checks its
    /// imports, so the snapshot ends [`verified`](Snapshot::verified) or the
    /// check has an error, except on a [`key`](Self::key) replay. `false`
    /// skips the re-check, except for module-system code, which only the
    /// re-check can describe: the statuses are then the build's own report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<bool>,
    /// The new snapshot's labels; see
    /// [`check_labels`](crate::labels::check_labels) for the rules.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Makes the create safe to retry: while a snapshot made with this key
    /// exists, a create with it on the same code returns that snapshot as it
    /// is, instead of adding another; on other code it is a `bad_request`.
    /// Such a replay ignores the request's other fields, `labels` and
    /// `verify` included, so it can be `verified: false` with no error after
    /// `verify: true`. Creates that keep nothing don't use the key up, and
    /// deleting the snapshot frees it. See
    /// [`check_key`](crate::labels::check_key) for the rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The snapshot this code replaces, e.g. the skeleton a proof fills in.
    /// The code is kept only if it still declares everything that snapshot's
    /// module declares, each the same kind of declaration with the same type
    /// and, for definitions, the same value unless `allow` or that
    /// snapshot's [`holes`](Snapshot::holes) name them, and adds no
    /// assumptions (`axiom`s, its own or a newly imported
    /// snapshot's); it may add other declarations and imports. If it no
    /// longer builds on a snapshot that module does, what the module's
    /// declarations use from there must be unchanged in what the code builds
    /// on now. Otherwise nothing is kept and `check.reasons` lists the
    /// changes. The new snapshot records the link. The replaced snapshot
    /// must be a head, one that no snapshot replaces, unless `fork` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<Replaces>,
    /// Lets `replaces` name a snapshot that another snapshot replaces
    /// already, which forks the run. Otherwise such a create keeps nothing
    /// and builds nothing: `check.error` is a `conflict`, and the response's
    /// `heads` are the snapshots to replace instead. Another create can
    /// replace the snapshot while this one builds, and then it is refused
    /// the same way, with its build discarded. A `key` replay comes before
    /// this check. It does nothing without `replaces`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fork: bool,
    /// The definitions a spec snapshot leaves for the snapshots that replace
    /// it to change, its holes, named as [`allow`](Allow) names them:
    /// definitions or inductive types of its module or of a snapshot it
    /// builds on. The snapshots that replace it inherit them and may change
    /// them without `allow`, and a [spec audit](AuditSnapshotsResponse)
    /// allows them to change. Not with `replaces`, as a replacement
    /// inherits the replaced snapshot's, and not empty (`bad_request`); a
    /// name that isn't such a definition keeps nothing (`check.error` is a
    /// `bad_request`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holes: Option<Vec<String>>,
    /// Only `declarations` applies, to `check`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<Detail>,
}

/// A snapshot that a create replaces, and what the replacement may change:
/// a create's [`replaces`](CreateSnapshotRequest::replaces), and the link the
/// new snapshot keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Replaces {
    /// The replaced snapshot. A create's is of the create's version, and
    /// chooses it when the create names none, ahead of the server's default.
    pub snapshot_id: String,
    /// The definitions the replacement may change besides adding
    /// declarations; by default none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Allow>,
}

/// What a replacement may change besides adding declarations: the values of
/// the named definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Allow {
    /// The definitions whose value may change, e.g. to add a missing
    /// hypothesis; their types and theorems' types stay the same. A theorem's
    /// type only names a definition, so its meaning can change: the response
    /// lists the theorems that use a changed definition. Each is a definition
    /// of the replaced snapshot's module or of a snapshot the code no longer
    /// builds on, else the create's `check.error` is a `bad_request`, found
    /// before the code is built, and nothing is kept. Names are compared
    /// with a private one's module left out, so a `private def h` is
    /// `_private.h`, or its full name as a refusal gives it, but not `h`, as
    /// the error for `h` says. An empty list is a `bad_request`: to change
    /// none, leave `allow` out.
    pub definitions: Vec<String>,
}

/// What a replacement changed, as the comparison with the snapshot it
/// [`replaces`](CreateSnapshotRequest::replaces) found: its create's, or the
/// union of two, for a link composed when the snapshot between was deleted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Replacement {
    /// The replaced module's declarations that used `sorry` and no longer
    /// do.
    #[schema(required = true)]
    #[serde(default)]
    pub filled: Vec<String>,
    /// The definitions whose value changed, each of them one that `allow`
    /// names, and the inductive types `allow` names whose constructors, or
    /// what Lean makes from them, changed, each followed by those that did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_definitions: Vec<String>,
    /// The theorems whose type uses a changed definition, directly or
    /// through another definition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected: Vec<String>,
}

/// Kept code: a module that later code imports, with labels. Every snapshot
/// of the same code shares one module, which stays while a snapshot of it, or
/// a kept module that imports it, needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Snapshot {
    /// Its id, `<version>~<32 lowercase hex>`, which the other snapshot
    /// endpoints take. It carries the version, so they need no other.
    pub id: String,
    /// The worker pool version it belongs to.
    pub version: String,
    /// The module to import, e.g. `PisaSnap.s1a2b…`, shared by every snapshot
    /// of the same code.
    pub module: String,
    /// The header to start code on top of this snapshot with: its code's
    /// imports with its snapshots replaced by this one. Code with exactly
    /// this header reuses the Lean process the snapshot's builder keeps warm.
    pub header: String,
    /// The modules its module imports.
    pub imports: Vec<String>,
    pub labels: BTreeMap<String, String>,
    /// The snapshot it replaced, if it was created to. If that one was
    /// deleted, and it replaced one, the snapshot that one replaced, with the
    /// two links composed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<Replaces>,
    /// The definitions it leaves for the snapshots that replace it to
    /// change: a spec snapshot's [`holes`](CreateSnapshotRequest::holes), or
    /// those of the snapshot it replaced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<String>,
    /// The worst status among its module's declarations, by the build's
    /// report or, once `verified`, the kernel re-check.
    pub status: ProofStatus,
    /// Its module and everything it imports have passed the kernel re-check.
    pub verified: bool,
    /// When it was created, in seconds since the Unix epoch.
    pub created: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateSnapshotResponse {
    /// The snapshot this create added, or on a
    /// [`key`](CreateSnapshotRequest::key) replay the one the key is on.
    /// Absent when nothing new was kept: Lean reported errors, the check
    /// could not complete, the code changed what the snapshot it
    /// [`replaces`](CreateSnapshotRequest::replaces) states (`check.reasons`
    /// says how), the snapshot it replaces is replaced already (a
    /// `conflict`, see `heads`), or `verify: true` found it kept but failing
    /// the re-check (`check.error` says why).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    /// The check, as for `/check`. It has no `sorries`, and `time` includes
    /// importing the module's imports.
    pub check: CheckResult,
    /// What the replacement changed, if the request named a snapshot it
    /// replaces and the code was kept. Absent on a key replay, which doesn't
    /// compare again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<Replacement>,
    /// On a `conflict` (`check.error`), the snapshots to replace instead:
    /// those that nothing replaces, reached from the replaced snapshot by
    /// following what replaces it, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heads: Vec<String>,
    /// Set when the kernel re-check didn't finish, as it ran out of time or
    /// memory, for code outside the module system with no macros, its own or
    /// an import's: the snapshot is kept, not
    /// [`verified`](Snapshot::verified), with the statuses of the build's own
    /// report. `POST /snapshots/{id}/verify` finishes the re-check. Other
    /// code isn't kept when its re-check doesn't finish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified: Option<VerifyFailure>,
}

/// `GET /snapshots/{id}`: a snapshot with its module's code, check and
/// exports.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SnapshotDetail {
    #[serde(flatten)]
    pub snapshot: Snapshot,
    pub code: String,
    /// The module's check, as in [`CreateSnapshotResponse::check`], with the
    /// kernel re-check's axioms once it is verified.
    pub check: CheckResult,
    /// The declarations that code importing the module sees, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<Export>,
}

/// A declaration of a snapshot's module, as code that imports it sees it,
/// with its description once search has written one (with
/// `PISA_SEARCH_DESCRIBE_MODEL`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Export {
    pub name: String,
    pub kind: String,
    /// The declaration's signature, with notation.
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Description>,
}

/// `GET /snapshots`: the version's snapshots, newest first, that have every
/// label the query names as `label=key:value` (split on the first `:`; up to
/// 64 filters), at most `limit` of them (1 to 1000, default 100). With
/// `heads=true`, only the snapshots that no other snapshot replaces, which
/// are a run's current versions. Keep requesting with `next` as `after`
/// while a page has it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ListSnapshotsResponse {
    pub snapshots: Vec<Snapshot>,
    /// Where the next page starts, if this page is full. The page after it
    /// may be empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// `GET /snapshots/{id}/dependents`: the snapshots of every module that
/// imports the snapshot's module, directly or through others, parents first:
/// by level, where a module that imports it is at level 1 and one that
/// imports that at 2, by its longest chain, then oldest first. With
/// `label=key:value` filters, as in a listing, only the snapshots with every
/// label.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ListDependentsResponse {
    pub snapshots: Vec<Snapshot>,
}

/// `POST /snapshots/rebuild`: plans how to copy each head among the
/// dependents of `from` (see [`ListDependentsResponse`]), a snapshot nothing
/// replaces, onto `to`, with its imports of `from`'s module, and of the other
/// modules it rebuilds, swapped for the new ones, and unless `dry_run`
/// creates them: a level at a time, a level's creates side by side once the
/// level below is done. A module a head builds on above `from` is rebuilt
/// too, as a support item that leaves no snapshot behind (see
/// [`RebuildItem`]). `from` and `to` are of one version, which the rebuild
/// runs on.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RebuildSnapshotsRequest {
    /// The snapshot being replaced.
    pub from: String,
    /// Its replacement, of `from`'s version.
    pub to: String,
    /// Which dependents to rebuild: those with a head that has every label,
    /// and the modules they build on above `from`. Required, so that the
    /// caller chooses which projects follow the edit.
    pub labels: BTreeMap<String, String>,
    /// Only plan: the client creates the plan's items.
    #[serde(default)]
    pub dry_run: bool,
    /// Plan even though `to` changes what `from` states, when `to` wasn't
    /// created to [`replace`](CreateSnapshotRequest::replaces) `from`.
    #[serde(default)]
    pub allow_changes: bool,
    /// Once every item is created, delete the old snapshots whose copies
    /// replace them, from the top down, and with them the old modules
    /// nothing else needs. Each delete splices the run, so it goes on
    /// through the copy. An old snapshot whose copy has no link stays, and
    /// so does `from`: delete it yourself if nothing needs it.
    #[serde(default)]
    pub retire: bool,
    /// As [`CreateSnapshotRequest::verify`], for each create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<bool>,
    /// As [`CreateSnapshotRequest::timeout`], for each create.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

/// What a rebuild planned and, unless it was a dry run, did. Sending the same
/// rebuild again plans the same items with the same keys, so the creates that
/// are done replay their snapshots, and the rest run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct RebuildSnapshotsResponse {
    /// For a plan, whether there is one: false when `to` changes what `from`
    /// states and `allow_changes` isn't set. For a run, whether every item
    /// was created, and retired if asked.
    pub ok: bool,
    /// How `to` differs from `from`, as in a refused replacement's
    /// `check.reasons`. Empty when `to` replaces `from`: its create compared
    /// them.
    #[schema(required = true)]
    #[serde(default)]
    pub changes: Vec<String>,
    /// The definitions whose value `to` changed: when `to` replaces `from`,
    /// among those its `allow` names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_definitions: Vec<String>,
    /// The creates to make, by level, then oldest first. A level's items can
    /// be created together once the level below is done.
    #[schema(required = true)]
    #[serde(default)]
    pub plan: Vec<RebuildItem>,
    /// The snapshots with every label that the plan leaves out because a
    /// snapshot without every label replaces them, on dry runs and runs
    /// alike: their runs went on under other labels, which a rebuild with
    /// their heads' labels follows. It doesn't change `ok`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced: Vec<RebuildReplaced>,
    /// For a run, the items created, in plan order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created: Vec<RebuildCreated>,
    /// For a run, the items whose create kept nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<RebuildFailed>,
    /// For a run, the items not created because they build on one that
    /// failed or was skipped. Other branches go on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<RebuildSkipped>,
    /// With `retire`, the ids of the old snapshots deleted, top down.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired: Vec<String>,
}

/// A labelled snapshot left out of a rebuild because one without every label
/// replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RebuildReplaced {
    /// The snapshot left out.
    pub snapshot_id: String,
    /// The snapshots without every label that replace it, oldest first.
    pub replaced_by: Vec<String>,
}

/// A [`RebuildItem`] created.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RebuildCreated {
    /// As in [`RebuildItem::snapshot_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    pub module: String,
    /// The snapshot the create made, or replayed. Absent for a support item:
    /// its snapshot was deleted once every level had run, and its module
    /// stays only while a created snapshot builds on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_snapshot_id: Option<String>,
    pub new_module: String,
}

/// A [`RebuildItem`] whose create kept nothing.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RebuildFailed {
    /// As in [`RebuildItem::snapshot_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    pub module: String,
    /// The create's check: Lean's errors, a refused replacement's
    /// `reasons`, or the `error` that stopped it.
    pub check: CheckResult,
}

/// A [`RebuildItem`] not created, because it builds on one that wasn't.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RebuildSkipped {
    /// As in [`RebuildItem::snapshot_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    pub module: String,
}

/// A create in a [`RebuildSnapshotsResponse`]: `POST /snapshots` with its
/// `code`, `labels`, `key` and `replaces`.
///
/// An item without `snapshot_id` is a support item: a module that a copy
/// builds on, such as one between a head and `from`, or an obsolete
/// snapshot's that a head imports. It has no labels and no `replaces`. Create
/// it as given so that it exists while the items above it are built, then
/// delete its snapshot once they are created. The module then stays only
/// while a created snapshot builds on it, and appears in no listing, search or
/// audit.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RebuildItem {
    /// The wave its create runs in: one more than the highest level among
    /// the items whose modules it imports, so 1 for one that imports only
    /// `from`'s module.
    pub level: usize,
    /// The snapshot it copies: a head with the labels, or one that only its
    /// copies from this same rebuild replace, so that sending the rebuild
    /// again replays them. Absent for a support item, which copies none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// The module it rebuilds.
    pub module: String,
    /// The module the create makes.
    pub new_module: String,
    /// The header of code on top of the new module.
    pub new_header: String,
    /// The old module's code with its imports swapped.
    pub code: String,
    /// The labels to create it with: those of the snapshot it copies, none
    /// for a support item.
    pub labels: BTreeMap<String, String>,
    /// The idempotency key to create it with. Planning again gives the same
    /// keys, so running a plan again after a failure replays the creates
    /// that are done.
    pub key: String,
    /// What the new snapshot replaces, to send as the create's `replaces`:
    /// the snapshot it copies, so the run goes on through the copy. The plan
    /// runs the comparison the create will, taking the copy and the new
    /// modules it builds on to declare what their originals do, and `allow`
    /// names the definitions it found changed, if any. Absent when the
    /// comparison fails, as when `to` changes the type of something the
    /// copy's statements use: the copy then belongs to no run, and `retire`
    /// keeps the old snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<Replaces>,
    /// The theorems of the module whose type uses a changed definition,
    /// directly or through other definitions, here or below.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected: Vec<String>,
}

/// `GET /snapshots/audit`: what the snapshots that have every label the
/// query names (as in a listing) rest on. Statuses come from the build's
/// axiom report, or the kernel re-check once a module is verified, so a
/// trusted audit has no `rejected` or `sorry` and `unverified_modules` 0.
/// Whether an assumption is reasonable is for review.
///
/// With `against=<snapshot id>` instead of labels, repeatable, it audits a
/// run against its spec, the snapshots named. From each, it follows the
/// snapshots that [`replace`](CreateSnapshotRequest::replaces) it, whatever
/// their labels, to the run's `heads`, and judges each spec declaration on
/// each head it reaches. The counts are then those of the heads' snapshots.
/// The ids are of one version, which the audit uses, and `label` doesn't
/// combine with them. A spec snapshot that doesn't exist is `not_found`.
/// The heads may change a spec snapshot's
/// [`holes`](CreateSnapshotRequest::holes), and the definitions
/// `allow=<name>`, repeatable, names: those the leader allows later. The
/// links' own allows, which whoever created them wrote, don't count.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct AuditSnapshotsResponse {
    pub summary: AuditSummary,
    /// The assumptions what the snapshots declare uses, by name, wherever
    /// they are declared.
    pub assumptions: Vec<AuditAssumption>,
    /// The declarations that are `sorry` or `rejected`.
    pub not_proved: Vec<AuditUnproved>,
    /// With `against`: whether the run is done. There is no fork, no spec
    /// snapshot was copied from one that is gone (as a rebuild with `retire`
    /// leaves it), every [comparison](Self::comparisons) found the spec's
    /// statements kept, with no definition changed but the spec's holes and
    /// those the audit's `allow` names, and every target is
    /// [accepted](AuditTarget::accepted) on its head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clean: Option<bool>,
    /// With `against`: the snapshots the run ends at, which nothing
    /// replaces, each once, in the order the walk reached them. Deleting a
    /// head makes the snapshot it replaced a head again, unless something
    /// else replaces it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heads: Vec<String>,
    /// With `against`: the snapshots on the run that more than one snapshot
    /// replaces. The run isn't clean until all but one branch are deleted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forks: Vec<AuditFork>,
    /// With `against`: each declaration of a spec snapshot's module, other
    /// than its assumptions and what Lean generated, on each head reached
    /// from that spec snapshot. A replacement keeps every declaration of what
    /// it replaces, so every head carries them all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<AuditTarget>,
    /// With `against`: the links that allowed definitions to change, for
    /// review: those on the way from the spec snapshots to the heads, and
    /// those of the spec snapshots' own chains back through what they
    /// replaced, as far as those snapshots exist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<AuditLink>,
    /// With `against`: each head compared with each spec snapshot it is
    /// reached from, from their verified records, as a create that replaced
    /// the spec snapshot with the head's code would compare them, allowing
    /// only the spec's holes and the definitions the audit's `allow` names to
    /// change. Links record
    /// what a create's build reported, and what its request allowed; this is
    /// what acceptance rests on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comparisons: Vec<AuditComparison>,
    /// With `against` and `review=true`: for each spec snapshot and head it
    /// reaches, what a reviewer reads to know what the spec's statements say
    /// on the head.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review: Vec<AuditReview>,
}

/// A head compared with a spec snapshot it is reached from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditComparison {
    /// The spec snapshot.
    pub spec: String,
    /// The head.
    pub head: String,
    /// Why the head doesn't keep the spec's statements, as a refused create
    /// says it, e.g. `statement: t has a different type`. Empty if it keeps
    /// them, or if either record isn't verified, which leaves the targets
    /// unaccepted anyway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    /// The same, as the declarations that changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<DeclarationChange>,
    /// What the head changed of the spec's holes and of what the audit's
    /// `allow` names, and what it filled, if it keeps the spec's statements
    /// and both are verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<Replacement>,
}

/// A tar.gz archive, as `GET /snapshots/export` answers it.
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct Tarball(pub Vec<u8>);

/// `POST /snapshots/comparator`: judges a run with `lake comparator`,
/// which shares no code with the audit, on the Lake project
/// `GET /snapshots/export` gives: each spec snapshot against each head it
/// reaches.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ComparatorRequest {
    /// The spec's snapshot ids, as for the audit.
    pub against: Vec<String>,
    /// The definitions the heads may change besides the spec's holes, as for
    /// the audit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Also run the external kernels Lean bundles: `leanchecker-paranoid`,
    /// `lean4lean`, `nanoda`, `con-leche` and `con-ron`.
    #[serde(default = "paranoid_default")]
    pub paranoid: bool,
    /// The time limit in seconds for each spec snapshot and head, in which
    /// comparator builds and exports both and checks the head.
    #[serde(default = "comparator_timeout")]
    pub timeout: u64,
}

fn paranoid_default() -> bool {
    true
}

fn comparator_timeout() -> u64 {
    600
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComparatorResponse {
    /// There is no fork, and comparator accepted every pair.
    pub accepted: bool,
    /// As in the audit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forks: Vec<AuditFork>,
    /// Each spec snapshot with each head it reaches.
    pub pairs: Vec<ComparatorPair>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ComparatorPair {
    /// The spec snapshot.
    pub spec: String,
    /// The head.
    pub head: String,
    /// Its comparator configuration in the export.
    pub config: String,
    pub accepted: bool,
    /// Comparator's: 0 accepted, 1 rejected, 2 couldn't start. None when it
    /// ran past `timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The end of what it printed.
    pub output: String,
    /// Targets, holes and assumptions the configuration leaves out because
    /// they are private, which comparator can't name across two modules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// What a spec snapshot's statements depend on, on one head it reaches: the
/// declarations reached from the spec module's declarations through their
/// types and the values of definitions and inductive types, as the head and
/// what it builds on declare them. Theorems' proofs aren't followed, and
/// axioms are leaves. The text is what the builds printed, for reading; the
/// [comparison](AuditComparison) from verified records is what acceptance
/// rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditReview {
    /// The spec snapshot.
    pub spec: String,
    /// The head.
    pub head: String,
    /// The spec's declarations, then those new in the head, then those of
    /// the snapshots below, module by module.
    pub declarations: Vec<ReviewDeclaration>,
    /// The constants of the version's libraries (Lean's, Mathlib's and the
    /// like) they use, by name, which the version fixes.
    #[schema(required = true)]
    #[serde(default)]
    pub library: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReviewDeclaration {
    pub name: String,
    pub kind: String,
    pub origin: ReviewOrigin,
    /// A snapshot of its module, for one from below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// As the build printed it, with notation. A private declaration has
    /// none, as the build's export leaves private names out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Its type, with full names and no notation.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub type_full: Option<String>,
    /// One of the spec's holes, or a definition the audit's `allow` names.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hole: bool,
    /// A hole's value on the head, as the build printed it with full names
    /// and no notation, cut at 4,000 characters. None from a build before
    /// values were printed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// Where a reviewed declaration is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOrigin {
    /// The spec snapshot's module, and the head as it keeps it.
    Spec,
    /// The head's module, which the spec's doesn't declare.
    Head,
    /// A snapshot below the head.
    Snapshot,
}

/// A snapshot that more than one snapshot replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditFork {
    /// The snapshot replaced more than once.
    pub snapshot_id: String,
    /// The snapshots that replace it, oldest first.
    pub replaced_by: Vec<String>,
}

/// A spec declaration, as a head of the run declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditTarget {
    /// Its name as the spec snapshot's module declares it, so a private
    /// one's names the spec's module. The head's declaration is the one
    /// with the same name, a private one's module left out.
    pub name: String,
    /// The spec snapshot that declares it.
    pub spec: String,
    /// The head, reached from `spec`, whose declaration this is.
    pub head: String,
    /// Its status on the head.
    pub status: ProofStatus,
    /// The head's module has passed the kernel re-check.
    pub verified: bool,
    /// What a clean run needs of it: the spec snapshot's module and the
    /// head's are verified, so that both records come from the kernel
    /// re-check rather than from a build's report; the head keeps the spec's
    /// statements (see [`AuditSnapshotsResponse::comparisons`]); and the
    /// target is `proved`, or `conditional` on assumptions the spec permits
    /// (see [`AuditAssumption::in_spec`]).
    pub accepted: bool,
}

/// A link that let definitions change: `snapshot_id` replaces another
/// snapshot with an `allow`, and a comparison found what changed: its
/// create's, or the union of two if the snapshot between was deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditLink {
    /// The replacing snapshot.
    pub snapshot_id: String,
    /// Its link, as its [`Snapshot::replaces`] shows it: the replaced
    /// snapshot, which may have been deleted since, and the `allow`.
    pub replaces: Replaces,
    /// What the comparison with the replaced snapshot found: its create's, as
    /// [`CreateSnapshotResponse::replacement`] gave it, or, if the snapshot
    /// between was deleted, the union of the two links' comparisons.
    pub replacement: Replacement,
}

/// The snapshots' declarations by status, each module's once, without the
/// assumptions themselves and what Lean generated.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct AuditSummary {
    pub proved: u64,
    pub conditional: u64,
    pub sorry: u64,
    pub rejected: u64,
    /// The snapshots' modules that haven't passed the kernel re-check, so
    /// whose statuses come from the build's report.
    pub unverified_modules: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditAssumption {
    pub name: String,
    /// As written, from its module's declaration export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// A snapshot of the module that declares it: one with the labels, or
    /// a head, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// Its module has passed the kernel re-check.
    pub verified: bool,
    /// The snapshots' declarations that rest on it.
    pub used_by: Vec<String>,
    /// With `against`: whether the spec permits it, as declared in a spec
    /// snapshot's module or in a module that one builds on. Names are
    /// compared with a private one's module left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_spec: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditUnproved {
    pub name: String,
    /// A snapshot with the labels, or a head, whose module declares it.
    pub snapshot_id: String,
    pub status: ProofStatus,
    /// Its axioms outside the allowed list.
    pub axioms: Vec<String>,
}

/// `PATCH /snapshots/{id}`: changes the snapshot's labels as a JSON merge
/// patch, and answers the [`Snapshot`]. The labels that result must follow
/// the rules of [`check_labels`](crate::labels::check_labels), or nothing
/// changes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateSnapshotRequest {
    /// A label set to `null` is removed, one set to a string takes that
    /// value, and those not named stay as they are.
    pub labels: BTreeMap<String, Option<String>>,
}

/// `POST /snapshots/{id}/verify`: re-checks the snapshot's module and
/// everything it imports through the kernel, imports first, skipping what
/// already passed. A module that fails stops the run and stays usable,
/// marked as not verified. `verified` and `status` belong to the module, so
/// every snapshot of it shows the result.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VerifySnapshotResponse {
    /// The snapshot, as the re-check left it.
    pub snapshot: Snapshot,
    /// The modules this run re-checked and passed, in order.
    pub checked: Vec<String>,
    /// The module that failed, if one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<VerifyFailure>,
}

/// A module that failed the kernel re-check.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VerifyFailure {
    pub module: String,
    /// Why the re-check failed, e.g. the kernel's error or a timeout.
    pub reason: String,
}

/// `DELETE /snapshots/{id}`: deletes the snapshot. Its module goes too once
/// no snapshot and no kept module needs it, and then so do the modules it
/// imports, on the same terms. The snapshots that replace it replace what it
/// replaced instead, with the two links composed: their `allow` and
/// `replacement` hold the changes of both steps. If it replaced nothing, they
/// keep their links to it.
///
/// With `with_replacers=true`, it deletes the snapshot and every snapshot
/// that replaces it, directly or through others, in one step, and splices
/// nothing: a whole branch of a run goes. If the snapshot replaced another,
/// the branch no longer counts among that one's replacers, which is a head
/// again if none are left.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeleteSnapshotResponse {
    /// The modules removed, each before the modules it imports: the
    /// snapshot's own first, unless a snapshot deleted with it imports it.
    #[schema(required = true)]
    #[serde(default)]
    pub removed: Vec<String>,
    /// The ids of the snapshots deleted, the one asked for first. It is the
    /// one alone unless `with_replacers` was set.
    #[schema(required = true)]
    #[serde(default)]
    pub deleted: Vec<String>,
}

/// Whether `p` can be a part of a [`Key`] or a [`SnapshotId`]: non-empty
/// `[A-Za-z0-9._-]`, not starting with `.`, so that no part is a path trick.
fn is_id_part(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('.')
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// A state's id: `<version>~<session>~<id>`, where the session is its open
/// document's id within the version. Carrying the version and the document
/// lets the router route it without a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub version: String,
    /// The session's document.
    pub session: String,
    pub id: String,
}

impl Key {
    pub fn parse(s: &str) -> Option<Key> {
        let mut parts = s.split('~');
        let (version, session, id) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || ![version, session, id].into_iter().all(is_id_part) {
            return None;
        }
        Some(Key {
            version: version.into(),
            session: session.into(),
            id: id.into(),
        })
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}~{}~{}", self.version, self.session, self.id)
    }
}

/// A snapshot id, `<version>~<32 lowercase hex>`; the hex is the entry's row
/// id. Carrying the version lets the router route it without a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotId {
    pub version: String,
    pub entry: String,
}

impl SnapshotId {
    pub fn parse(s: &str) -> Option<SnapshotId> {
        let (version, entry) = s.split_once('~')?;
        if !is_id_part(version) || !crate::labels::is_entry_id(entry) {
            return None;
        }
        Some(SnapshotId {
            version: version.into(),
            entry: entry.into(),
        })
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}~{}", self.version, self.entry)
    }
}

/// A document id, `<version>~<32 lowercase hex>`, routed like a
/// [`SnapshotId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentId {
    pub version: String,
    pub id: String,
}

impl DocumentId {
    pub fn parse(s: &str) -> Option<DocumentId> {
        let (version, id) = s.split_once('~')?;
        if !is_id_part(version) || !crate::labels::is_entry_id(id) {
            return None;
        }
        Some(DocumentId {
            version: version.into(),
            id: id.into(),
        })
    }
}

impl std::fmt::Display for DocumentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}~{}", self.version, self.id)
    }
}

impl From<repl::Pos> for Pos {
    fn from(p: repl::Pos) -> Self {
        Pos {
            line: p.line,
            column: p.column,
        }
    }
}

impl From<repl::Severity> for Severity {
    fn from(s: repl::Severity) -> Self {
        match s {
            repl::Severity::Trace => Severity::Trace,
            repl::Severity::Info => Severity::Info,
            repl::Severity::Warning => Severity::Warning,
            repl::Severity::Error => Severity::Error,
        }
    }
}

impl From<repl::Message> for Message {
    fn from(m: repl::Message) -> Self {
        Message {
            severity: m.severity.into(),
            pos: m.pos.into(),
            end_pos: m.end_pos.map(Into::into),
            text: m.data,
        }
    }
}

impl From<repl::Sorry> for Sorry {
    fn from(s: repl::Sorry) -> Self {
        Sorry {
            goal: s.goal,
            pos: s.pos.map(Into::into),
            end_pos: s.end_pos.map(Into::into),
        }
    }
}

impl From<repl::Tactic> for TacticInfo {
    fn from(t: repl::Tactic) -> Self {
        TacticInfo {
            tactic: t.tactic,
            goals_before: t.goals_before,
            goals_after: t.goals_after,
            pos: t.pos.into(),
            end_pos: t.end_pos.into(),
        }
    }
}

impl From<repl::At> for AtPosition {
    fn from(a: repl::At) -> Self {
        AtPosition {
            pos: a.pos.into(),
            tactic: a.tactic.map(Into::into),
            term: a.term.map(|t| TermInfo {
                term: t.term,
                expected_type: t.expected_type,
                expr: t.expr,
                pos: t.pos.into(),
                end_pos: t.end_pos.into(),
            }),
        }
    }
}

pub fn messages(ms: Vec<repl::Message>) -> Vec<Message> {
    ms.into_iter().map(Into::into).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_default_their_timeout() {
        let r: CheckRequest = serde_json::from_value(json!({"code": "#check Nat"})).unwrap();
        assert_eq!(r.timeout, 60);
        assert_eq!(r.version, None);
    }

    #[test]
    fn results_are_snake_case_and_omit_absent_fields() {
        let r = CheckResult {
            ok: false,
            time: 1.0,
            error: Some(Error::timeout(1)),
            messages: messages(vec![repl::Message {
                severity: repl::Severity::Info,
                pos: repl::Pos { line: 1, column: 0 },
                end_pos: Some(repl::Pos { line: 1, column: 6 }),
                data: "Nat : Type".into(),
            }]),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({
                "ok": false, "time": 1.0,
                "error": {"kind": "timeout", "message": "Lean ran for more than 1s"},
                "messages": [{"severity": "info", "pos": {"line": 1, "column": 0},
                              "end_pos": {"line": 1, "column": 6}, "text": "Nat : Type"}],
                "sorries": [], "reasons": [], "declarations": []
            })
        );
        assert_eq!(ErrorKind::OutOfMemory.status(), 422);
        assert_eq!(
            serde_json::to_value(ErrorKind::OutOfMemory).unwrap(),
            json!("out_of_memory")
        );
    }

    #[test]
    fn keys_round_trip_and_reject_path_tricks() {
        let k = Key {
            version: "v4.35.0-1a2b3c4d".into(),
            session: "abc".into(),
            id: "0f".into(),
        };
        assert_eq!(Key::parse(&k.to_string()), Some(k));
        assert_eq!(Key::parse("v~h"), None);
        assert_eq!(Key::parse("v~h~a~b"), None);
        assert_eq!(Key::parse("v~h~a/../b"), None);
        assert_eq!(Key::parse("v~~a"), None);
        assert_eq!(Key::parse("v~h~.."), None);
    }

    #[test]
    fn snapshot_ids_round_trip_and_reject_old_and_odd_ids() {
        let hex = "0123456789abcdef0123456789abcdef";
        let id = SnapshotId {
            version: "v4.35.0-1a2b3c4d".into(),
            entry: hex.into(),
        };
        assert_eq!(id.to_string(), format!("v4.35.0-1a2b3c4d~{hex}"));
        assert_eq!(SnapshotId::parse(&id.to_string()), Some(id));
        for bad in [
            hex.to_string(),
            format!("PisaSnap.s{hex}"),
            "a~b~c".into(),
            "v~..".into(),
            format!("v~{}", &hex[1..]),
            format!("v~{}", hex.to_uppercase()),
        ] {
            assert_eq!(SnapshotId::parse(&bad), None, "{bad}");
        }
    }

    #[test]
    fn document_ids_round_trip() {
        let hex = "0123456789abcdef0123456789abcdef";
        let id = DocumentId::parse(&format!("v~{hex}")).unwrap();
        assert_eq!((id.version.as_str(), id.id.as_str()), ("v", hex));
        assert_eq!(id.to_string(), format!("v~{hex}"));
        assert_eq!(DocumentId::parse(hex), None);
        assert_eq!(DocumentId::parse(&format!("v~{}", &hex[1..])), None);
    }

    #[test]
    fn a_probe_is_at_a_sorry_or_a_position() {
        let at = |v| serde_json::from_value::<ProbeAt>(v).unwrap();
        assert_eq!(at(json!({"sorry": 1})), ProbeAt::Sorry { sorry: 1 });
        assert_eq!(
            at(json!({"line": 3})),
            ProbeAt::Pos(LinePos {
                line: 3,
                column: None
            })
        );
        assert_eq!(
            at(json!({"line": 3, "column": 2})),
            ProbeAt::Pos(LinePos {
                line: 3,
                column: Some(2)
            })
        );
        assert!(serde_json::from_value::<ProbeAt>(json!({"column": 2})).is_err());
        let r: DocumentGoalsRequest = serde_json::from_value(json!({"at": [{"line": 1}]})).unwrap();
        assert_eq!(
            (r.mode, r.revision, r.timeout),
            (GoalsMode::Before, None, 60)
        );
    }
}
