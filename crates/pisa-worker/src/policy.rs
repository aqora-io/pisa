//! The verification policy: what submitted code may contain, and which
//! axioms its declarations may depend on.
//!
//! The textual checks run before any code does. They block metaprogramming,
//! which could otherwise rewrite the environment or fake the axiom report, and
//! options that weaken checking, such as `debug.skipKernelTC`.

use pisa_protocol::header::import_parts;
use pisa_protocol::repl::{CommandResponse, Severity};
use pisa_protocol::verify::{Declaration, Verification};

/// Commands and attributes that run or install metaprograms.
const FORBIDDEN: &[&str] = &[
    "#eval",
    "run_cmd",
    "run_elab",
    "run_meta",
    "run_tac",
    "macro",
    "macro_rules",
    "syntax",
    "elab",
    "elab_rules",
    "declare_syntax_cat",
    "initialize",
    "builtin_initialize",
    "unsafe",
    "implemented_by",
    "extern",
    "command_elab",
    "term_elab",
    "tactic_elab",
    "builtin_command_elab",
    "builtin_term_elab",
    "builtin_tactic",
    "builtin_macro",
];

pub const DEFAULT_AXIOMS: &str = "propext,Classical.choice,Quot.sound";
/// `backward.*` options restore older elaborator behaviour (unification,
/// `dsimp`, match compilation, equation lemmas); the kernel still checks the
/// result. Mathlib-style code sets them often.
pub const DEFAULT_OPTIONS: &str = "maxHeartbeats,maxRecDepth,synthInstance.maxHeartbeats,\
synthInstance.maxSize,autoImplicit,relaxedAutoImplicit,linter.*,pp.*,trace.*,\
exponentiation.threshold,backward.*";

/// `run_cmd` needs this module, so every header a worker imports gets it.
/// Mathlib and most projects import it anyway.
pub const REPORT_IMPORT: &str = "import Lean.Elab.Command";

/// In a module-system header, `run_cmd` runs at compile time, so it needs a
/// `meta` import.
pub const MODULE_REPORT_IMPORT: &str = "meta import Lean.Elab.Command";

pub fn header_with_report_import(header: &str) -> String {
    let module_system = header.lines().next() == Some("module");
    let imported = header
        .lines()
        .filter_map(import_parts)
        .any(|(prefix, modules)| {
            modules.contains(&"Lean.Elab.Command") && (!module_system || prefix.contains("meta"))
        });
    let import = if module_system {
        MODULE_REPORT_IMPORT
    } else {
        REPORT_IMPORT
    };
    if imported {
        header.to_string()
    } else if header.trim().is_empty() {
        import.to_string()
    } else {
        format!("{header}\n{import}")
    }
}

/// Prefix of the info message the axiom report prints.
const MARKER: &str = "PISA_DECLS ";

/// Lists every declaration added after the imports with the axioms it depends
/// on, a hash of its type and, for definitions, of its value. Names are rooted so user declarations can't
/// shadow them.
pub const AXIOM_REPORT: &str = r#"run_cmd do
  let env : _root_.Lean.Environment ← _root_.Lean.MonadEnv.getEnv
  let mut out : Array _root_.Lean.Json := #[]
  for (n, ci) in env.constants.map₂.toList do
    if n.isInternal && !(_root_.Lean.isPrivateName n) then continue
    let kind : String := match ci with
      | .thmInfo _ => "theorem"
      | .defnInfo _ => "def"
      | .axiomInfo _ => "axiom"
      | .opaqueInfo _ => "opaque"
      | .quotInfo _ => "quot"
      | .inductInfo _ => "inductive"
      | .ctorInfo _ => "constructor"
      | .recInfo _ => "recursor"
    let axioms ← _root_.Lean.collectAxioms n
    out := out.push (_root_.Lean.Json.mkObj [
      ("name", _root_.Lean.ToJson.toJson n),
      ("kind", _root_.Lean.ToJson.toJson kind),
      ("axioms", _root_.Lean.ToJson.toJson axioms),
      ("type_hash", _root_.Lean.ToJson.toJson (toString (hash ci.type))),
      ("value_hash", match ci with
        | .defnInfo d => _root_.Lean.ToJson.toJson (toString (hash d.value))
        | _ => _root_.Lean.Json.null)])
  _root_.Lean.logInfo (_root_.Lean.MessageData.ofFormat
    (_root_.Std.Format.text ("PISA_DECLS " ++ (_root_.Lean.Json.arr out).compress)))"#;

