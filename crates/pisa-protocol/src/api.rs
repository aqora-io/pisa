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
//! | `POST /check/batch` | [`BatchRequest`] → [`BatchResponse`] |
//! | `POST /sessions` | [`StartRequest`] → [`StartResponse`] |
//! | `POST /states/{state}/tactic` | [`TacticRequest`] → [`TacticResponse`] |
//! | `POST /sessions/{session}/check` | [`SessionCheckRequest`] → [`CheckResult`] |
//! | `POST /snapshots` | [`SnapshotRequest`] → [`SnapshotResponse`] |
//! | `GET /snapshots` | `?label=key:value&limit=&after=` → [`SnapshotList`] |
//! | `GET /snapshots/{entry}` | → [`SnapshotInfo`] |
//! | `PATCH /snapshots/{entry}` | [`LabelsRequest`] → [`SnapshotSummary`] |
//! | `DELETE /snapshots/{entry}` | → [`Deleted`] |
//! | `POST /snapshots/{entry}/verify` | → [`SnapshotVerified`] |
//! | `GET /jobs/{job}` | `?wait=` ([`JobWait`]) → the request's answer, or [`JobStatus`] (202) |
//! | `DELETE /jobs/{job}` | → [`JobStatus`] |
//!
//! The snapshot endpoints take an optional `?version=` ([`VersionQuery`]).
//! Checks can ask for more in `detail` ([`Detail`]). Checks, sessions, and
//! snapshot creates and verifies run asynchronously with
//! `Prefer: respond-async` ([`PreferAsync`]), answering 202 with a
//! [`JobStatus`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, IntoResponses, ToSchema};

use crate::repl;
use crate::search::Informal;
pub use crate::verify::Declaration;

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
    /// `not_found`: an unknown or expired job, state, session, entry or
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
    /// `/jobs/{job}`, rather than the answer.
    #[param(rename = "Prefer")]
    pub prefer: Option<String>,
}

/// What a request with `Prefer: respond-async` answers.
#[derive(IntoResponses)]
pub enum Accepted {
    /// Queued: wait for its answer at `/jobs/{job}`.
    #[response(
        status = 202,
        headers(
            ("Location" = String, description = "`/jobs/{job}`, relative to the server's root"),
            ("Preference-Applied" = String, description = "`respond-async`"),
        )
    )]
    Accepted(JobStatus),
}

/// An async request, as `/jobs/{job}` reports it until it is done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobStatus {
    /// Its id: 32 hex digits, to keep secret, as anyone who has it can read
    /// the answer or cancel the job.
    pub job: String,
    pub kind: JobKind,
    pub status: JobState,
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
    Tactic,
    /// `POST /sessions/{id}/check`
    CheckSession,
    /// `POST /snapshots`
    CreateSnapshot,
    /// `POST /snapshots/{entry}/verify`
    VerifySnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for a worker.
    Queued,
    /// A worker is running it.
    Running,
    /// Its answer is waiting at `/jobs/{job}`.
    Done,
    /// Withdrawn before a worker started it.
    Cancelled,
}

/// `GET /jobs/{job}`'s query.
#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JobWait {
    /// How long to wait for the job to finish, in seconds: at most 60, and
    /// by default 0, which answers at once.
    #[serde(default)]
    pub wait: Option<u64>,
}

/// What `GET /jobs/{job}` answers once the job is done: what its request
/// would have. By `kind`: `check` and `check_session` a `CheckResult`,
/// `start_session` a `StartResponse`, `tactic` a `TacticResponse`,
/// `create_snapshot` a `SnapshotResponse` and `verify_snapshot` a
/// `SnapshotVerified`. An error is the request's own, with its status.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(untagged)]
pub enum JobOutcome {
    Check(CheckResult),
    Session(StartResponse),
    Tactic(TacticResponse),
    Snapshot(Box<SnapshotResponse>),
    Verified(SnapshotVerified),
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
    pub data: String,
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
}

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
}

