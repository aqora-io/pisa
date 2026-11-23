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
/// and `all`. As in Lean, a `/- … -/` comment before an import on its line
/// doesn't hide it.
pub fn imports(code: &str) -> Vec<&str> {
    code.lines()
        .filter_map(|l| {
            let l = match (l.trim_start().strip_prefix("/-"), l.find("-/")) {
                (Some(_), Some(end)) => &l[end + 2..],
                _ => l,
            };
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

/// The theorems of the snapshot modules `code` imports, directly or not,
/// that use `sorry`, each with its module: those of a module whose text says
/// `sorry`, and those whose line names one of these.
pub fn sorried_imports(code: &str) -> std::collections::HashMap<String, String> {
    fn visit(
        code: &str,
        seen: &mut HashSet<String>,
        out: &mut std::collections::HashMap<String, String>,
    ) {
        for module in imports(code)
            .into_iter()
            .filter(|m| m.starts_with("PisaSnap."))
        {
            if !seen.insert(module.to_string()) {
                continue;
            }
            let Some(file) = find_module(module) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            if is_module(&text) && !file.with_extension("olean.private").exists() {
                continue;
            }
            visit(&text, seen, out);
            let visible = visible(&text);
            for line in visible.lines() {
                let Some((name, false)) = declared(line) else {
                    continue;
                };
                let theorem = line.split_whitespace().any(|w| w == "theorem");
                let names_one = names_in(line).iter().any(|n| out.contains_key(*n));
                if theorem && (text.contains("sorry") || names_one) {
                    out.insert(name.to_string(), module.to_string());
                }
            }
        }
    }
    let mut out = std::collections::HashMap::new();
    visit(code, &mut HashSet::new(), &mut out);
    out
}

/// Prints a Lean message as `lean --json` does, on `line` of the input.
pub fn print_message(line: usize, severity: &str, data: &str) {
    let message = json!({
        "severity": severity,
        "pos": {"line": line, "column": 0},
        "endPos": null,
        "fileName": "<stdin>",
        "data": data,
    });
    println!("{message}");
}

/// The name a line declares, and whether it is private.
fn declared(line: &str) -> Option<(&str, bool)> {
    let mut words = line.split_whitespace();
    let mut keyword = words.next()?;
    let private = keyword == "private";
    if private || keyword == "public" {
        keyword = words.next()?;
    }
    matches!(keyword, "theorem" | "def" | "axiom" | "structure")
        .then(|| words.next().map(|n| (n, private)))?
}

/// Whether `name` is declared in `local`, or non-privately in `imported`.
pub fn declares(imported: &str, local: &str, name: &str) -> bool {
    imported
        .lines()
        .filter_map(declared)
        .any(|(n, private)| n == name && !private)
        || local.lines().filter_map(declared).any(|(n, _)| n == name)
}

/// Lean's error for an unmet `needs NAME` on `line`, with Lean's note on
/// `autoImplicit` after `needs NAME binder`, as where a binder would bind
/// the name were the option on.
pub fn unknown_identifier(line: &str, name: &str) -> String {
    let mut text = format!("Unknown identifier `{name}`");
    if line.split_whitespace().nth(2) == Some("binder") {
        text.push_str(&format!(
            "\n\nNote: It is not possible to treat `{name}` as an implicitly bound variable \
             here because the `autoImplicit` option is set to `false`."
        ));
    }
    text
}

/// The names `needs` lines of `code` ask for, with their line numbers.
pub fn needs(code: &str) -> impl Iterator<Item = (usize, &str)> {
    code.lines().enumerate().filter_map(|(i, l)| {
        let name = l.trim().strip_prefix("needs ")?.split_whitespace().next()?;
        Some((i + 1, name))
    })
}

/// The worker's axiom report for `code`: one declaration per line starting
/// with `theorem`, `def` or `axiom`, and for `structure S where FIELDS`, the
/// inductive `S` and its constructor `S.mk`, whose parent it is. Theorems depend on `sorryAx` if the code
/// uses `sorry`, on `Lean.ofReduceBool` if it uses `native_decide`, on any
/// axiom it declares, and on the axioms of imported snapshots its line names;
/// those it declares or imports are its `assumes`. Its "type" is the statement text between the name
/// and `:=`, and a `def`'s "value" the text after it, which the worker hashes. Its refs are the
/// words of each that the code or a snapshot it imports declares. A theorem
/// of `¬ X`, or `¬ (X)`, "negates" the statement `X`.
/// The worker's declaration export for `code`: each `theorem`, `def` and
/// `axiom`, with its statement as the signature and as the type with every
/// argument shown, the words of its statement and of its value as the
/// constants they use, and a `/-- … -/` on the line before as its docstring.
/// A `-- printed-as: LINE` line before a declaration makes it print as `LINE`
/// but for its type with every argument shown, as a type that prints like
/// another can differ from it in an argument the printer hides. That type is
/// cut as Lean's export cuts it, at `VALUE_CHARS` characters and ` …`.
pub fn exports(code: &str) -> String {
    // The catalog's `VALUE_CHARS`, which this crate doesn't depend on.
    const VALUE_CHARS: usize = 4000;
    let mut decls = Vec::new();
    let mut doc = None;
    let mut printed_as: Option<&str> = None;
    for line in code.lines() {
        let line = line.strip_prefix("public ").unwrap_or(line);
        if let Some(text) = line.strip_prefix("/--") {
            doc = Some(text.trim_end_matches("-/").trim().to_string());
            continue;
        }
        if let Some(printed) = line.strip_prefix("-- printed-as: ") {
            printed_as = Some(printed);
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(kind @ ("theorem" | "def" | "axiom")), Some(name)) = (words.next(), words.next())
        else {
            doc = None;
            continue;
        };
        if line.contains("UNPRINTABLE") {
            // A declaration that fails to print, as Lean's printer can.
            decls.push(
                json!({"name": name, "kind": kind, "error": "printing ran out of heartbeats"}),
            );
            doc = None;
            continue;
        }
        let (explicit, _) = statement_and_value(line, name);
        let explicit = match explicit.char_indices().nth(VALUE_CHARS) {
            Some((end, _)) => format!("{} \u{2026}", &explicit[..end]),
            None => explicit,
        };
        let (statement, value) = statement_and_value(printed_as.take().unwrap_or(line), name);
        decls.push(json!({
            "name": name,
            "kind": kind,
            "signature": format!("{name} {statement}"),
            "type_pp_full": statement,
            "type_explicit": explicit,
            "type_refs": names_in(&statement),
            "value_refs": names_in(&value),
            "docstring": doc.take(),
            "value": (kind == "def").then_some(&value),
        }));
    }
    Value::Array(decls).to_string()
}

/// The text of a declaration line (its keyword, then `name`) between `name`
/// and `:=`, and after `:=`.
fn statement_and_value(line: &str, name: &str) -> (String, String) {
    let after_keyword = line.trim_start().split_once(char::is_whitespace);
    let rest = after_keyword.map_or("", |(_, rest)| rest.trim_start());
    let rest = rest.strip_prefix(name).unwrap_or(rest);
    let (statement, value) = rest.split_once(":=").unwrap_or((rest, ""));
    (statement.trim().to_string(), value.trim().to_string())
}

/// The names `text` mentions, as the constants it would use.
fn names_in(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '\''))
        .filter(|w| !w.is_empty())
        .collect()
}

/// `code` as fake-repl's axiom report sees it, standing in for a build
/// process that prints what it likes: a `-- reported-as: LINE` line puts
/// `LINE` in place of the line after it. The verifier sees the code as it is.
pub fn as_reported(code: &str) -> String {
    let mut out = Vec::new();
    let mut lines = code.lines();
    while let Some(line) = lines.next() {
        match line.strip_prefix("-- reported-as: ") {
            Some(reported) => {
                out.push(reported);
                lines.next();
            }
            None => out.push(line),
        }
    }
    out.join("\n")
}

pub fn declarations(code: &str) -> String {
    declarations_with(code, false)
}

/// [`declarations`], as the build and the verifier describe them: with each
/// declaration's `own_axioms` and the imported theorems it `inherits`
/// `sorry` through, those its line names among [`sorried_imports`].
pub fn linked_declarations(code: &str) -> String {
    declarations_with(code, true)
}

fn declarations_with(code: &str, linked: bool) -> String {
    let imported = imported_code(code).unwrap_or_default();
    let sorried = if linked {
        sorried_imports(code)
    } else {
        Default::default()
    };
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
        // `structure S where FIELDS`: the type, and its constructor, whose
        // "type" is the fields.
        if keyword == "structure" {
            let fields = line.split_once(" where ").map_or("", |(_, f)| f.trim());
            decls.push((
                "inductive",
                name.to_string(),
                vec![],
                "Type".into(),
                String::new(),
                None,
            ));
            let mk = format!("{name}.mk");
            decls.push((
                "constructor",
                mk,
                vec![],
                fields.into(),
                String::new(),
                Some(name),
            ));
            continue;
        }
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
        decls.push((kind, name.to_string(), axioms, statement, value, None));
    }
    let decls: Vec<Value> = decls
        .into_iter()
        .map(|(kind, name, axioms, statement, value, parent)| {
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
            let value_text = (kind == "def").then_some(&value);
            let refs = |text: &str| -> Vec<String> {
                let mut refs: Vec<String> = names_in(text)
                    .into_iter()
                    .filter(|n| *n != name && declares(&imported, code, n))
                    .map(String::from)
                    .collect();
                refs.dedup();
                refs
            };
            let value_refs = match kind {
                "def" => refs(&value),
                "inductive" => vec![format!("{name}.mk")],
                _ => Vec::new(),
            };
            let negates = (kind == "theorem")
                .then(|| statement.strip_prefix(": ¬"))
                .flatten()
                .map(|x| {
                    let x = x.trim();
                    let x = (x.strip_prefix('(').and_then(|x| x.strip_suffix(')'))).unwrap_or(x);
                    format!(": {}", x.trim())
                });
            let mut decl = json!({"name": name, "kind": kind, "axioms": axioms, "assumes": assumes,
                   "type": statement, "value": value_text, "type_refs": refs(&statement),
                   "value_refs": value_refs, "parent": parent, "negates": negates});
            if linked {
                let mut inherits: Vec<(&str, &String)> = names_in(&statement)
                    .into_iter()
                    .chain(names_in(&value))
                    .filter_map(|n| sorried.get_key_value(n).map(|(k, m)| (k.as_str(), m)))
                    .filter(|_| kind == "theorem")
                    .collect();
                inherits.sort();
                inherits.dedup();
                decl["own_axioms"] = json!(axioms);
                if !inherits.is_empty() && !axioms.iter().any(|a| a == "sorryAx") {
                    let mut all = axioms.clone();
                    all.push("sorryAx".into());
                    decl["axioms"] = json!(all);
                }
                decl["inherits"] = inherits
                    .iter()
                    .map(|(n, m)| json!({"name": n, "module": m}))
                    .collect();
            }
            decl
        })
        .collect();
    Value::Array(decls).to_string()
}
