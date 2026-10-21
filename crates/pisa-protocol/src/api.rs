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

use serde::{Deserialize, Serialize};

use crate::repl;
pub use crate::verify::Declaration;

fn default_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Workers kept disconnecting while running the job; retry later.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pos {
    pub line: u64,
    pub column: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Trace,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub severity: Severity,
    pub pos: Pos,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sorry {
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<Pos>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
}

/// `POST /check`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckRequest {
    pub code: String,
    /// Worker pool label or alias; defaults to the server's default.
    #[serde(default)]
    pub version: Option<String>,
    /// Lean's time limit in seconds.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

/// `POST /check/batch`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest {
    pub items: Vec<BatchItem>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchItem {
    pub id: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResponse {
    /// In request order.
    pub results: Vec<CheckResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub sorries: Vec<Sorry>,
    /// Why `ok` is false, e.g. "axiom: t uses sorryAx".
    #[serde(default)]
    pub reasons: Vec<String>,
    /// Declarations the code added, with the axioms each depends on.
    #[serde(default)]
    pub declarations: Vec<Declaration>,
}

/// `POST /sessions`: runs code containing `sorry`; each `sorry` becomes a
/// proof state that tactics can be applied to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartRequest {
    pub code: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartResponse {
    pub session: String,
    /// One per `sorry`, in source order.
    pub states: Vec<StateInfo>,
    #[serde(default)]
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateInfo {
    pub state: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<Pos>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
}

/// `POST /states/{state}/tactic`. A failing tactic is a 422 with kind `lean`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TacticRequest {
    pub tactic: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TacticResponse {
    /// The new state. Earlier states stay valid, so a session is a tree.
    pub state: String,
    pub goals: Vec<String>,
    /// No goals remain for this state's `sorry`. This says nothing about the
    /// rest of the proof; check the assembled proof with the session check.
    pub complete: bool,
    #[serde(default)]
    pub messages: Vec<Message>,
}

/// `POST /sessions/{session}/check`: re-checks completed code from scratch
/// and confirms it proves the statements the session started from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCheckRequest {
    pub code: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
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
