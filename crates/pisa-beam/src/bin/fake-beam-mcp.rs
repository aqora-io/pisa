//! A stand-in for `lean-beam-mcp` in tests: newline-delimited JSON-RPC on
//! stdio, the real tool list (`testdata/tools-list.json`), argument checks as
//! strict as Beam's, and behaviour scripted by markers in a probe's `text`:
//!
//! - `SLEEP <ms>`: answer after a delay (requests run concurrently);
//! - `SLOW_SYNC <ms>` in a document: its syncs answer after a delay;
//! - `BUSY`: refuse without running, as Beam does when full;
//! - `STALE`: a document-version mismatch;
//! - `FAIL`: a tool error;
//! - `RECURSION`: a run that failed on Lean's `maxRecDepth` error;
//! - `HEARTBEAT`: a run that failed on Lean's `maxHeartbeats` error (200000,
//!   at `whnf`);
//! - `FLAKY`: a tool error the first time the text comes, not after;
//! - `CRASH`: exit with a message on stderr;
//! - `rfl` or `trivial`: the tactic closes the goal; `fail` or `omega`: it
//!   fails; anything else leaves the goal `<goal> after <text>`.
//!
//! A probe starts from the goal `True`, and one from a handle from that
//! handle's goal, so a chain of tactics replayed in another process reaches
//! the same goals.
//!
//! When the document exists on disk it is read, as Beam reads it: `update`
//! and `sync` give it a new version when its text changed, each `sorry` is a
//! warning and a `todo` item, and the goal at a position is the rest of that
//! line, so tests can tell where a position landed. Where the rest is blank,
//! there is no proof, which Beam answers with an `invalidParams` error; where
//! it is `-- done`, a finished proof, with no goals; where it is
//! `FAIL_GOALS`, Beam's own failure.
//! Probe messages have no range, as Beam's don't. Otherwise the answers are
//! fixed.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};

static HANDLES: AtomicU64 = AtomicU64::new(0);
/// The goal each handle holds, by its raw value; none once it is closed.
static GOALS: LazyLock<Mutex<HashMap<String, Option<String>>>> = LazyLock::new(Mutex::default);
/// The `FLAKY` texts that have failed once.
static FLAKED: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(Mutex::default);
/// Each document's text and version, by path.
static DOCS: LazyLock<Mutex<HashMap<PathBuf, (String, u64)>>> = LazyLock::new(Mutex::default);

