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

/// Ordered by line, then column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    /// Read from `infotree` by [`at_positions`]; the REPL never sends it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at: Vec<At>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infotree: Option<Value>,
}

impl CommandResponse {
    pub fn has_errors(&self) -> bool {
        self.messages.iter().any(|m| m.severity == Severity::Error)
    }

    /// Moves every message, sorry, tactic and term position down by `lines`,
    /// so positions reported for a body match the original code that had a
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
        for a in &mut self.at {
            shift(&mut a.pos);
            if let Some(t) = &mut a.tactic {
                shift(&mut t.pos);
                shift(&mut t.end_pos);
            }
            if let Some(t) = &mut a.term {
                shift(&mut t.pos);
                shift(&mut t.end_pos);
            }
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

/// A term written in the code, read from the info tree: the innermost one
/// at a position that Lean elaborated against an expected type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Term {
    /// The term's source text.
    pub term: String,
    pub expected_type: String,
    /// The term as Lean elaborated it.
    pub expr: String,
    pub pos: Pos,
    pub end_pos: Pos,
}

/// What the info tree says at a position, read by [`at_positions`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct At {
    pub pos: Pos,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tactic: Option<Tactic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term: Option<Term>,
}

/// A node of the info tree the REPL returns for `infotree`. Only the fields
/// [`tactics`] and [`at_positions`] read are modelled. Tactic, term and
/// command nodes carry a `node`; other kinds, and holes, carry none.
#[derive(Deserialize)]
struct InfoTree {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    node: Option<Node>,
    #[serde(default)]
    children: Vec<InfoTree>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    #[serde(default)]
    name: Option<String>,
    stx: NodeSyntax,
    #[serde(default)]
    goals_before: Vec<String>,
    #[serde(default)]
    goals_after: Vec<String>,
    #[serde(default)]
    expected_type: Option<String>,
    #[serde(default)]
    expr: Option<String>,
}

#[derive(Deserialize)]
struct NodeSyntax {
    range: Range,
}

#[derive(Deserialize)]
struct Range {
    /// Syntax Lean made, not syntax written in the code.
    #[serde(default)]
    synthetic: bool,
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

/// The nodes of syntax written in the code, in tree order: what
/// `infotree: "original"` keeps of the tactics, and of the rest of a
/// `"full"` tree.
fn original(infotree: &Value) -> Result<Vec<Original>, serde_json::Error> {
    fn walk(tree: InfoTree, out: &mut Vec<Original>) {
        if let (Some(kind), Some(node)) = (tree.kind, tree.node)
            && !node.stx.range.synthetic
        {
            out.push(Original { kind, node });
        }
        for child in tree.children {
            walk(child, out);
        }
    }
    let trees = Vec::<InfoTree>::deserialize(infotree)?;
    let mut out = Vec::new();
    for tree in trees {
        walk(tree, &mut out);
    }
    Ok(out)
}

struct Original {
    kind: String,
    node: Node,
}

impl Original {
    /// A tactic Lean ran, not syntax that only structures tactics.
    fn is_tactic(&self) -> bool {
        self.kind == "TacticInfo"
            && self
                .node
                .name
                .as_deref()
                .is_some_and(|name| !STRUCTURE.contains(&name))
    }

    fn tactic(&self, code: &str) -> Tactic {
        let Range { start, finish, .. } = self.node.stx.range;
        Tactic {
            tactic: source(code, start, finish).unwrap_or_default().to_string(),
            goals_before: self.node.goals_before.clone(),
            goals_after: self.node.goals_after.clone(),
            pos: start,
            end_pos: finish,
        }
    }

    /// A term Lean elaborated against an expected type.
    fn is_typed_term(&self) -> bool {
        self.kind == "TermInfo" && self.node.expected_type.is_some()
    }

    fn term(&self, code: &str) -> Option<Term> {
        let Range { start, finish, .. } = self.node.stx.range;
        Some(Term {
            term: source(code, start, finish).unwrap_or_default().to_string(),
            expected_type: self.node.expected_type.clone()?,
            expr: self.node.expr.clone().unwrap_or_default(),
            pos: start,
            end_pos: finish,
        })
    }