/// `POST /check/batch`
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchRequest {
    pub items: Vec<BatchItem>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Applies to every item.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<Detail>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchItem {
    pub id: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchResponse {
    /// In request order.
    pub results: Vec<CheckResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CheckResult {
    /// The item's id, in batch results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
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

/// `POST /sessions`: runs code containing `sorry`; each `sorry` becomes a
/// proof state that tactics can be applied to.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StartRequest {
    /// Code with `sorry`s, each of which becomes a state.
    pub code: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StartResponse {
    pub session: String,
    /// One per `sorry`, in source order.
    pub states: Vec<StateInfo>,
    #[schema(required = true)]
    #[serde(default)]
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StateInfo {
    pub state: String,
    pub goal: String,
    /// Where the `sorry` is in the code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<Pos>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
}

/// `POST /states/{state}/tactic`. A failing tactic is a 422 with kind `lean`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TacticRequest {
    pub tactic: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TacticResponse {
    /// The new state. Earlier states stay valid, so a session is a tree.
    pub state: String,
    pub goals: Vec<String>,
    /// No goals remain for this state's `sorry`. This says nothing about the
    /// rest of the proof; check the assembled proof with the session check.
    pub complete: bool,
    #[schema(required = true)]
    #[serde(default)]
    pub messages: Vec<Message>,
}

/// `POST /sessions/{session}/check`: re-checks completed code from scratch
/// and confirms it proves the statements the session started from, with the
/// same imports.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionCheckRequest {
    pub code: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

/// `POST /snapshots`: checks code like `/check` and, if Lean accepts it,
/// keeps it as a module that later code can import, with a new entry that
/// holds the module. Code that is already kept gets a new entry on its
/// module without being built again.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SnapshotRequest {
    pub code: String,
    #[serde(default)]
    pub version: Option<String>,
    /// Lean's time limit in seconds, for building the module including its
    /// imports.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Whether to re-check the code through the kernel. `true` also re-checks
    /// its imports, so the result ends [`verified`](SnapshotResponse::verified)
    /// or has an error, except on a [`key`](Self::key) replay. `false` never
    /// re-checks. By default, only code that uses macros, its own or an
    /// import's, is re-checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<bool>,
    /// The new entry's labels; see [`check_labels`](crate::labels::check_labels)
    /// for the rules.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Makes the create safe to retry: while an entry made with this key
    /// exists, a create with it on the same code returns that entry as it is,
    /// instead of adding another; on other code it is a `bad_request`. Such a
    /// replay ignores the request's other fields, `labels` and `verify`
    /// included, so it can be `verified: false` with no error after
    /// `verify: true`. Creates that keep nothing don't use the key up, and
    /// deleting the entry frees it. See [`check_key`](crate::labels::check_key)
    /// for the rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SnapshotResponse {
    /// The entry, which the other snapshot endpoints take: the one this create
    /// added, or on a [`key`](SnapshotRequest::key) replay the one the key is
    /// on. It keeps the module until it is deleted. Absent when nothing new
    /// was kept: the code was not kept, or `verify: true` found it kept but
    /// failing the re-check, in which case `snapshot` and `header` are set and
    /// `result` has the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The module to import, e.g. `PisaSnap.s1a2b…`, shared by every entry on
    /// the same code. Absent when the code was not kept: Lean reported errors,
    /// or the check could not complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    /// The header to start code on top of this snapshot with: this code's
    /// imports with its snapshots replaced by the new one. Code with exactly
    /// this header reuses the Lean process the snapshot's builder keeps warm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// The worker pool version the snapshot belongs to.
    pub version: String,
    /// Kept although a declaration uses `sorry` or another axiom outside the
    /// allowed list; `result.reasons` says which.
    pub dirty: bool,
    /// This snapshot and everything it imports have passed the kernel
    /// re-check.
    pub verified: bool,
    /// The entry's labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// The check, as for `/check`. It has no `sorries`, and `time` includes
    /// importing the module's imports.
    pub result: CheckResult,
}

/// `GET /snapshots/{entry}`: an entry and its module.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SnapshotInfo {
    #[schema(required = true)]
    #[serde(default)]
    pub entry: String,
    /// The entry's module.
    pub snapshot: String,
    /// The modules it imports.
    pub imports: Vec<String>,
    /// As in [`SnapshotResponse::header`].
    pub header: String,
    pub code: String,
    /// The entry's labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub dirty: bool,
    /// As in [`SnapshotResponse::verified`].
    pub verified: bool,
    pub result: CheckResult,
    /// When the entry was created, in seconds since the Unix epoch.
    pub created: u64,
    /// The module's declarations, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decls: Vec<ModuleDecl>,
}

/// A declaration of a snapshot module, with its description once search has
/// written one (with `PISA_SEARCH_DESCRIBE_MODEL`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModuleDecl {
    pub name: String,
    pub kind: String,
    /// The declaration's signature, with notation.
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub informal: Option<Informal>,
}

/// An entry in a listing: as [`SnapshotInfo`], without the code and the check
/// result.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SnapshotSummary {
    pub entry: String,
    /// The entry's module.
    pub snapshot: String,
    /// The modules it imports.
    pub imports: Vec<String>,
    /// As in [`SnapshotResponse::header`].
    pub header: String,
    pub labels: BTreeMap<String, String>,
    pub dirty: bool,
    /// As in [`SnapshotResponse::verified`].
    pub verified: bool,
    /// When the entry was created, in seconds since the Unix epoch.
    pub created: u64,
}

/// `GET /snapshots`: the version's entries, newest first, that have every
/// label the query names as `label=key:value` (split on the first `:`; up to
/// 64 filters), at most `limit` of them (1 to 1000, default 100). Keep
/// requesting with `next` as `after` while a page has it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SnapshotList {
    pub entries: Vec<SnapshotSummary>,
    /// Where the next page starts, if this page is full. The page after it
    /// may be empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// `PATCH /snapshots/{entry}`: removes the labels `remove` names, then sets
/// those in `set`. The labels that result must follow the rules of
/// [`check_labels`](crate::labels::check_labels).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LabelsRequest {
    #[serde(default)]
    pub set: BTreeMap<String, String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

/// `POST /snapshots/{entry}/verify`: re-checks the entry's module and
/// everything it imports through the kernel, imports first, skipping what
/// already passed. A module that fails stops the run and stays usable, marked
/// as not verified. `verified` and `dirty` belong to the module, so every
/// entry on it shows the result.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SnapshotVerified {
    #[schema(required = true)]
    #[serde(default)]
    pub entry: String,
    /// The entry's module.
    pub snapshot: String,
    /// As in [`SnapshotResponse::verified`].
    pub verified: bool,
    /// As in [`SnapshotResponse::dirty`], from the re-check if it got this far.
    pub dirty: bool,
    /// The modules this run re-checked and passed, in order.
    pub checked: Vec<String>,
    /// The module that failed, if one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<SnapshotFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SnapshotFailure {
    pub snapshot: String,
    /// Why the re-check failed, e.g. the kernel's error or a timeout.
    pub reason: String,
}

