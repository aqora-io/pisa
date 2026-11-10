//! A stand-in for `lean-beam-mcp` in tests: newline-delimited JSON-RPC on
//! stdio, the real tool list (`testdata/tools-list.json`), argument checks as
//! strict as Beam's, and behaviour scripted by markers in a probe's `text`:
//!
//! - `SLEEP <ms>`: answer after a delay (requests run concurrently);
//! - `BUSY`: refuse without running, as Beam does when full;
//! - `STALE`: a document-version mismatch;
//! - `FAIL`: a tool error;
//! - `CRASH`: exit with a message on stderr;
//! - `rfl` or `trivial`: the tactic closes the goal; anything else fails.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

static HANDLES: AtomicU64 = AtomicU64::new(0);

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
    if let Some(ms) = text
        .split_whitespace()
        .skip_while(|w| *w != "SLEEP")
        .nth(1)
        .and_then(|ms| ms.parse().ok())
    {
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
    if text.contains("FAIL") {
        return tool_err("invalidParams", "fake failure");
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
            let closes = text.contains("rfl") || text.contains("trivial");
            let mut result = if closes {
                json!({"success": true, "messages": [], "proof_state": {"goals": []}, "traces": []})
            } else {
                json!({"success": false, "traces": [], "proof_state": {"goals": [goal("True")]},
                       "messages": [{"severity": "error", "text": "fake: tactic failed"}]})
            };
            if closes && name != "lean_run_at" {
                let n = HANDLES.fetch_add(1, Ordering::SeqCst);
                result["next_handle"] = json!({"epoch": 1, "raw": {"value": format!("runAt:fake:{n}")},
                                               "session": "fake", "workspaceId": "local:fake"});
            }
            tool_ok(result)
        }
        "lean_release" | "lean_close" => tool_ok(json!({})),
        "lean_save" => tool_ok(json!({"module": "Docs.A", "olean": "/fake/Docs/A.olean"})),
        "beam_version" => tool_ok(json!({"version": "fake"})),
        "beam_stats" => tool_ok(json!({"requests": 0})),
        _ => tool_err("invalidInput", &format!("{name} not scripted")),
    }
}
