//! A stand-in for the Lean REPL that speaks its protocol without Lean, for tests.
//!
//! Commands with `env` are scanned line by line for markers:
//! - `#check Nat` gives an info message `Nat : Type`
//! - `error_here` gives an error message on that line
//! - `sorry` gives a warning and a sorry with a fresh proof state
//! - `SLEEP <ms>` sleeps before answering; `CRASH` exits
//!
//! Without `env`, a command is an import header; `import Missing` fails.
//! The worker's axiom report (`run_cmd` … `PISA_DECLS`) lists each `theorem`,
//! `def` and `axiom` seen so far; theorems depend on `sorryAx` if the code used
//! `sorry`, on `Lean.ofReduceBool` if it used `native_decide`, and on any
//! declared axiom.
//! Tactic `rfl` closes the goal, `fail` errors, anything else leaves a goal open.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

struct State {
    envs: Vec<String>,
    goals: Vec<String>,
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
        std::fs::write(path, json!({"env": s.envs[env]}).to_string()).unwrap();
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
            Some(prev) => run(s, &prev, cmd),
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
    if let Some(line) = header.lines().position(|l| l.contains("import Missing")) {
        s.envs.push(String::new());
        return json!({"env": s.envs.len() - 1, "messages": [message(line + 1, "error",
            "unknown module prefix 'Missing'")]});
    }
    if header.contains("import Slow") {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    s.envs.push(header.to_string());
    json!({"env": s.envs.len() - 1})
}

fn run(s: &mut State, prev: &str, cmd: &str) -> Value {
    if cmd.starts_with("run_cmd") && cmd.contains("PISA_DECLS") {
        s.envs.push(prev.to_string());
        let report = format!("PISA_DECLS {}", declarations(prev));
        return json!({"env": s.envs.len() - 1, "messages": [message(1, "info", &report)]});
    }
    let mut messages = Vec::new();
    let mut sorries = Vec::new();
    for (i, line) in cmd.lines().enumerate() {
        let n = i + 1;
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
    s.envs.push(format!("{prev}\n{cmd}"));
    let mut out = json!({"env": s.envs.len() - 1});
    if !messages.is_empty() {
        out["messages"] = messages.into();
    }
    if !sorries.is_empty() {
        out["sorries"] = sorries.into();
    }
    out
}

/// One declaration per line starting with `theorem`, `def` or `axiom`. Its
/// "type hash" is the statement text between the name and `:=`.
fn declarations(code: &str) -> String {
    let mut used = vec!["propext".to_string()];
    if code.contains("sorry") {
        used.push("sorryAx".into());
    }
    if code.contains("native_decide") {
        used.push("Lean.ofReduceBool".into());
    }
    let mut decls = Vec::new();
    for line in code.lines() {
        let mut words = line.split_whitespace();
        let (Some(keyword), Some(name)) = (words.next(), words.next()) else {
            continue;
        };
        let statement = line
            .split_once(name)
            .map_or("", |(_, rest)| rest.split(":=").next().unwrap_or("").trim());
        let (kind, axioms) = match keyword {
            "axiom" => {
                used.push(name.to_string());
                ("axiom", vec![name.to_string()])
            }
            "def" => ("def", vec![]),
            "theorem" => ("theorem", Vec::new()),
            _ => continue,
        };
        decls.push((kind, name.to_string(), axioms, statement.to_string()));
    }
    let decls: Vec<Value> = decls
        .into_iter()
        .map(|(kind, name, axioms, statement)| {
            let axioms = if kind == "theorem" {
                used.clone()
            } else {
                axioms
            };
            json!({"name": name, "kind": kind, "axioms": axioms, "typeHash": statement})
        })
        .collect();
    Value::Array(decls).to_string()
}

fn message(line: usize, severity: &str, data: &str) -> Value {
    json!({"severity": severity, "pos": {"line": line, "column": 0}, "data": data})
}
