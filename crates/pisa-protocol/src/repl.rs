//! Types for the JSON protocol of the Lean REPL (leanprover-community/repl, v4.35.0).
//!
//! Commands are written as one JSON line followed by a blank line. Responses are
//! pretty-printed JSON values terminated by a blank line. Fields are omitted when
//! empty, and key order is not stable, so everything optional has a default.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ReplCommand {
    Tactic {
        tactic: String,
        #[serde(rename = "proofState")]
        proof_state: u64,
    },
    PickleEnv {
        #[serde(rename = "pickleTo")]
        pickle_to: String,
        env: u64,
    },
    PickleProofState {
        #[serde(rename = "pickleTo")]
        pickle_to: String,
        #[serde(rename = "proofState")]
        proof_state: u64,
    },
    UnpickleEnv {
        #[serde(rename = "unpickleEnvFrom")]
        unpickle_env_from: String,
    },
    UnpickleProofState {
        #[serde(rename = "unpickleProofStateFrom")]
        unpickle_proof_state_from: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<u64>,
    },
    Cmd {
        cmd: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<u64>,
        #[serde(rename = "allTactics", skip_serializing_if = "Option::is_none")]
        all_tactics: Option<bool>,
        #[serde(rename = "rootGoals", skip_serializing_if = "Option::is_none")]
        root_goals: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        infotree: Option<String>,
    },
}

impl ReplCommand {
    pub fn cmd(cmd: impl Into<String>, env: Option<u64>) -> Self {
        ReplCommand::Cmd {
            cmd: cmd.into(),
            env,
            all_tactics: None,
            root_goals: None,
            infotree: None,
        }
    }
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
    #[serde(rename = "endPos", default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sorry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<Pos>,
    #[serde(rename = "endPos", default, skip_serializing_if = "Option::is_none")]
    pub end_pos: Option<Pos>,
    pub goal: String,
    #[serde(
        rename = "proofState",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub proof_state: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResponse {
    pub env: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sorries: Vec<Sorry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tactics: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infotree: Option<Value>,
}

impl CommandResponse {
    pub fn has_errors(&self) -> bool {
        self.messages.iter().any(|m| m.severity == Severity::Error)
    }

    /// Moves every message and sorry position down by `lines`, so positions
    /// reported for a body match the original code that had a header on top.
    pub fn shift_lines(&mut self, lines: u64) {
        if lines == 0 {
            return;
        }
        let shift = |p: &mut Pos| p.line += lines;
        for m in &mut self.messages {
            shift(&mut m.pos);
            m.end_pos.as_mut().map(shift);
        }
        for s in &mut self.sorries {
            s.pos.as_mut().map(shift);
            s.end_pos.as_mut().map(shift);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProofStepResponse {
    #[serde(rename = "proofState")]
    pub proof_state: u64,
    pub goals: Vec<String>,
    #[serde(rename = "proofStatus")]
    pub proof_status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sorries: Vec<Sorry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traces: Vec<String>,
}

/// An error the REPL reports without dying, such as "Unknown environment.".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReplResponse {
    ProofStep(ProofStepResponse),
    Command(CommandResponse),
    Error(ReplError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commands_serialize_like_the_repl_expects() {
        let cases = [
            (
                ReplCommand::cmd("import Mathlib", None),
                json!({"cmd": "import Mathlib"}),
            ),
            (
                ReplCommand::cmd("example : 1 = 1 := rfl", Some(3)),
                json!({"cmd": "example : 1 = 1 := rfl", "env": 3}),
            ),
            (
                ReplCommand::Tactic {
                    tactic: "simp".into(),
                    proof_state: 2,
                },
                json!({"tactic": "simp", "proofState": 2}),
            ),
            (
                ReplCommand::PickleProofState {
                    pickle_to: "a.olean".into(),
                    proof_state: 1,
                },
                json!({"pickleTo": "a.olean", "proofState": 1}),
            ),
            (
                ReplCommand::UnpickleProofState {
                    unpickle_proof_state_from: "a.olean".into(),
                    env: Some(0),
                },
                json!({"unpickleProofStateFrom": "a.olean", "env": 0}),
            ),
        ];
        for (cmd, expected) in cases {
            assert_eq!(serde_json::to_value(&cmd).unwrap(), expected);
        }
    }

    #[test]
    fn responses_parse_by_shape() {
        let cmd: ReplResponse = serde_json::from_value(json!({
            "sorries": [{"proofState": 0, "pos": {"line": 1, "column": 20},
                         "goal": "⊢ 1 = 1", "endPos": {"line": 1, "column": 25}}],
            "messages": [{"severity": "warning", "pos": {"line": 1, "column": 8},
                          "endPos": {"line": 1, "column": 9},
                          "data": "declaration uses 'sorry'"}],
            "env": 1
        }))
        .unwrap();
        let ReplResponse::Command(cmd) = cmd else {
            panic!("{cmd:?}")
        };
        assert_eq!(cmd.env, 1);
        assert_eq!(cmd.sorries[0].proof_state, Some(0));

        let step: ReplResponse = serde_json::from_value(json!({
            "proofStatus": "Completed", "proofState": 4, "goals": []
        }))
        .unwrap();
        assert!(matches!(step, ReplResponse::ProofStep(ref s) if s.proof_status == "Completed"));

        let err: ReplResponse =
            serde_json::from_value(json!({"message": "Unknown environment."})).unwrap();
        assert!(matches!(err, ReplResponse::Error(_)));
    }

    #[test]
    fn shift_moves_messages_and_sorries() {
        let mut r: CommandResponse = serde_json::from_value(json!({
            "env": 1,
            "messages": [{"severity": "error", "pos": {"line": 1, "column": 0}, "data": "x"}],
            "sorries": [{"pos": {"line": 2, "column": 3}, "endPos": {"line": 2, "column": 8},
                         "goal": "g", "proofState": 0}]
        }))
        .unwrap();
        r.shift_lines(3);
        assert_eq!(r.messages[0].pos.line, 4);
        assert_eq!(r.messages[0].end_pos, None);
        assert_eq!(r.sorries[0].pos.unwrap().line, 5);
        assert_eq!(r.sorries[0].end_pos.unwrap().line, 5);
    }
}