/// `DELETE /snapshots/{entry}`: deletes the entry. Its module goes too once
/// no entry and no kept module needs it, and then so do the modules it
/// imports, on the same terms.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Deleted {
    #[schema(required = true)]
    #[serde(default)]
    pub entry: String,
    /// The modules removed, the entry's own first.
    #[schema(required = true)]
    #[serde(default)]
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct VersionQuery {
    /// The worker version; the server's default when absent.
    #[serde(default)]
    pub version: Option<String>,
}

/// A state key or session ID: `<version>~<header key>~<id>`. Carrying the
/// version and header lets the router route it without a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub version: String,
    pub header_key: String,
    pub id: String,
}

impl Key {
    pub fn parse(s: &str) -> Option<Key> {
        let mut parts = s.split('~');
        let (version, header_key, id) = (parts.next()?, parts.next()?, parts.next()?);
        let ok = |p: &str| {
            !p.is_empty()
                && !p.starts_with('.')
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        };
        if parts.next().is_some() || ![version, header_key, id].into_iter().all(ok) {
            return None;
        }
        Some(Key {
            version: version.into(),
            header_key: header_key.into(),
            id: id.into(),
        })
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}~{}~{}", self.version, self.header_key, self.id)
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
            data: m.data,
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
                              "end_pos": {"line": 1, "column": 6}, "data": "Nat : Type"}],
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
            header_key: "abc".into(),
            id: "0f".into(),
        };
        assert_eq!(Key::parse(&k.to_string()), Some(k));
        assert_eq!(Key::parse("v~h"), None);
        assert_eq!(Key::parse("v~h~a~b"), None);
        assert_eq!(Key::parse("v~h~a/../b"), None);
        assert_eq!(Key::parse("v~~a"), None);
        assert_eq!(Key::parse("v~h~.."), None);
    }
}
