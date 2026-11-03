//! A stand-in for the Lean REPL that speaks its protocol without Lean, for tests.
//!
//! Commands with `env` are scanned line by line for markers:
//! - `#check Nat` gives an info message `Nat : Type`
//! - `error_here` gives an error message on that line
//! - `sorry` gives a warning and a sorry with a fresh proof state
//! - `SLEEP <ms>` sleeps before answering; `CRASH` exits
//! - `needs NAME` errors unless `NAME` is declared (see `pisa_tests`)
//! - with `infotree: "original"`, every indented line is a tactic that
//!   closes `⊢ goal <line>`, inside one `by` node
//!
//! Without `env`, a command is an import header; `import Missing` fails, and
//! `import Clash` silently yields an empty environment, like the real REPL
//! when two imported modules declare the same name: every `#check` in it fails.
//! Snapshot modules (`import PisaSnap.s…`) are read from `LEAN_PATH`, with
//! the snapshots they import.
//! The worker's axiom report (`run_cmd` … `PISA_DECLS`) lists each `theorem`,
//! `def` and `axiom` seen since the imports, or, for a module report, those
//! of that module (see `pisa_tests::declarations`). Its declaration export
//! (`run_cmd` … `PISA_EXPORT`) is the same declarations, with their
//! statements and docstrings (see `pisa_tests::exports`).
//! Tactic `rfl` closes the goal, `fail` errors, anything else leaves a goal open.

use std::io::{BufRead, Write};

use pisa_tests::{declarations, declares, exports, find_module, imported_code, needs};
use serde_json::{Value, json};

struct State {
    envs: Vec<Env>,
    goals: Vec<String>,
}

#[derive(Clone)]
struct Env {
    /// The code of the snapshot modules the header imports.
    imported: String,
    /// The header, then every command run since.
    local: String,
}

fn main() {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut state = State {
        envs: Vec::new(),
        goals: Vec::new(),
    };
    loop {
        let mut query = String::new();
        loop {
            match lines.next() {
                None if query.is_empty() => return,
                None => break,
                Some(line) => {
                    let line = line.unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    query.push_str(&line);
                }
            }
        }
        if query.is_empty() {
            return;
        }
        let input: Value = serde_json::from_str(&query).expect("valid JSON command");
        let out = respond(&mut state, &input);
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{}\n", serde_json::to_string_pretty(&out).unwrap()).unwrap();
        stdout.flush().unwrap();
    }
}

fn respond(s: &mut State, input: &Value) -> Value {
    if let Some(tactic) = input.get("tactic").and_then(Value::as_str) {
        let Some(goal) = state_goal(s, input) else {
            return json!({"message": "Unknown proof state."});
        };
        return apply_tactic(s, &goal, tactic);
    }
    if let Some(path) = input.get("pickleTo").and_then(Value::as_str) {
        if input.get("proofState").is_some() {
            let Some(goal) = state_goal(s, input) else {
                return json!({"message": "Unknown proof State."});
            };
            std::fs::write(path, json!({"goal": goal}).to_string()).unwrap();
            return step(s, vec![goal], "Incomplete: open goals remain");
        }
        let env = input["env"].as_u64().unwrap() as usize;
        std::fs::write(path, json!({"env": s.envs[env].local}).to_string()).unwrap();
        return json!({"env": env});
    }
    if let Some(path) = input.get("unpickleProofStateFrom").and_then(Value::as_str) {
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let goal = saved["goal"].as_str().unwrap().to_string();
        return step(s, vec![goal], "Incomplete: open goals remain");
    }
    let cmd = input["cmd"].as_str().expect("cmd");
    match input.get("env").and_then(Value::as_u64) {
        None => import(s, cmd),
        Some(env) => match s.envs.get(env as usize).cloned() {
            None => json!({"message": "Unknown environment."}),
            Some(prev) => run(s, &prev, cmd, input["infotree"] == "original"),
        },
    }
}

fn state_goal(s: &State, input: &Value) -> Option<String> {
    let id = input["proofState"].as_u64()? as usize;
    s.goals.get(id).cloned()
}

fn step(s: &mut State, goals: Vec<String>, status: &str) -> Value {
    s.goals.push(goals.first().cloned().unwrap_or_default());
    json!({"proofState": s.goals.len() - 1, "goals": goals, "proofStatus": status})
}

fn apply_tactic(s: &mut State, goal: &str, tactic: &str) -> Value {
    match tactic {
        "fail" => json!({"message": "Lean error:\nfailed"}),
        "rfl" => step(s, vec![], "Completed"),
        t => step(
            s,
            vec![format!("{goal} after {t}")],
            "Incomplete: open goals remain",
        ),
    }
}

