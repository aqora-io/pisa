//! What fake-repl and fake-lean share: finding snapshot modules on
//! `LEAN_PATH` and reading declarations out of code.
//!
//! A fake module file is the module's code. A line declares a name when it
//! starts with `theorem`, `def` or `axiom` (or `private` or `public` and one
//! of those), and a line `needs NAME` is an error unless `NAME` is declared
//! in the same code or, visibly, in a module it imports: without `private`,
//! or, for module-system code (starting with `module`), with `public`. Like
//! Lean, a module-system module whose `.olean.private` is missing imports as
//! empty.

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::{Value, json};

/// The file of `module`, looked up as Lean does: in the first `LEAN_PATH` root
/// that has the module's top-level directory.
pub fn find_module(module: &str) -> Option<PathBuf> {
    let lean_path = std::env::var_os("LEAN_PATH")?;
    let top = module.split('.').next()?;
    let root = std::env::split_paths(&lean_path).find(|root| root.join(top).is_dir())?;
    let file = module
        .split('.')
        .fold(root, |p, part| p.join(part))
        .with_extension("olean");
    file.exists().then_some(file)
}

/// Whether `code` is module-system code.
pub fn is_module(code: &str) -> bool {
    code.trim_start().starts_with("module")
}

/// The modules `code`'s import lines name, with or without `public`, `meta`
/// and `all`.
pub fn imports(code: &str) -> Vec<&str> {
    code.lines()
        .filter_map(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            let at = words.iter().position(|w| *w == "import")?;
            let modules: Vec<&str> = words[at + 1..]
                .iter()
                .copied()
                .filter(|w| *w != "all")
                .collect();
            words[..at]
                .iter()
                .all(|w| matches!(*w, "public" | "meta"))
                .then_some(modules)
        })
        .flatten()
        .collect()
}

