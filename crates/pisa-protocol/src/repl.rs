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
    /// `lean --json` spells it "information".
    #[serde(alias = "information")]
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
    /// Read from `infotree` by [`tactics`]. Pisa doesn't send `allTactics`,
    /// so the REPL never fills this itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tactics: Vec<Tactic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infotree: Option<Value>,
}

impl CommandResponse {
    pub fn has_errors(&self) -> bool {
        self.messages.iter().any(|m| m.severity == Severity::Error)
    }

    /// Moves every message, sorry and tactic position down by `lines`, so
    /// positions reported for a body match the original code that had a
    /// header on top.
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
        for t in &mut self.tactics {
            shift(&mut t.pos);
            shift(&mut t.end_pos);
        }
    }
}

/// One run of a tactic written in the code, read from the info tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tactic {
    /// The tactic's source text.
    pub tactic: String,
    pub goals_before: Vec<String>,
    pub goals_after: Vec<String>,
    pub pos: Pos,
    pub end_pos: Pos,
}

/// A node of the info tree the REPL returns for `infotree: "original"`. Only
/// the fields [`tactics`] reads are modelled.
#[derive(Deserialize)]
struct InfoTree {
    #[serde(default)]
    node: Option<TacticNode>,
    #[serde(default)]
    children: Vec<InfoTree>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TacticNode {
    name: Option<String>,
    stx: NodeSyntax,
    goals_before: Vec<String>,
    goals_after: Vec<String>,
}

#[derive(Deserialize)]
struct NodeSyntax {
    range: Range,
}

#[derive(Deserialize)]
struct Range {
    start: Pos,
    finish: Pos,
}

/// Syntax that only structures tactics. Like the REPL's `allTactics`,
/// [`tactics`] leaves it out but keeps the tactics inside it.
const STRUCTURE: &[&str] = &[
    "null",
    "Lean.cdot",
    "Lean.cdotTk",
    "Lean.Parser.Term.byTactic",
    "Lean.Parser.Tactic.tacticSeq",
    "Lean.Parser.Tactic.tacticSeq1Indented",
    "Lean.Parser.Tactic.«tactic_<;>_»",
    "Lean.Parser.Tactic.paren",
];

/// The tactics Lean ran, in order, from the `infotree` of the response to
/// `code` sent with `infotree: "original"`.
pub fn tactics(infotree: &Value, code: &str) -> Result<Vec<Tactic>, serde_json::Error> {
    fn walk(tree: &InfoTree, code: &str, out: &mut Vec<Tactic>) {
        if let Some(node) = &tree.node
            && let Some(name) = &node.name
            && !STRUCTURE.contains(&name.as_str())
        {
            let Range { start, finish } = node.stx.range;
            out.push(Tactic {
                tactic: source(code, start, finish).unwrap_or_default().to_string(),
                goals_before: node.goals_before.clone(),
                goals_after: node.goals_after.clone(),
                pos: start,
                end_pos: finish,
            });
        }
        for child in &tree.children {
            walk(child, code, out);
        }
    }
    let trees: Vec<InfoTree> = serde_json::from_value(infotree.clone())?;
    let mut out = Vec::new();
    for tree in &trees {
        walk(tree, code, &mut out);
    }
    Ok(out)
}

/// The text of `code` between two positions. Lean counts lines from 1 and
/// columns from 0, in characters.
fn source(code: &str, start: Pos, end: Pos) -> Option<&str> {
    let offset = |p: Pos| {
        let line_start = match p.line.checked_sub(1)? {
            0 => 0,
            n => code.match_indices('\n').nth(n as usize - 1)?.0 + 1,
        };
        let line = &code[line_start..];
        let column = line.char_indices().nth(p.column as usize);
        Some(line_start + column.map_or(line.len(), |(i, _)| i))
    };
    code.get(offset(start)?..offset(end)?)
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
    fn lean_json_messages_parse() {
        let m: Message = serde_json::from_value(json!({
            "caption": "", "data": "Nat : Type", "endPos": {"column": 6, "line": 1},
            "fileName": "<stdin>", "isSilent": false, "keepFullRange": false,
            "kind": "[anonymous]", "pos": {"column": 0, "line": 1}, "severity": "information"
        }))
        .unwrap();
        assert_eq!(m.severity, Severity::Info);
        assert_eq!(serde_json::to_value(m.severity).unwrap(), json!("info"));
    }

    #[test]
    fn shift_moves_messages_and_sorries() {
        let mut r: CommandResponse = serde_json::from_value(json!({
            "env": 1,
            "messages": [{"severity": "error", "pos": {"line": 1, "column": 0}, "data": "x"}],
            "sorries": [{"pos": {"line": 2, "column": 3}, "endPos": {"line": 2, "column": 8},
                         "goal": "g", "proofState": 0}],
            "tactics": [{"tactic": "simp", "goals_before": ["g"], "goals_after": [],
                         "pos": {"line": 3, "column": 2}, "end_pos": {"line": 4, "column": 5}}]
        }))
        .unwrap();
        r.shift_lines(3);
        assert_eq!(r.messages[0].pos.line, 4);
        assert_eq!(r.messages[0].end_pos, None);
        assert_eq!(r.sorries[0].pos.unwrap().line, 5);
        assert_eq!(r.sorries[0].end_pos.unwrap().line, 5);
        assert_eq!(r.tactics[0].pos.line, 6);
        assert_eq!(r.tactics[0].end_pos.line, 7);
    }

    /// `testdata/infotree-original.json` is the REPL's info tree for
    /// [`TACTIC_CODE`], with the fields [`tactics`] doesn't read removed.
    const TACTIC_CODE: &str = "theorem u : True ∧ True := by constructor <;> (trivial)";

    #[test]
    fn tactics_come_from_the_info_tree_without_structure() {
        let tree: Value =
            serde_json::from_str(include_str!("testdata/infotree-original.json")).unwrap();
        let got = tactics(&tree, TACTIC_CODE).unwrap();
        let at: Vec<_> = got
            .iter()
            .map(|t| (t.tactic.as_str(), t.pos.column))
            .collect();
        // `<;>` runs `trivial` once per goal; the column counts `∧` as one.
        assert_eq!(at, [("constructor", 30), ("trivial", 47), ("trivial", 47)]);
        assert_eq!(got[0].goals_before, ["⊢ True ∧ True"]);
        assert_eq!(
            got[0].goals_after,
            ["case left\n⊢ True", "case right\n⊢ True"]
        );
        assert_eq!(got[2].goals_before, ["case right\n⊢ True"]);
    }

    #[test]
    fn source_counts_columns_in_characters() {
        let code = "a\n∧ bc\nd";
        let pos = |line, column| Pos { line, column };
        assert_eq!(source(code, pos(2, 2), pos(2, 4)), Some("bc"));
        assert_eq!(source(code, pos(1, 0), pos(3, 1)), Some(code));
        assert_eq!(source(code, pos(4, 0), pos(4, 1)), None);
    }
}