fn import(s: &mut State, header: &str) -> Value {
    let empty = Env {
        imported: String::new(),
        local: String::new(),
    };
    if let Some(line) = header.lines().position(|l| l.contains("import Missing")) {
        s.envs.push(empty);
        return json!({"env": s.envs.len() - 1, "messages": [message(line + 1, "error",
            "unknown module prefix 'Missing'")]});
    }
    let imported = match imported_code(header) {
        Ok(code) => code,
        Err(module) => {
            s.envs.push(empty);
            let error = format!("unknown module prefix '{module}'");
            return json!({"env": s.envs.len() - 1, "messages": [message(1, "error", &error)]});
        }
    };
    if header.contains("import Slow") {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    s.envs.push(Env {
        imported,
        local: header.to_string(),
    });
    json!({"env": s.envs.len() - 1})
}

fn run(s: &mut State, prev: &Env, cmd: &str, infotree: bool) -> Value {
    for (marker, report) in [
        ("PISA_DECLS", declarations as fn(&str) -> String),
        ("PISA_EXPORT", exports),
    ] {
        if !(cmd.starts_with("run_cmd") && cmd.contains(marker)) {
            continue;
        }
        let code = match cmd.split_once("getModuleIdx? `") {
            None => prev.local.clone(),
            Some((_, rest)) => {
                let module = rest.split_whitespace().next().unwrap();
                std::fs::read_to_string(find_module(module).unwrap()).unwrap()
            }
        };
        s.envs.push(prev.clone());
        let report = format!("{marker} {}", report(&code));
        return json!({"env": s.envs.len() - 1, "messages": [message(1, "info", &report)]});
    }
    if prev.local.contains("import Clash") && cmd.starts_with("#check") {
        s.envs.push(prev.clone());
        return json!({"env": s.envs.len() - 1,
                      "messages": [message(1, "error", "Unknown identifier")]});
    }
    let mut messages = Vec::new();
    let mut sorries = Vec::new();
    let mut tactics = Vec::new();
    for (i, line) in cmd.lines().enumerate() {
        let n = i + 1;
        let tactic = line.trim_start();
        if infotree && !tactic.is_empty() && tactic.len() < line.len() {
            let range = ((n, line.len() - tactic.len()), (n, line.len()));
            tactics.push(info_node("Lean.Parser.Tactic.fake", range, n, vec![]));
        }
        if let Some(ms) = line.split("SLEEP ").nth(1) {
            let ms: u64 = ms.trim().parse().unwrap_or(1000);
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
        if line.contains("CRASH") {
            eprintln!("fake-repl: crashing as asked");
            std::process::exit(1);
        }
        if line.contains("#check Nat") {
            let mut m = message(n, "info", "Nat : Type");
            m["endPos"] = json!({"line": n, "column": 6});
            messages.push(m);
        }
        if line.contains("error_here") {
            messages.push(message(n, "error", "unexpected error_here"));
        }
        if let Some(col) = line.find("sorry") {
            messages.push(message(n, "warning", "declaration uses 'sorry'"));
            s.goals.push(format!("⊢ goal {n}"));
            sorries.push(json!({
                "pos": {"line": n, "column": col},
                "endPos": {"line": n, "column": col + 5},
                "goal": format!("⊢ goal {n}"),
                "proofState": s.goals.len() - 1,
            }));
        }
    }
    let local = format!("{}\n{cmd}", prev.local);
    for (n, name) in needs(cmd) {
        if !declares(&prev.imported, &local, name) {
            messages.push(message(n, "error", &format!("Unknown identifier `{name}`")));
        }
    }
    s.envs.push(Env {
        imported: prev.imported.clone(),
        local,
    });
    let mut out = json!({"env": s.envs.len() - 1});
    if !messages.is_empty() {
        out["messages"] = messages.into();
    }
    if !sorries.is_empty() {
        out["sorries"] = sorries.into();
    }
    if let (Some(first), Some(last)) = (tactics.first(), tactics.last()) {
        let line = |t: &Value, end: &str| t["node"]["stx"]["range"][end]["line"].as_u64().unwrap();
        let (start, finish) = (line(first, "start") as usize, line(last, "finish") as usize);
        let by = info_node(
            "Lean.Parser.Term.byTactic",
            ((start, 0), (finish, 0)),
            start,
            tactics,
        );
        out["infotree"] = json!([by]);
    }
    out
}

/// A `TacticInfo` node shaped like the REPL's, closing `⊢ goal <goal>`.
fn info_node(
    name: &str,
    ((line, column), (end_line, end_column)): ((usize, usize), (usize, usize)),
    goal: usize,
    children: Vec<Value>,
) -> Value {
    json!({"kind": "TacticInfo", "node": {
        "name": name,
        "stx": {"range": {"synthetic": false, "start": {"line": line, "column": column},
                          "finish": {"line": end_line, "column": end_column}}},
        "goalsBefore": [format!("⊢ goal {goal}")], "goalsAfter": []},
        "children": children})
}

fn message(line: usize, severity: &str, data: &str) -> Value {
    json!({"severity": severity, "pos": {"line": line, "column": 0}, "data": data})
}