fn main() {
    let tools: Value = serde_json::from_str(include_str!("../../testdata/tools-list.json"))
        .expect("tools-list.json");
    let out = Arc::new(Mutex::new(std::io::stdout()));
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = msg.get("id").cloned();
        match msg["method"].as_str().unwrap_or_default() {
            "initialize" => send(
                &out,
                id,
                Ok(
                    json!({"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                          "serverInfo": {"name": "fake-beam-mcp", "version": "0"}}),
                ),
            ),
            "tools/list" => send(&out, id, Ok(tools.clone())),
            "tools/call" => {
                let (out, tools) = (out.clone(), tools.clone());
                let params = msg["params"].clone();
                std::thread::spawn(move || {
                    let reply = call(&tools, &params);
                    send(&out, id, reply);
                });
            }
            m if m.starts_with("notifications/") => {}
            m => send(
                &out,
                id,
                Err(json!({"code": -32601, "message": format!("no method {m}")})),
            ),
        }
    }
}

fn send(out: &Mutex<std::io::Stdout>, id: Option<Value>, reply: Result<Value, Value>) {
    let Some(id) = id else { return };
    let msg = match reply {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    };
    let mut out = out.lock().unwrap();
    writeln!(out, "{msg}").unwrap();
    out.flush().unwrap();
}

fn tool_ok(content: Value) -> Result<Value, Value> {
    Ok(
        json!({"content": [{"type": "text", "text": content.to_string()}],
              "structuredContent": content, "isError": false}),
    )
}

fn tool_err(code: &str, message: &str) -> Result<Value, Value> {
    let content = json!({"code": code, "message": message});
    Ok(
        json!({"content": [{"type": "text", "text": content.to_string()}],
              "structuredContent": content, "isError": true}),
    )
}

/// A run that failed with `error`, leaving the goal `True`.
fn failed_run(error: &str) -> Value {
    json!({"success": false, "messages": [{"severity": "error", "text": error}],
           "proof_state": {"goals": [goal("True")]}, "traces": []})
}

fn goal(target: &str) -> Value {
    json!({"goal_prefix": "⊢ ", "hyps": [], "target": target})
}

fn call(tools: &Value, params: &Value) -> Result<Value, Value> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = params["arguments"].as_object().cloned().unwrap_or_default();
    let Some(tool) = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
    else {
        return tool_err("invalidInput", &format!("unknown tool {name}"));
    };
    let schema = &tool["inputSchema"];
    for key in args.keys() {
        if schema["properties"].get(key).is_none() {
            return tool_err(
                "invalidInput",
                &format!("{name} accepts no undeclared input fields: {key}"),
            );
        }
    }
    for key in schema["required"].as_array().into_iter().flatten() {
        if !args.contains_key(key.as_str().unwrap()) {
            return tool_err("invalidInput", &format!("{name} requires {key}"));
        }
    }
    let text = args.get("text").and_then(Value::as_str).unwrap_or_default();
    if let Some(ms) = delay(text, "SLEEP") {
        std::thread::sleep(Duration::from_millis(ms));
    }
    if text.contains("CRASH") {
        eprintln!("fake-beam-mcp: crashing as asked");
        std::process::exit(3);
    }
    if text.contains("BUSY") {
        return Err(
            json!({"code": -32000, "message": "MCP server is busy; this request did not execute"}),
        );
    }
    if text.contains("STALE") {
        return tool_err("contentModified", "document version 1 is stale; accepted 2");
    }
    // Beam answers Lean's limits as a failed run, the tactic's error nesting
    // Lean's (Lean 4.36.0's Beam; before, a tool error).
    if text.contains("RECURSION") {
        return tool_ok(failed_run(
            "Tactic `simp` failed with a nested error:\nmaximum recursion depth has been \
             reached\nuse `set_option maxRecDepth <num>` to increase limit\nuse `set_option \
             diagnostics true` to get diagnostic information",
        ));
    }
    if text.contains("HEARTBEAT") {
        return tool_ok(failed_run(
            "Tactic `simp` failed with a nested error:\n(deterministic) timeout at `whnf`, \
             maximum number of heartbeats (200000) has been reached\n\nNote: Use `set_option \
             maxHeartbeats <num>` to set the limit.",
        ));
    }
    if text.contains("FAIL") {
        return tool_err("invalidParams", "fake failure");
    }
    if text.contains("FLAKY") && FLAKED.lock().unwrap().insert(text.to_string()) {
        return tool_err("internalError", "fake failure, once");
    }
    if let Some(path) = document(&args) {
        if name == "lean_sync"
            && let Some(ms) = delay(
                &std::fs::read_to_string(&path).unwrap_or_default(),
                "SLOW_SYNC",
            )
        {
            std::thread::sleep(Duration::from_millis(ms));
        }
        return document_call(name, &path, &args, text);
    }
    match name {
        "lean_update" => tool_ok(json!({"version": 1, "changed": true})),
        "lean_sync" => tool_ok(json!({
            "version": 1,
            "diagnostics": {"counts": {"error": 0, "total": 1}, "items": [{
                "message": "declaration uses `sorry`", "severity": "warning",
                "range": {"start": {"line": 0, "character": 8}, "end": {"line": 0, "character": 9}}}]},
            "readiness": {"save_ready": true, "reason": "ok"}})),
        "lean_todo" => tool_ok(json!({"version": 1, "items": [{
            "kind": "sorry", "message": "syntactic sorry",
            "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 7}},
            "run_at_position": {"line": 1, "character": 2},
            "proof_state": {"goals": [goal("True")]}}]})),
        "lean_goals" => tool_ok(json!({"goals": [goal("True")]})),
        "lean_run_at" | "lean_run_at_handle" | "lean_run_with" | "lean_run_with_linear" => {
            tool_ok(probe(name, text, &args))
        }
        "lean_release" | "lean_close" => tool_ok(json!({})),
        "lean_save" => tool_ok(json!({"module": "Docs.A", "olean": "/fake/Docs/A.olean"})),
        "beam_version" => tool_ok(json!({"version": "fake"})),
        "beam_stats" => tool_ok(json!({"requests": 0})),
        _ => tool_err("invalidInput", &format!("{name} not scripted")),
    }
}