/// The loop of [`AXIOM_REPORT`] over the declarations added after the imports.
const REPORT_LOOP: &str = "  for (n, ci) in env.constants.map₂.toList do\n";

/// [`AXIOM_REPORT`] for the declarations of one imported module, e.g. a
/// snapshot (whose own declarations are not "after the imports").
pub fn module_report(module: &str) -> String {
    let report_loop = format!(
        "  let some idx := env.getModuleIdx? `{module} | _root_.Lean.throwError \"{module} is not imported\"\n  \
         for n in env.header.moduleData[idx.toNat]!.constNames do\n    \
         let some ci := env.find? n | continue\n"
    );
    AXIOM_REPORT.replacen(REPORT_LOOP, &report_loop, 1)
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub allowed_axioms: Vec<String>,
    pub allowed_options: Vec<String>,
    pub allow_metaprogramming: bool,
}

impl Policy {
    /// Reasons to reject `body` without running it.
    pub fn scan(&self, body: &str) -> Vec<String> {
        let tokens = tokens(body);
        let mut reasons = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            if !self.allow_metaprogramming && FORBIDDEN.contains(&t.as_str()) {
                reasons.push(format!("forbidden: {t}"));
            }
            if t == "set_option"
                && let Some(option) = tokens.get(i + 1)
                && !self.option_allowed(option)
            {
                reasons.push(format!("option: {option}"));
            }
        }
        reasons.dedup();
        reasons
    }

    fn option_allowed(&self, option: &str) -> bool {
        self.allowed_options
            .iter()
            .any(|a| match a.strip_suffix('*') {
                Some(prefix) => option.starts_with(prefix),
                None => a == option,
            })
    }

    /// Judges a checked body from its response and the axiom report.
    pub fn judge(
        &self,
        response: &CommandResponse,
        report: Option<&CommandResponse>,
    ) -> Verification {
        let mut v = Verification {
            ok: true,
            reasons: Vec::new(),
            declarations: Vec::new(),
        };
        if response.has_errors() {
            v.reasons.push("error: Lean reported errors".into());
        }
        match report.and_then(parse_report) {
            Some(decls) => v.declarations = decls,
            None if response.has_errors() => {}
            None => v.reasons.push("error: axiom report unavailable".into()),
        }
        for d in &v.declarations {
            for ax in d.axioms.iter().filter(|a| !self.allowed_axioms.contains(a)) {
                v.reasons.push(format!("axiom: {} uses {ax}", d.name));
            }
        }
        v.ok = v.reasons.is_empty();
        v
    }
}

pub fn parse_report(report: &CommandResponse) -> Option<Vec<Declaration>> {
    report
        .messages
        .iter()
        .filter(|m| m.severity == Severity::Info)
        .find_map(|m| m.data.strip_prefix(MARKER))
        .and_then(|json| serde_json::from_str::<Vec<Declaration>>(json).ok())
        .map(|mut decls| {
            decls.sort_by(|a, b| a.name.cmp(&b.name));
            decls
        })
}