/// The declarations of a module that its importers see: module-system code
/// shows only its `public` declarations.
fn visible(code: &str) -> String {
    if !is_module(code) {
        return code.to_string();
    }
    code.lines()
        .filter_map(|l| l.strip_prefix("public "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The code of every snapshot module `code` imports, directly or not, or the
/// first one that is missing.
pub fn imported_code(code: &str) -> Result<String, String> {
    fn visit(code: &str, seen: &mut HashSet<String>, out: &mut String) -> Result<(), String> {
        for module in imports(code)
            .into_iter()
            .filter(|m| m.starts_with("PisaSnap."))
        {
            if !seen.insert(module.to_string()) {
                continue;
            }
            let file = find_module(module).ok_or_else(|| module.to_string())?;
            let text = std::fs::read_to_string(&file).map_err(|_| module.to_string())?;
            if is_module(&text) && !file.with_extension("olean.private").exists() {
                continue;
            }
            visit(&text, seen, out)?;
            out.push_str(&visible(&text));
            out.push('\n');
        }
        Ok(())
    }
    let mut out = String::new();
    visit(code, &mut HashSet::new(), &mut out)?;
    Ok(out)
}

/// The name a line declares, and whether it is private.
fn declared(line: &str) -> Option<(&str, bool)> {
    let mut words = line.split_whitespace();
    let mut keyword = words.next()?;
    let private = keyword == "private";
    if private || keyword == "public" {
        keyword = words.next()?;
    }
    matches!(keyword, "theorem" | "def" | "axiom").then(|| words.next().map(|n| (n, private)))?
}

/// Whether `name` is declared in `local`, or non-privately in `imported`.
pub fn declares(imported: &str, local: &str, name: &str) -> bool {
    imported
        .lines()
        .filter_map(declared)
        .any(|(n, private)| n == name && !private)
        || local.lines().filter_map(declared).any(|(n, _)| n == name)
}

/// The names `needs` lines of `code` ask for, with their line numbers.
pub fn needs(code: &str) -> impl Iterator<Item = (usize, &str)> {
    code.lines().enumerate().filter_map(|(i, l)| {
        let name = l.trim().strip_prefix("needs ")?.split_whitespace().next()?;
        Some((i + 1, name))
    })
}

/// The worker's axiom report for `code`: one declaration per line starting
/// with `theorem`, `def` or `axiom`. Theorems depend on `sorryAx` if the code
/// uses `sorry`, on `Lean.ofReduceBool` if it uses `native_decide`, on any
/// axiom it declares, and on the axioms of imported snapshots its line names;
/// those it declares or imports are its `assumes`. A "type hash" is the statement text between the name
/// and `:=`, and a `def`'s "value hash" the text after it. Its refs are the
/// words of each that the code or a snapshot it imports declares.
/// The worker's declaration export for `code`: each `theorem`, `def` and
/// `axiom`, with its statement as the signature, the words of its statement
/// and of its value as the constants they use, and a `/-- … -/` on the line
/// before as its docstring.
pub fn exports(code: &str) -> String {
    let mut decls = Vec::new();
    let mut doc = None;
    for line in code.lines() {
        let line = line.strip_prefix("public ").unwrap_or(line);
        if let Some(text) = line.strip_prefix("/--") {
            doc = Some(text.trim_end_matches("-/").trim().to_string());
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(kind @ ("theorem" | "def" | "axiom")), Some(name)) = (words.next(), words.next())
        else {
            doc = None;
            continue;
        };
        let (statement, value) = statement_and_value(line, name);
        decls.push(json!({
            "name": name,
            "kind": kind,
            "signature": format!("{name} {statement}"),
            "type_pp_full": statement,
            "type_refs": names_in(&statement),
            "value_refs": names_in(&value),
            "docstring": doc.take(),
        }));
    }
    Value::Array(decls).to_string()
}

/// The text of a declaration line between `name` and `:=`, and after `:=`.
fn statement_and_value(line: &str, name: &str) -> (String, String) {
    let rest = line.split_once(name).map_or("", |(_, rest)| rest);
    let (statement, value) = rest.split_once(":=").unwrap_or((rest, ""));
    (statement.trim().to_string(), value.trim().to_string())
}

/// The names `text` mentions, as the constants it would use.
fn names_in(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '\''))
        .filter(|w| !w.is_empty())
        .collect()
}

pub fn declarations(code: &str) -> String {
    let imported = imported_code(code).unwrap_or_default();
    let axioms_of = |code: &str| -> Vec<String> {
        code.lines()
            .filter_map(|l| {
                let mut words = l.strip_prefix("public ").unwrap_or(l).split_whitespace();
                (words.next()? == "axiom").then(|| words.next().map(String::from))?
            })
            .collect()
    };
    let (own_axioms, imported_axioms) = (axioms_of(code), axioms_of(&imported));
    let mut used = vec!["propext".to_string()];
    if code.contains("sorry") {
        used.push("sorryAx".into());
    }
    if code.contains("native_decide") {
        used.push("Lean.ofReduceBool".into());
    }
    let mut decls = Vec::new();
    for line in code.lines() {
        let line = line.strip_prefix("public ").unwrap_or(line);
        let mut words = line.split_whitespace();
        let (Some(keyword), Some(name)) = (words.next(), words.next()) else {
            continue;
        };
        let (statement, value) = statement_and_value(line, name);
        let (kind, axioms) = match keyword {
            "axiom" => {
                used.push(name.to_string());
                ("axiom", vec![name.to_string()])
            }
            "def" => ("def", vec![]),
            "theorem" => ("theorem", Vec::new()),
            _ => continue,
        };
        decls.push((kind, name.to_string(), axioms, statement, value));
    }
    let decls: Vec<Value> = decls
        .into_iter()
        .map(|(kind, name, axioms, statement, value)| {
            let axioms = if kind == "theorem" {
                // And the imported axioms it names.
                let mut axioms = used.clone();
                let named = names_in(&statement).into_iter().chain(names_in(&value));
                for n in named.filter(|n| imported_axioms.iter().any(|a| a == n)) {
                    if !axioms.iter().any(|a| a == n) {
                        axioms.push(n.to_string());
                    }
                }
                axioms
            } else {
                axioms
            };
            let assumes: Vec<&String> = axioms
                .iter()
                .filter(|a| own_axioms.contains(a) || imported_axioms.contains(a))
                .collect();
            let value_hash = (kind == "def").then_some(&value);
            let refs = |text: &str| -> Vec<String> {
                let mut refs: Vec<String> = names_in(text)
                    .into_iter()
                    .filter(|n| *n != name && declares(&imported, code, n))
                    .map(String::from)
                    .collect();
                refs.dedup();
                refs
            };
            let value_refs = if kind == "def" { refs(&value) } else { Vec::new() };
            json!({"name": name, "kind": kind, "axioms": axioms, "assumes": assumes, "type_hash": statement,
                   "value_hash": value_hash, "type_refs": refs(&statement), "value_refs": value_refs})
        })
        .collect();
    Value::Array(decls).to_string()
}