/// A probe's result, from the goal `True` or the handle's in `args`: `rfl`
/// or `trivial` closes it, `fail` or `omega` fails, and anything else leaves
/// `<goal> after <text>`. A run that keeps its result, and succeeds, has a
/// new handle on it; `lean_run_with_linear` uses its handle up.
fn probe(name: &str, text: &str, args: &Map<String, Value>) -> Value {
    let raw = |handle: &Value| {
        handle["raw"]["value"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let mut goals = GOALS.lock().unwrap();
    let from = match args.get("handle") {
        Some(handle) if name == "lean_run_with_linear" => goals.remove(&raw(handle)).flatten(),
        Some(handle) => goals.get(&raw(handle)).cloned().flatten(),
        None => None,
    }
    .unwrap_or_else(|| "True".to_string());
    let (success, left) = if text.contains("rfl") || text.contains("trivial") {
        (true, None)
    } else if text.contains("fail") || text.contains("omega") {
        (false, Some(from.clone()))
    } else {
        (true, Some(format!("{from} after {}", text.trim())))
    };
    let state: Vec<Value> = left.iter().map(|g| goal(g)).collect();
    let messages = match success {
        true => json!([]),
        false => json!([{"severity": "error", "text": "fake: tactic failed"}]),
    };
    let mut result = json!({"success": success, "messages": messages,
                            "proof_state": {"goals": state}, "traces": []});
    if success && name != "lean_run_at" {
        let n = HANDLES.fetch_add(1, Ordering::SeqCst);
        let value = format!("runAt:fake:{n}");
        goals.insert(value.clone(), left.filter(|_| success));
        result["next_handle"] = json!({"epoch": 1, "raw": {"value": value},
                                       "session": "fake", "workspaceId": "local:fake"});
    }
    result
}

/// The document a call names, if it exists on disk.
/// The milliseconds after `marker` in `text`.
fn delay(text: &str, marker: &str) -> Option<u64> {
    text.split_whitespace()
        .skip_while(|w| *w != marker)
        .nth(1)
        .and_then(|ms| ms.parse().ok())
}

fn document(args: &Map<String, Value>) -> Option<PathBuf> {
    let root = args.get("workspace")?.get("root")?.as_str()?;
    let path = Path::new(root).join(args.get("path")?.as_str()?);
    path.is_file().then_some(path)
}

fn document_call(
    name: &str,
    path: &Path,
    args: &Map<String, Value>,
    text: &str,
) -> Result<Value, Value> {
    let mut docs = DOCS.lock().unwrap();
    match name {
        "lean_update" | "lean_sync" | "lean_refresh" => {
            let now = std::fs::read_to_string(path).unwrap_or_default();
            let entry = docs.entry(path.to_path_buf()).or_insert((String::new(), 0));
            let changed = entry.1 == 0 || entry.0 != now;
            if changed {
                *entry = (now, entry.1 + 1);
            }
            let (text, version) = entry.clone();
            if name == "lean_update" {
                return tool_ok(json!({"version": version, "changed": changed}));
            }
            let items: Vec<Value> = sorries(&text)
                .into_iter()
                .map(|range| {
                    json!({"message": "declaration uses `sorry`", "severity": "warning", "range": range})
                })
                .collect();
            return tool_ok(json!({
                "version": version,
                "diagnostics": {"counts": {"error": 0, "warning": items.len(), "total": items.len()},
                                "items": items},
                "readiness": {"save_ready": true, "reason": "ok"}}));
        }
        "lean_close" => {
            docs.remove(path);
            return tool_ok(json!({"closed": true}));
        }
        _ => {}
    }
    let Some((doc, version)) = docs.get(path).cloned() else {
        return tool_err("invalidParams", "the document was never synced");
    };
    if let Some(asked) = args.get("version").and_then(Value::as_u64)
        && asked != version
    {
        return tool_err(
            "contentModified",
            &format!("document version {asked} is stale; accepted {version}"),
        );
    }
    match name {
        "lean_todo" => {
            let items: Vec<Value> = sorries(&doc)
                .into_iter()
                .map(|range| {
                    json!({"kind": "sorry", "message": "syntactic sorry", "range": range,
                           "run_at_position": range["start"],
                           "proof_state": {"goals": [goal("True")]}})
                })
                .collect();
            tool_ok(json!({"version": version, "items": items}))
        }
        "lean_goals" => {
            let line = args["line"].as_u64().unwrap_or_default() as usize;
            let character = args["character"].as_u64().unwrap_or_default() as usize;
            let rest = rest_of_line(&doc, line, character);
            if rest.starts_with("-- done") {
                return tool_ok(json!({"goals": []}));
            }
            if rest.starts_with("FAIL_GOALS") {
                return tool_err("internalError", "fake failure on goals");
            }
            if rest.is_empty() {
                return tool_err(
                    "invalidParams",
                    &format!(
                        "position ({line}, {character}) is inside the document, but Lean has no \
                         proof goals there; try a position inside a tactic or proof body"
                    ),
                );
            }
            tool_ok(json!({"goals": [goal(rest)]}))
        }
        "lean_run_at" | "lean_run_at_handle" | "lean_run_with" | "lean_run_with_linear" => {
            tool_ok(probe(name, text, args))
        }
        "lean_save" => tool_ok(json!({"module": "Docs.A", "olean": "/fake/Docs/A.olean"})),
        "lean_release" => tool_ok(json!({})),
        _ => tool_err("invalidInput", &format!("{name} not scripted on documents")),
    }
}

/// The LSP range of each `sorry` word in `text`.
fn sorries(text: &str) -> Vec<Value> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    for (line, s) in text.split('\n').enumerate() {
        for (byte, _) in s.match_indices("sorry") {
            let before = s[..byte].chars().next_back();
            let after = s[byte + 5..].chars().next();
            if before.is_some_and(word) || after.is_some_and(word) {
                continue;
            }
            let start = s[..byte].encode_utf16().count();
            out.push(json!({"start": {"line": line, "character": start},
                            "end": {"line": line, "character": start + 5}}));
        }
    }
    out
}

/// What follows UTF-16 position `character` on `line`, trimmed.
fn rest_of_line(text: &str, line: usize, character: usize) -> &str {
    let s = text.split('\n').nth(line).unwrap_or_default();
    let mut units = 0;
    let byte = s
        .char_indices()
        .find(|(_, c)| {
            let here = units >= character;
            units += c.len_utf16();
            here
        })
        .map_or(s.len(), |(i, _)| i);
    s[byte..].trim()
}