    fn contains(&self, pos: Pos) -> bool {
        let range = &self.node.stx.range;
        range.start <= pos && pos <= range.finish
    }
}

/// The tactics Lean ran, in order, from the `infotree` of the response to
/// `code` sent with `infotree: "original"` or `"full"`.
pub fn tactics(infotree: &Value, code: &str) -> Result<Vec<Tactic>, serde_json::Error> {
    Ok(original(infotree)?
        .iter()
        .filter(|o| o.is_tactic())
        .map(|o| o.tactic(code))
        .collect())
}

/// What the `infotree` of the response to `code` says at each of `positions`:
/// the innermost tactic whose range contains it, and with `terms` the
/// innermost term that has an expected type. Innermost is the range that
/// starts last, then ends first; of several on one range, the outermost
/// node, so a constant's term, not its head (`rfl`, not `@rfl`). Terms need
/// a `"full"` tree.
pub fn at_positions(
    infotree: &Value,
    code: &str,
    positions: &[Pos],
    terms: bool,
) -> Result<Vec<At>, serde_json::Error> {
    let nodes = original(infotree)?;
    let innermost = |pos: Pos, keep: &dyn Fn(&Original) -> bool| {
        let mut best: Option<&Original> = None;
        for o in nodes.iter().filter(|o| o.contains(pos) && keep(o)) {
            let key = |o: &Original| {
                let range = &o.node.stx.range;
                (range.start, std::cmp::Reverse(range.finish))
            };
            if best.is_none_or(|b| key(o) > key(b)) {
                best = Some(o);
            }
        }
        best
    };
    Ok(positions
        .iter()
        .map(|&pos| At {
            pos,
            tactic: innermost(pos, &|o| o.is_tactic()).map(|o| o.tactic(code)),
            term: terms
                .then(|| innermost(pos, &|o| o.is_typed_term()))
                .flatten()
                .and_then(|o| o.term(code)),
        })
        .collect())
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
        let tactic = r.tactics[0].clone();
        let term = Term {
            term: "h".into(),
            expected_type: "g".into(),
            expr: "h".into(),
            pos: tactic.pos,
            end_pos: tactic.end_pos,
        };
        r.at = vec![At {
            pos: tactic.pos,
            tactic: Some(tactic),
            term: Some(term),
        }];
        r.shift_lines(3);
        assert_eq!(r.messages[0].pos.line, 4);
        assert_eq!(r.messages[0].end_pos, None);
        assert_eq!(r.sorries[0].pos.unwrap().line, 5);
        assert_eq!(r.sorries[0].end_pos.unwrap().line, 5);
        assert_eq!(r.tactics[0].pos.line, 6);
        assert_eq!(r.tactics[0].end_pos.line, 7);
        let at = &r.at[0];
        let (tactic, term) = (at.tactic.as_ref().unwrap(), at.term.as_ref().unwrap());
        assert_eq!(
            [at.pos, tactic.pos, tactic.end_pos, term.pos, term.end_pos].map(|p| p.line),
            [6, 6, 7, 6, 7]
        );
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

    /// `testdata/infotree-full.json` is the REPL's `"full"` info tree for
    /// [`FULL_CODE`], with the fields nothing reads removed.
    const FULL_CODE: &str = "theorem t (a b : Nat) (h : a = b) : b = a := by
  have h2 : b = a := h.symm
  exact h2

theorem s (n : Nat) : n + 0 = n := rfl";

    fn full_tree() -> Value {
        serde_json::from_str(include_str!("testdata/infotree-full.json")).unwrap()
    }

    fn pos(line: u64, column: u64) -> Pos {
        Pos { line, column }
    }

    #[test]
    fn a_full_tree_has_the_tactics_of_an_original_one() {
        // What the REPL returns for `FULL_CODE` with `infotree: "original"`.
        let got = tactics(&full_tree(), FULL_CODE).unwrap();
        let at: Vec<_> = got
            .iter()
            .map(|t| (t.tactic.as_str(), t.pos, t.end_pos))
            .collect();
        assert_eq!(
            at,
            [
                ("have h2 : b = a := h.symm", pos(2, 2), pos(2, 27)),
                ("exact h2", pos(3, 2), pos(3, 10)),
            ]
        );
        let after_have = "a b : Nat\nh : a = b\nh2 : b = a\n⊢ b = a";
        assert_eq!(got[0].goals_after, [after_have]);
        assert_eq!(got[1].goals_before, [after_have]);
    }

    #[test]
    fn a_position_gets_the_innermost_tactic_and_typed_term() {
        let positions = [
            pos(3, 9),
            pos(2, 25),
            pos(2, 2),
            pos(5, 36),
            pos(1, 18),
            pos(4, 0),
        ];
        let at = at_positions(&full_tree(), FULL_CODE, &positions, true).unwrap();
        let got: Vec<_> = at
            .iter()
            .map(|a| {
                let tactic = a.tactic.as_ref().map(|t| t.tactic.as_str());
                let term = a
                    .term
                    .as_ref()
                    .map(|t| (t.term.as_str(), t.expected_type.as_str(), t.expr.as_str()));
                (tactic, term)
            })
            .collect();
        let have = Some("have h2 : b = a := h.symm");
        assert_eq!(
            got,
            [
                // An `exact` argument has the goal as its expected type.
                (Some("exact h2"), Some(("h2", "b = a", "h2"))),
                // Inside a `have` value: `symm` alone has no expected type.
                (have, Some(("h.symm", "b = a", "Eq.symm h"))),
                // The terms Lean made for `have` itself are synthetic.
                (have, None),
                // A term-mode proof: `rfl` and its head `@rfl` share a range.
                (None, Some(("rfl", "n + 0 = n", "rfl"))),
                // A binder's type.
                (None, Some(("Nat", "Type", "Nat"))),
                // A blank line between the declarations.
                (None, None),
            ]
        );
        assert_eq!(at.iter().map(|a| a.pos).collect::<Vec<_>>(), positions);
        assert_eq!(at[3].term.as_ref().unwrap().pos, pos(5, 35));
    }

    #[test]
    fn a_position_gets_no_term_unless_asked() {
        let at = at_positions(&full_tree(), FULL_CODE, &[pos(3, 9)], false).unwrap();
        assert_eq!(at[0].tactic.as_ref().unwrap().tactic, "exact h2");
        assert_eq!(at[0].term, None);

        // An original tree has tactics alone. `<;>` runs `trivial` once per
        // goal, on one range: the first run is the one returned.
        let tree: Value =
            serde_json::from_str(include_str!("testdata/infotree-original.json")).unwrap();
        let at = at_positions(&tree, TACTIC_CODE, &[pos(1, 33), pos(1, 48)], true).unwrap();
        let constructor = at[0].tactic.as_ref().unwrap();
        let trivial = at[1].tactic.as_ref().unwrap();
        assert_eq!(constructor.tactic, "constructor");
        assert_eq!(
            (trivial.tactic.as_str(), trivial.goals_before.as_slice()),
            ("trivial", ["case left\n⊢ True".to_string()].as_slice())
        );
        assert_eq!((&at[0].term, &at[1].term), (&None, &None));
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
