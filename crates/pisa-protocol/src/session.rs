//! Tactic sessions: apply one tactic at a time to stored proof states.
//!
//! A session starts from code containing `sorry`; each `sorry` becomes a proof
//! state. Applying a tactic yields a new state and leaves the old one valid,
//! so a session is a tree. States are pickled into shared storage, so any
//! worker with the same version can continue them.
//!
//! State keys and session IDs carry the pool version and header key, so the
//! router can route them without a lookup: `<version>~<header key>~<id>`.

use serde::{Deserialize, Serialize};

use crate::kimina::CheckResult;
use crate::repl::{Message, Pos};

fn default_timeout() -> u64 {
    60
}

/// `POST /sessions`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartRequest {
    pub code: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StartResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// One per `sorry`, in source order.
    #[serde(default)]
    pub states: Vec<StateInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateInfo {
    pub state: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<Pos>,
    #[serde(rename = "endPos", default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
}

/// `POST /sessions/{state}/tactic`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TacticRequest {
    pub tactic: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TacticResponse {
    /// The new state, absent when the tactic failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default)]
    pub goals: Vec<String>,
    /// The REPL's verdict, e.g. "Completed" or "Incomplete: open goals remain".
    /// "Completed" covers only this goal; use the session check for the proof.
    #[serde(
        rename = "proofStatus",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub proof_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /sessions/{session}/check`: the final, from-scratch check of the
/// completed proof, plus a check that it proves the session's statements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckRequest {
    pub code: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    pub result: CheckResult,
}

/// A parsed state key or session ID.
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

#[cfg(test)]
mod tests {
    use super::*;

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