/// Splits Lean source into identifier-like tokens, skipping comments and
/// string literals.
fn tokens(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '-' && next == Some('-') {
            flush(&mut cur, &mut out);
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('-') {
            flush(&mut cur, &mut out);
            let mut depth = 0;
            while i < chars.len() {
                match (chars[i], chars.get(i + 1).copied()) {
                    ('/', Some('-')) => {
                        depth += 1;
                        i += 2;
                    }
                    ('-', Some('/')) => {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => i += 1,
                }
            }
        } else if c == '"' {
            flush(&mut cur, &mut out);
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i += 1;
        } else if c.is_alphanumeric() || matches!(c, '_' | '.' | '#' | '\'' | '!' | '?') {
            cur.push(c);
            i += 1;
        } else {
            flush(&mut cur, &mut out);
            i += 1;
        }
    }
    flush(&mut cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> Policy {
        Policy {
            allowed_axioms: DEFAULT_AXIOMS.split(',').map(String::from).collect(),
            allowed_options: DEFAULT_OPTIONS.split(',').map(String::from).collect(),
            allow_metaprogramming: false,
        }
    }

    #[test]
    fn scan_flags_metaprogramming_and_options_outside_comments_and_strings() {
        let p = policy();
        assert!(
            p.scan("theorem x : True := trivial -- #eval 1\n/- run_cmd -/")
                .is_empty()
        );
        assert!(p.scan("example : String := \"#eval macro\"").is_empty());
        assert!(
            p.scan("set_option maxHeartbeats 400000 in\nset_option linter.unusedVariables false")
                .is_empty()
        );
        assert_eq!(p.scan("#eval 1"), ["forbidden: #eval"]);
        assert_eq!(
            p.scan("set_option debug.skipKernelTC true in\ntheorem x : False := sorry"),
            ["option: debug.skipKernelTC"]
        );
        assert!(
            p.scan("set_option backward.isDefEq.respectTransparency false in\ntheorem x : True := trivial")
                .is_empty()
        );
        assert_eq!(
            p.scan("@[command_elab foo] def f := 1"),
            ["forbidden: command_elab"]
        );
        // Names merely containing a forbidden word are fine.
        assert!(
            p.scan("theorem elab_ok : True := trivial\n#check Lean.Elab.Command")
                .is_empty()
        );
    }

    fn response(v: serde_json::Value) -> CommandResponse {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn report_import_is_added_once_in_either_kind_of_header() {
        assert_eq!(header_with_report_import(""), REPORT_IMPORT);
        assert_eq!(
            header_with_report_import("import Mathlib\nimport Lean.Elab.Command"),
            "import Mathlib\nimport Lean.Elab.Command"
        );
        assert_eq!(
            header_with_report_import("module\npublic import Mathlib"),
            "module\npublic import Mathlib\nmeta import Lean.Elab.Command"
        );
        // A module needs the `meta` import even if it imports the module.
        assert_eq!(
            header_with_report_import("module\nimport Lean.Elab.Command"),
            "module\nimport Lean.Elab.Command\nmeta import Lean.Elab.Command"
        );
        let has = "module\npublic meta import Lean.Elab.Command";
        assert_eq!(header_with_report_import(has), has);
    }

    #[test]
    fn module_report_walks_the_module() {
        assert!(AXIOM_REPORT.contains(REPORT_LOOP));
        let report = module_report("PisaSnap.s0f");
        assert!(!report.contains("map₂"), "{report}");
        assert!(
            report.contains("env.getModuleIdx? `PisaSnap.s0f |"),
            "{report}"
        );
        assert!(report.contains("    let some ci := env.find? n | continue\n    if n.isInternal"));
    }

    #[test]
    fn judge_rejects_disallowed_axioms_and_errors() {
        let p = policy();
        let report = |decls: serde_json::Value| {
            response(json!({"env": 2, "messages": [{"severity": "info",
                "pos": {"line": 1, "column": 0}, "data": format!("PISA_DECLS {decls}")}]}))
        };
        let ok = report(json!([{"name": "x", "kind": "theorem",
            "axioms": ["propext", "Quot.sound"], "type_hash": "1"}]));
        let v = p.judge(&response(json!({"env": 1})), Some(&ok));
        assert!(v.ok, "{v:?}");
        assert_eq!(v.declarations[0].name, "x");

        let bad = report(json!([{"name": "x", "kind": "theorem",
            "axioms": ["sorryAx"], "type_hash": "1"},
            {"name": "y", "kind": "theorem", "axioms": ["Lean.ofReduceBool"], "type_hash": "2"}]));
        let v = p.judge(&response(json!({"env": 1})), Some(&bad));
        assert!(!v.ok);
        assert_eq!(
            v.reasons,
            ["axiom: x uses sorryAx", "axiom: y uses Lean.ofReduceBool"]
        );

        let errors = response(json!({"env": 1, "messages": [{"severity": "error",
            "pos": {"line": 1, "column": 0}, "data": "boom"}]}));
        let v = p.judge(&errors, None);
        assert_eq!(v.reasons, ["error: Lean reported errors"]);

        let v = p.judge(
            &response(json!({"env": 1})),
            Some(&response(json!({"env": 2}))),
        );
        assert_eq!(v.reasons, ["error: axiom report unavailable"]);
    }
}
