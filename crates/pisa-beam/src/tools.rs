//! Typed wrappers for the Beam tools pisa uses. Beam rejects undeclared
//! argument fields, so each request is built from exactly the fields its
//! schema names (`testdata/tools-list.json`); results keep only the fields
//! pisa reads and ignore the rest.

use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::client::{Beam, BeamError};
use crate::pos::LspPos;

/// A `.lean` file of a workspace, by its path relative to the root.
#[derive(Debug, Clone, Copy)]
pub struct Doc<'a> {
    pub root: &'a Path,
    pub path: &'a str,
}

/// Goals before or after the tactic at a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Before,
    After,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Before => "before",
            Mode::After => "after",
        }
    }
}

/// A speculative state to continue from: opaque, and valid only in the
/// process and document version that made it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Handle(pub Value);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Update {
    pub version: u64,
    #[serde(default)]
    pub changed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sync {
    pub version: u64,
    #[serde(default)]
    pub diagnostics: Diagnostics,
    #[serde(default)]
    pub readiness: Option<Readiness>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Diagnostics {
    #[serde(default)]
    pub items: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub message: String,
    pub severity: String,
    #[serde(default)]
    pub range: Option<Range>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: LspPos,
    pub end: LspPos,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Readiness {
    #[serde(default)]
    pub save_ready: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    #[serde(default)]
    pub items: Vec<TodoItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub kind: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub range: Option<Range>,
    /// Where to probe from to work on this item.
    #[serde(default)]
    pub run_at_position: Option<LspPos>,
    #[serde(default)]
    pub proof_state: Option<ProofState>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProofState {
    #[serde(default)]
    pub goals: Vec<Goal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub goal_prefix: String,
    #[serde(default)]
    pub hyps: Vec<Hyp>,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hyp {
    pub names: Vec<String>,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// The outcome of a probe (`run_at`, `run_with`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub success: bool,
    #[serde(default)]
    pub messages: Vec<RunMessage>,
    /// Absent after a command probe.
    #[serde(default)]
    pub proof_state: Option<ProofState>,
    /// Set by the handle-keeping probes when they succeed.
    #[serde(default)]
    pub next_handle: Option<Handle>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunMessage {
    pub severity: String,
    pub text: String,
    #[serde(default)]
    pub range: Option<Range>,
}

/// The module artifacts `save` wrote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Save {
    pub module: String,
    pub olean: String,
    #[serde(default)]
    pub ilean: Option<String>,
    #[serde(default)]
    pub source_hash: Option<String>,
}

fn doc_args(doc: Doc<'_>) -> Map<String, Value> {
    let mut args = Map::new();
    args.insert("workspace".into(), json!({"root": doc.root}));
    args.insert("path".into(), json!(doc.path));
    args
}

fn at_args(doc: Doc<'_>, version: u64, pos: LspPos) -> Map<String, Value> {
    let mut args = doc_args(doc);
    args.insert("version".into(), json!(version));
    args.insert("line".into(), json!(pos.line));
    args.insert("character".into(), json!(pos.character));
    args
}

pub(crate) fn update_args(doc: Doc<'_>) -> Value {
    Value::Object(doc_args(doc))
}

/// Waits for diagnostics; returns errors and warnings with `all`, errors otherwise.
pub(crate) fn sync_args(doc: Doc<'_>, all: bool) -> Value {
    let mut args = doc_args(doc);
    let scope = if all { "all" } else { "errors" };
    args.insert("diagnostic_scope".into(), json!(scope));
    args.insert("diagnostics_in_result".into(), json!(true));
    Value::Object(args)
}

pub(crate) fn todo_args(doc: Doc<'_>, version: u64, kinds: &[&str]) -> Value {
    let mut args = doc_args(doc);
    args.insert("version".into(), json!(version));
    if !kinds.is_empty() {
        args.insert("kinds".into(), json!(kinds));
    }
    Value::Object(args)
}

pub(crate) fn goals_args(doc: Doc<'_>, version: u64, pos: LspPos, mode: Mode) -> Value {
    let mut args = at_args(doc, version, pos);
    args.insert("mode".into(), json!(mode.as_str()));
    Value::Object(args)
}

pub(crate) fn run_at_args(doc: Doc<'_>, version: u64, pos: LspPos, text: &str) -> Value {
    let mut args = at_args(doc, version, pos);
    args.insert("text".into(), json!(text));
    Value::Object(args)
}

pub(crate) fn run_with_args(doc: Doc<'_>, handle: &Handle, text: &str) -> Value {
    let mut args = doc_args(doc);
    args.insert("handle".into(), handle.0.clone());
    args.insert("text".into(), json!(text));
    Value::Object(args)
}

pub(crate) fn release_args(doc: Doc<'_>, handle: &Handle) -> Value {
    let mut args = doc_args(doc);
    args.insert("handle".into(), handle.0.clone());
    Value::Object(args)
}

pub(crate) fn save_args(doc: Doc<'_>) -> Value {
    let mut args = doc_args(doc);
    args.insert("diagnostic_scope".into(), json!("errors"));
    Value::Object(args)
}

pub(crate) fn close_args(doc: Doc<'_>) -> Value {
    Value::Object(doc_args(doc))
}

impl Beam {
    async fn typed<T: DeserializeOwned>(
        &self,
        tool: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<T, BeamError> {
        let value = self.call(tool, args, timeout).await?;
        serde_json::from_value(value).map_err(|e| BeamError::Protocol(format!("{tool}: {e}")))
    }

    /// Reads the file from disk into Beam's mirror; returns at once, without
    /// waiting for diagnostics.
    pub async fn update(&self, doc: Doc<'_>, timeout: Duration) -> Result<Update, BeamError> {
        self.typed("lean_update", update_args(doc), timeout).await
    }

    /// Like [`update`](Self::update), then waits until the file is elaborated.
    pub async fn sync(
        &self,
        doc: Doc<'_>,
        all: bool,
        timeout: Duration,
    ) -> Result<Sync, BeamError> {
        self.typed("lean_sync", sync_args(doc, all), timeout).await
    }

    /// The problems left in the file: `sorry`s, holes, errors. `kinds` narrows
    /// them (e.g. `["sorry"]`); empty means all.
    pub async fn todo(
        &self,
        doc: Doc<'_>,
        version: u64,
        kinds: &[&str],
        timeout: Duration,
    ) -> Result<Todo, BeamError> {
        self.typed("lean_todo", todo_args(doc, version, kinds), timeout)
            .await
    }

    pub async fn goals(
        &self,
        doc: Doc<'_>,
        version: u64,
        pos: LspPos,
        mode: Mode,
        timeout: Duration,
    ) -> Result<Vec<Goal>, BeamError> {
        let state: ProofState = self
            .typed("lean_goals", goals_args(doc, version, pos, mode), timeout)
            .await?;
        Ok(state.goals)
    }

    /// Runs `text` (a tactic block, or one command) at `pos`; keeps nothing.
    pub async fn run_at(
        &self,
        doc: Doc<'_>,
        version: u64,
        pos: LspPos,
        text: &str,
        timeout: Duration,
    ) -> Result<RunResult, BeamError> {
        self.typed("lean_run_at", run_at_args(doc, version, pos, text), timeout)
            .await
    }

    /// Like [`run_at`](Self::run_at), and keeps the result as `next_handle`.
    pub async fn run_at_handle(
        &self,
        doc: Doc<'_>,
        version: u64,
        pos: LspPos,
        text: &str,
        timeout: Duration,
    ) -> Result<RunResult, BeamError> {
        let args = run_at_args(doc, version, pos, text);
        self.typed("lean_run_at_handle", args, timeout).await
    }

    /// Runs `text` from `handle`, which stays valid.
    pub async fn run_with(
        &self,
        doc: Doc<'_>,
        handle: &Handle,
        text: &str,
        timeout: Duration,
    ) -> Result<RunResult, BeamError> {
        let args = run_with_args(doc, handle, text);
        self.typed("lean_run_with", args, timeout).await
    }

    /// Runs `text` from `handle` and consumes it, whether or not it succeeds.
    pub async fn run_with_linear(
        &self,
        doc: Doc<'_>,
        handle: &Handle,
        text: &str,
        timeout: Duration,
    ) -> Result<RunResult, BeamError> {
        let args = run_with_args(doc, handle, text);
        self.typed("lean_run_with_linear", args, timeout).await
    }

    pub async fn release(
        &self,
        doc: Doc<'_>,
        handle: &Handle,
        timeout: Duration,
    ) -> Result<(), BeamError> {
        self.call("lean_release", release_args(doc, handle), timeout)
            .await
            .map(drop)
    }

    /// Writes the file's module artifacts into the workspace's Lake build
    /// directory, from the state Beam already elaborated.
    pub async fn save(&self, doc: Doc<'_>, timeout: Duration) -> Result<Save, BeamError> {
        self.typed("lean_save", save_args(doc), timeout).await
    }

    pub async fn close(&self, doc: Doc<'_>, timeout: Duration) -> Result<(), BeamError> {
        self.call("lean_close", close_args(doc), timeout)
            .await
            .map(drop)
    }

    /// Beam's version and installation, as JSON.
    pub async fn version(&self, timeout: Duration) -> Result<Value, BeamError> {
        self.call("beam_version", json!({}), timeout).await
    }

    /// Beam's request and document counters, as JSON.
    pub async fn stats(&self, timeout: Duration) -> Result<Value, BeamError> {
        self.call("beam_stats", json!({}), timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// Each tool's declared and required argument names, from the saved
    /// `tools/list` of lean-beam-mcp 0.2.0.
    fn schemas() -> BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> {
        let list: Value =
            serde_json::from_str(include_str!("../testdata/tools-list.json")).unwrap();
        let names = |v: &Value| -> BTreeSet<String> {
            match v {
                Value::Object(m) => m.keys().cloned().collect(),
                Value::Array(a) => a
                    .iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect(),
                _ => BTreeSet::new(),
            }
        };
        list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let s = &t["inputSchema"];
                let name = t["name"].as_str().unwrap().to_string();
                (name, (names(&s["properties"]), names(&s["required"])))
            })
            .collect()
    }

    #[test]
    fn arguments_match_the_tool_schemas() {
        let root = Path::new("/w");
        let doc = Doc {
            root,
            path: "D/A.lean",
        };
        let pos = LspPos {
            line: 1,
            character: 2,
        };
        let handle = Handle(json!({"raw": {"value": "h"}}));
        let calls = [
            ("lean_update", update_args(doc)),
            ("lean_sync", sync_args(doc, true)),
            ("lean_todo", todo_args(doc, 1, &["sorry"])),
            ("lean_todo", todo_args(doc, 1, &[])),
            ("lean_goals", goals_args(doc, 1, pos, Mode::After)),
            ("lean_run_at", run_at_args(doc, 1, pos, "simp")),
            ("lean_run_at_handle", run_at_args(doc, 1, pos, "simp")),
            ("lean_run_with", run_with_args(doc, &handle, "simp")),
            ("lean_run_with_linear", run_with_args(doc, &handle, "simp")),
            ("lean_release", release_args(doc, &handle)),
            ("lean_save", save_args(doc)),
            ("lean_close", close_args(doc)),
        ];
        let schemas = schemas();
        for (tool, args) in calls {
            let (declared, required) = &schemas[tool];
            let given: BTreeSet<String> = args.as_object().unwrap().keys().cloned().collect();
            assert!(
                given.is_subset(declared),
                "{tool}: {given:?} not in {declared:?}"
            );
            assert!(
                required.is_subset(&given),
                "{tool}: {required:?} not all in {given:?}"
            );
        }
    }

    #[test]
    fn results_parse_as_beam_returns_them() {
        // Trimmed from lean-beam-mcp 0.2.0's answers on a small file.
        let run: RunResult = serde_json::from_value(json!({
            "document_progress": {"done": true}, "messages": [],
            "next_handle": {"epoch": 1, "raw": {"value": "runAt:1:2:0"}, "session": "s", "workspaceId": "local:/w"},
            "proof_state": {"goals": [{"goal_prefix": "⊢ ", "hyps": [{"names": ["a", "b"], "type": "Nat"}],
                                       "target": "2 * (a + b) = 2 * a + 2 * b"}]},
            "success": true, "traces": [], "workspace": {"root": "/w"}
        }))
        .unwrap();
        assert!(run.success);
        assert_eq!(run.next_handle.unwrap().0["raw"]["value"], "runAt:1:2:0");
        assert_eq!(run.proof_state.unwrap().goals[0].hyps[0].ty, "Nat");
        let todo: Todo = serde_json::from_value(json!({
            "items": [{"kind": "sorry", "message": "syntactic sorry",
                       "range": {"start": {"line": 3, "character": 2}, "end": {"line": 3, "character": 7}},
                       "run_at_position": {"line": 3, "character": 2}}],
            "version": 1
        }))
        .unwrap();
        assert_eq!(
            todo.items[0].run_at_position,
            Some(LspPos {
                line: 3,
                character: 2
            })
        );
    }
}
