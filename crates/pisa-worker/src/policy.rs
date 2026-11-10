//! The verification policy: what submitted code may contain, and which
//! axioms its declarations may depend on.
//!
//! The textual checks run before any code does. They block metaprogramming,
//! which could otherwise rewrite the environment or fake the axiom report, and
//! options that weaken checking, such as `debug.skipKernelTC`.

use pisa_protocol::header::import_parts;
use pisa_protocol::repl::{CommandResponse, Message, Severity};
use pisa_protocol::verify::{DescribedDeclaration, ProofStatus, ReportedDeclaration, Verification};

use crate::catalog::SnapshotDecl;

/// Commands and attributes that run or install metaprograms.
const FORBIDDEN: &[&str] = &[
    "#eval",
    "run_cmd",
    "run_elab",
    "run_meta",
    "run_tac",
    "elab",
    "elab_rules",
    "initialize",
    "builtin_initialize",
    "unsafe",
    "simproc",
    "dsimproc",
    "simproc_decl",
    "dsimproc_decl",
    "builtin_simproc",
];

/// Attributes that run code, or install it to run while later code
/// elaborates. Their names are words code may use as names (`init`,
/// `tactic`, `extern`), so they are forbidden only where an attribute is
/// named: in `@[…]` and `attribute […]`.
const FORBIDDEN_ATTRIBUTES: &[&str] = &[
    "implemented_by",
    "extern",
    "command_elab",
    "term_elab",
    "tactic_elab",
    "builtin_command_elab",
    "builtin_term_elab",
    "builtin_tactic",
    "builtin_macro",
    "init",
    "builtin_init",
    "tactic",
    "delab",
    "app_delab",
    "app_unexpander",
    "simproc",
    "norm_num",
    "positivity",
];

/// Pure syntax transformers: they can't run code, do IO or add declarations,
/// so their only risk is what they expand into. Snapshots allow them because
/// code that uses them is re-checked through the kernel unless the request
/// opts out, and is then marked as not verified; other endpoints don't.
const MACRO_FAMILY: &[&str] = &["macro", "macro_rules", "syntax", "declare_syntax_cat"];

pub const DEFAULT_AXIOMS: &str = "propext,Classical.choice,Quot.sound";
/// `backward.*` options restore older elaborator behaviour (unification,
/// `dsimp`, match compilation, equation lemmas); the kernel still checks the
/// result. Mathlib-style code sets them often.
/// `diagnostics` is what Lean's own heartbeat message suggests, and notation
/// over section variables needs `quotPrecheck`.
pub const DEFAULT_OPTIONS: &str = "maxHeartbeats,maxRecDepth,synthInstance.maxHeartbeats,\
synthInstance.maxSize,maxSynthPendingDepth,autoImplicit,relaxedAutoImplicit,linter.*,pp.*,\
trace.*,exponentiation.threshold,backward.*,diagnostics,quotPrecheck*";

/// The report is the `Pisa` library's (`docker/pisa-lean`), so every header a
/// worker imports gets it. It brings `Lean.Elab.Command`, which `run_cmd`
/// needs.
pub const REPORT_IMPORT: &str = "import Pisa";

/// In a module-system header, `run_cmd` runs at compile time, so it needs a
/// `meta` import.
pub const MODULE_REPORT_IMPORT: &str = "meta import Pisa";

/// Lean defaults `autoImplicit` to true, which turns a misspelt name in a
/// statement into a new implicit argument instead of an error. Every header
/// command ends with this line and the commands that follow reuse its scopes,
/// so submitted code starts with the option off. It can opt back in with
/// `set_option autoImplicit true`, which stays on the allowlist.
pub const AUTO_IMPLICIT_OFF: &str = "set_option autoImplicit false";

/// What a worker sends the REPL for a header: the header with the report
/// import, then [`AUTO_IMPLICIT_OFF`]. It is built after the header's key, so
/// the added lines change no key, and the REPL numbers each command's lines
/// from 1, so they move no position in the code that follows.
pub fn header_command(header: &str) -> String {
    let module_system = header.lines().next() == Some("module");
    let imported = header
        .lines()
        .filter_map(import_parts)
        .any(|(prefix, modules)| {
            modules.contains(&"Pisa") && (!module_system || prefix.contains("meta"))
        });
    let import = if module_system {
        MODULE_REPORT_IMPORT
    } else {
        REPORT_IMPORT
    };
    let imports = if imported {
        header.to_string()
    } else if header.trim().is_empty() {
        import.to_string()
    } else {
        format!("{header}\n{import}")
    };
    format!("{imports}\n{AUTO_IMPLICIT_OFF}")
}

/// Prefix of the info message the axiom report prints.
const MARKER: &str = "PISA_DECLS ";
/// What marks the declarations a report on code couldn't describe.
const FAILED_MARKER: &str = "PISA_REPORT_FAILED ";

/// Describes every declaration added after the imports (see
/// `docker/pisa-lean/Pisa/Report.lean`): the axioms it depends on, the
/// canonical text of its type and, for definitions, of its value, which
/// [`parse_report`] hashes, and the constants of this code and of snapshots
/// that its type and value use (`type_refs`, `value_refs`; an inductive's
/// "value" is its constructors and recursor). `generated` marks what Lean
/// makes on demand, such as equation lemmas, which a replacement needn't
/// have. The kernel verifier (`docker/verifier/PisaVerify.lean`) describes
/// declarations with the same code. Rooted, so user declarations can't shadow
/// it.
pub const AXIOM_REPORT: &str = "run_cmd _root_.Pisa.reportNew";

/// [`AXIOM_REPORT`] for the declarations of one imported module, e.g. a
/// snapshot (whose own declarations are not "after the imports").
pub fn module_report(module: &str) -> String {
    format!("run_cmd _root_.Pisa.reportModule `{module}")
}

const EXPORT_MARKER: &str = "PISA_EXPORT ";

/// What the search service indexes of each declaration of a snapshot module:
/// [`SnapshotDecl`]s as JSON, without `uses_sorry` and `status`, which come
/// from the check result. Skips what `search/exporter/PisaExport.lean`
/// skips: internal, private and auto-generated names. Docstrings of a
/// module-system snapshot aren't loaded in a module-system header, so they
/// are missing there.
const MODULE_EXPORT: &str = r#"run_cmd do
  let env : _root_.Lean.Environment ← _root_.Lean.MonadEnv.getEnv
  let some idx := env.getModuleIdx? `{module} | _root_.Lean.throwError "{module} is not imported"
  let out ← _root_.Lean.Elab.Command.liftTermElabM do
    let mut out : Array _root_.Lean.Json := #[]
    for n in env.header.moduleData[idx.toNat]!.constNames do
      let some ci := env.find? n | continue
      if n.isInternalDetail || _root_.Lean.isPrivateName n then continue
      if n matches .str _ "inj" || n matches .str _ "injEq" || n matches .str _ "noConfusionType" then continue
      if _root_.Lean.isAuxRecursor env n || _root_.Lean.isNoConfusion env n then continue
      if ← _root_.Lean.isRec n <||> _root_.Lean.Meta.isMatcher n then continue
      -- Each declaration gets its own heartbeat budget.
      let record? : Option _root_.Lean.Json ← _root_.Lean.Core.withCurrHeartbeats do try
          let signature ← _root_.Lean.MonadWithOptions.withOptions (fun o => o |>.setBool `pp.deepTerms true
              |>.set `pp.deepTerms.threshold (10000 : Nat) |>.set `pp.maxSteps (100000 : Nat)) do
            return (← _root_.Lean.PrettyPrinter.ppSignature n).fmt.pretty 120
          let typeFull ← _root_.Lean.MonadWithOptions.withOptions (fun o => o |>.setBool `pp.notation false
              |>.setBool `pp.fullNames true |>.setBool `pp.deepTerms true) do
            return (← _root_.Lean.Meta.ppExpr ci.type).pretty 120
          let kind : String := match ci with
            | .axiomInfo _ => "axiom"
            | .defnInfo d =>
              if _root_.Lean.Meta.isInstanceCore env d.name then "instance"
              else if _root_.Lean.getReducibilityStatusCore env d.name == _root_.Lean.ReducibilityStatus.reducible then "abbrev"
              else "def"
            | .thmInfo _ => "theorem"
            | .opaqueInfo _ => "opaque"
            | .quotInfo _ => "quot"
            | .inductInfo i =>
              if _root_.Lean.isClass env i.name then "class"
              else if _root_.Lean.isStructure env i.name then "structure"
              else "inductive"
            | .ctorInfo _ => "constructor"
            | .recInfo _ => "recursor"
          let typeRefs := ci.type.getUsedConstants.filter (· != n)
          let valueRefs := match ci.value? (allowOpaque := true) with
            | some v => v.getUsedConstantsAsSet.toArray.filter (fun r => r != n && !r.isInternalDetail)
            | none => #[]
          let doc : Option String := match ← _root_.Lean.findInternalDocString? env n with
            | some (.inl s) => some s
            | _ => none
          let deprecated := _root_.Lean.Linter.deprecatedAttr.getParam? env n
          pure (some (_root_.Lean.Json.mkObj [
            ("name", _root_.Lean.ToJson.toJson n),
            ("kind", _root_.Lean.ToJson.toJson kind),
            ("signature", _root_.Lean.ToJson.toJson signature),
            ("type_pp_full", _root_.Lean.ToJson.toJson typeFull),
            ("type_refs", _root_.Lean.ToJson.toJson typeRefs),
            ("value_refs", _root_.Lean.ToJson.toJson valueRefs),
            ("docstring", _root_.Lean.ToJson.toJson doc),
            ("deprecated_to", _root_.Lean.ToJson.toJson (deprecated.bind (·.newName?)))]))
        catch _ => pure none
      if let some r := record? then out := out.push r
    return out
  _root_.Lean.logInfo (_root_.Lean.MessageData.ofFormat
    (_root_.Std.Format.text ("PISA_EXPORT " ++ (_root_.Lean.Json.arr out).compress)))"#;

/// The export of the declarations of `module`, which the process imports.
pub fn module_export(module: &str) -> String {
    MODULE_EXPORT.replace("{module}", module)
}

pub fn parse_export(report: &CommandResponse) -> Option<Vec<SnapshotDecl>> {
    report
        .messages
        .iter()
        .filter(|m| m.severity == Severity::Info)
        .find_map(|m| m.data.strip_prefix(EXPORT_MARKER))
        .and_then(|json| serde_json::from_str::<Vec<SnapshotDecl>>(json).ok())
        .map(|mut decls| {
            decls.sort_by(|a, b| a.name.cmp(&b.name));
            decls
        })
}

/// Whether `body` uses macros, or other code that runs at elaboration time
/// (which only a pool with `allow_metaprogramming` lets through). What these
/// expand into is not in the text the scan sees, so by default a snapshot that
/// uses them is re-checked through the kernel.
pub fn uses_macros(body: &str) -> bool {
    let tokens = tokens(body);
    let command = |t: &&Token| !t.quoted;
    tokens
        .iter()
        .filter(command)
        .any(|t| FORBIDDEN.contains(&t.text.as_str()) || MACRO_FAMILY.contains(&t.text.as_str()))
        || attributes(&tokens).any(|a| FORBIDDEN_ATTRIBUTES.contains(&a.text.as_str()))
}

/// A reason the scan refuses code, where it is, and what to do instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// As in a check's `reasons`, e.g. `forbidden: #eval`.
    pub reason: String,
    /// Where in the scanned text: the line from 1 and the column from 0, in
    /// code points.
    pub line: u64,
    pub column: u64,
    pub hint: String,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub allowed_axioms: Vec<String>,
    pub allowed_options: Vec<String>,
    pub allow_metaprogramming: bool,
}

impl Policy {
    /// Reasons to reject `body` without running it. `allow_macros` permits
    /// the macro family, for code that can be re-checked through the kernel
    /// (snapshots).
    pub fn scan(&self, body: &str, allow_macros: bool) -> Vec<String> {
        let found = self.findings(body, allow_macros);
        found.into_iter().map(|f| f.reason).collect()
    }

    /// The same as [`scan`](Self::scan), each reason once, at the first
    /// place it is found, with a hint.
    pub fn findings(&self, body: &str, allow_macros: bool) -> Vec<Finding> {
        let tokens = tokens(body);
        let mut found: Vec<Finding> = Vec::new();
        let mut add = |reason: String, t: &Token, hint: String| {
            if !found.iter().any(|f| f.reason == reason) {
                let (line, column) = position(body, t.at);
                found.push(Finding {
                    reason,
                    line,
                    column,
                    hint,
                });
            }
        };
        for (i, t) in tokens.iter().enumerate() {
            let text = t.text.as_str();
            let macros = !allow_macros && MACRO_FAMILY.contains(&text);
            if !self.allow_metaprogramming && !t.quoted && (FORBIDDEN.contains(&text) || macros) {
                let hint = if macros {
                    "the macro family (`macro`, `macro_rules`, `syntax`, `declare_syntax_cat`) is \
                     allowed only in snapshots, which re-check what it expands into through the \
                     kernel: put it in one with POST /snapshots and import that"
                        .to_string()
                } else {
                    format!(
                        "`{text}` runs a metaprogram or unsafe code, which is refused before the code runs"
                    )
                };
                add(format!("forbidden: {text}"), t, hint);
            }
            if text == "set_option"
                && let Some(option) = tokens.get(i + 1)
                && !self.option_allowed(&option.text)
            {
                let hint = format!(
                    "the options code may set are {}",
                    self.allowed_options.join(", ")
                );
                add(format!("option: {}", option.text), option, hint);
            }
        }
        if !self.allow_metaprogramming {
            let named =
                attributes(&tokens).filter(|a| FORBIDDEN_ATTRIBUTES.contains(&a.text.as_str()));
            for a in named {
                let hint = format!(
                    "the `{}` attribute runs or installs code that runs while later code elaborates",
                    a.text
                );
                add(format!("forbidden: {}", a.text), a, hint);
            }
        }
        found
    }

    fn option_allowed(&self, option: &str) -> bool {
        self.allowed_options
            .iter()
            .any(|a| match a.strip_suffix('*') {
                Some(prefix) => option.starts_with(prefix),
                None => a == option,
            })
    }

    /// Judges a checked body from its response and the axiom report, if it
    /// ran: what the report couldn't describe, or why it failed, is a reason.
    pub fn judge(
        &self,
        response: &CommandResponse,
        report: Option<&CommandResponse>,
    ) -> Verification {
        let errors = response.has_errors();
        let Some(report) = report else {
            return self.judge_decls(errors, None);
        };
        let (mut v, problems) = match read_report(report) {
            Ok(r) => {
                let problems = r
                    .failed
                    .into_iter()
                    .map(|e| format!("error: axiom report: {e}"));
                (
                    self.judge_decls(errors, Some(r.declarations)),
                    problems.collect(),
                )
            }
            Err(said) if said.is_empty() => (
                self.judge_decls(errors, Some(Vec::new())),
                vec!["error: axiom report unavailable".to_string()],
            ),
            Err(said) => (
                self.judge_decls(errors, Some(Vec::new())),
                vec![format!("error: axiom report unavailable: {said}")],
            ),
        };
        v.reasons.extend(problems);
        v.ok = v.reasons.is_empty();
        v
    }

    /// Judges from whether the body had errors and the declarations the axiom
    /// report found (from the REPL report, or a snapshot's kernel re-check),
    /// each with its status.
    pub fn judge_decls(
        &self,
        errors: bool,
        declarations: Option<Vec<ReportedDeclaration>>,
    ) -> Verification {
        let mut v = Verification {
            ok: true,
            reasons: Vec::new(),
            declarations: Vec::new(),
            changes: Vec::new(),
        };
        if errors {
            v.reasons.push("error: Lean reported errors".into());
        }
        match declarations {
            Some(decls) => {
                v.declarations = decls
                    .into_iter()
                    .map(|d| {
                        let status = self.status(&d);
                        d.with_status(status)
                    })
                    .collect();
            }
            None if errors => {}
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

impl Policy {
    /// What `d`'s axioms make it: besides the allowed ones, none is
    /// `proved`; only assumptions (`d.assumes`) `conditional`; `sorryAx`,
    /// with or without assumptions, `sorry`; anything else `rejected`.
    pub fn status(&self, d: &ReportedDeclaration) -> ProofStatus {
        let each = d
            .axioms
            .iter()
            .filter(|a| !self.allowed_axioms.contains(a))
            .map(|a| {
                if d.assumes.contains(a) {
                    ProofStatus::Conditional
                } else if a == "sorryAx" {
                    ProofStatus::Sorry
                } else {
                    ProofStatus::Rejected
                }
            });
        each.max().unwrap_or(ProofStatus::Proved)
    }
}

/// What the axiom report on code says: the declarations it described, and
/// why it couldn't describe others, e.g. `describing x: a metavariable`.
pub struct Report {
    pub declarations: Vec<ReportedDeclaration>,
    pub failed: Vec<String>,
}

/// The axiom report's response read, or what Lean said if it has no
/// declarations.
pub fn read_report(report: &CommandResponse) -> Result<Report, String> {
    #[derive(serde::Deserialize)]
    struct Failure {
        error: String,
    }
    let info = |marker: &str| {
        let mut infos = report
            .messages
            .iter()
            .filter(|m| m.severity == Severity::Info);
        infos.find_map(|m| m.data.strip_prefix(marker))
    };
    let Some(declarations) =
        info(MARKER).and_then(|json| parse_declarations(serde_json::from_str(json).ok()?))
    else {
        let errors = report
            .messages
            .iter()
            .filter(|m| m.severity == Severity::Error);
        return Err(errors
            .map(|m| m.data.as_str())
            .collect::<Vec<_>>()
            .join("; "));
    };
    let failed = match info(FAILED_MARKER) {
        None => Vec::new(),
        Some(json) => match serde_json::from_str::<Vec<Failure>>(json) {
            Ok(failed) => failed.into_iter().map(|f| f.error).collect(),
            Err(_) => vec!["what it couldn't describe doesn't parse".into()],
        },
    };
    Ok(Report {
        declarations,
        failed,
    })
}

/// The declarations of a module's report, which must describe them all.
pub fn parse_report(report: &CommandResponse) -> Option<Vec<ReportedDeclaration>> {
    let read = read_report(report).ok()?;
    read.failed.is_empty().then_some(read.declarations)
}

/// Declarations as `Pisa`'s `describe` gives them, in the report or the
/// kernel verifier's verdict, by name, with their texts hashed. `None` if
/// they aren't descriptions.
pub fn parse_declarations(json: serde_json::Value) -> Option<Vec<ReportedDeclaration>> {
    let decls: Vec<DescribedDeclaration> = serde_json::from_value(json).ok()?;
    let mut decls: Vec<ReportedDeclaration> = decls.into_iter().map(Into::into).collect();
    decls.sort_by(|a, b| a.name.cmp(&b.name));
    Some(decls)
}

/// A token of Lean source: its text, where it starts (in characters), and
/// whether it is a name in `«»`, which no command or keyword can be.
struct Token {
    text: String,
    at: usize,
    quoted: bool,
}

/// Splits Lean source into identifier-like tokens, with the brackets,
/// parentheses, commas and `@` that [`attributes`] reads as tokens of their
/// own, skipping comments and string and character literals. A name in
/// `«»` is one token, without them, as Lean reads it. `#` starts a token, as
/// no name has one.
///
/// An interpolated string (`s!"…{code}…"`) nests code in a string, to any
/// depth, so what the lexer is inside is a stack it keeps, not its own.
fn tokens(src: &str) -> Vec<Token> {
    /// What the lexer is inside, innermost last.
    enum Inside {
        /// Code, with the braces open in it; an interpolation's code ends at
        /// the `}` that closes none of them.
        Code { braces: usize },
        /// An interpolated string.
        Interpolated,
    }
    /// The token being read.
    struct Cur {
        text: String,
        at: usize,
        quoted: bool,
    }
    impl Cur {
        fn push(&mut self, c: char, i: usize) {
            if self.text.is_empty() {
                self.at = i;
            }
            self.text.push(c);
        }
        fn flush(&mut self, out: &mut Vec<Token>) {
            if !self.text.is_empty() {
                out.push(Token {
                    text: std::mem::take(&mut self.text),
                    at: self.at,
                    quoted: self.quoted,
                });
            }
            self.quoted = false;
        }
    }
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut cur = Cur {
        text: String::new(),
        at: 0,
        quoted: false,
    };
    let mut inside = vec![Inside::Code { braces: 0 }];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let depth = inside.len();
        let Some(Inside::Code { braces }) = inside.last_mut() else {
            match c {
                '\\' => i += 2,
                '"' => {
                    inside.pop();
                    i += 1;
                }
                '{' => {
                    inside.push(Inside::Code { braces: 0 });
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        };
        if c == '-' && next == Some('-') {
            cur.flush(&mut out);
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('-') {
            cur.flush(&mut out);
            let mut nested = 0;
            while i < chars.len() {
                match (chars[i], chars.get(i + 1).copied()) {
                    ('/', Some('-')) => {
                        nested += 1;
                        i += 2;
                    }
                    ('-', Some('/')) => {
                        nested -= 1;
                        i += 2;
                        if nested == 0 {
                            break;
                        }
                    }
                    _ => i += 1,
                }
            }
        } else if c == '"' {
            // `s!"…"` and the like interpolate `{…}`.
            let interpolated = cur.text.ends_with('!');
            cur.flush(&mut out);
            i += 1;
            if interpolated {
                inside.push(Inside::Interpolated);
            } else {
                while i < chars.len() && chars[i] != '"' {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
                i += 1;
            }
        } else if let Some(end) = cur.text.is_empty().then(|| raw_string(&chars, i)).flatten() {
            i = end;
        } else if let Some(end) = cur
            .text
            .is_empty()
            .then(|| char_literal(&chars, i))
            .flatten()
        {
            i = end;
        } else if c == '«' {
            if cur.text.is_empty() {
                cur.at = i;
                cur.quoted = true;
            }
            i += 1;
            while i < chars.len() && chars[i] != '»' {
                cur.text.push(chars[i]);
                i += 1;
            }
            i += 1;
            // The name goes on only through a dot.
            if chars.get(i) != Some(&'.') {
                cur.flush(&mut out);
            }
        } else if c == '#' {
            cur.flush(&mut out);
            cur.push(c, i);
            i += 1;
        } else if c.is_alphanumeric() || matches!(c, '_' | '.' | '\'' | '!' | '?') {
            cur.push(c, i);
            i += 1;
        } else {
            cur.flush(&mut out);
            match c {
                '{' => *braces += 1,
                '}' if *braces == 0 && depth > 1 => {
                    inside.pop();
                }
                '}' => *braces = braces.saturating_sub(1),
                '@' | '[' | ']' | '(' | ')' | ',' => out.push(Token {
                    text: c.to_string(),
                    at: i,
                    quoted: false,
                }),
                _ => {}
            }
            i += 1;
        }
    }
    cur.flush(&mut out);
    out
}

/// The end of the raw string literal (`r"…"`, `r#"…"#`, …) at `i`, if
/// there is one.
fn raw_string(chars: &[char], i: usize) -> Option<usize> {
    if chars[i] != 'r' {
        return None;
    }
    let hashes = chars[i + 1..].iter().take_while(|&&c| c == '#').count();
    let mut j = i + 1 + hashes;
    if chars.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    while j < chars.len() {
        if chars[j] == '"'
            && chars[j + 1..]
                .iter()
                .take(hashes)
                .filter(|&&c| c == '#')
                .count()
                == hashes
        {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(j)
}

/// The end of the character literal (`'a'`, `'\n'`, `'\x41'`, `'\u{…}'`) at
/// `i`, if there is one.
fn char_literal(chars: &[char], i: usize) -> Option<usize> {
    if chars[i] != '\'' {
        return None;
    }
    let mut j = i + 1;
    match chars.get(j)? {
        '\\' => {
            j += 1;
            match chars.get(j)? {
                'u' if chars.get(j + 1) == Some(&'{') => {
                    j += chars[j..].iter().position(|&c| c == '}')? + 1;
                }
                'x' => j += 3,
                _ => j += 1,
            }
        }
        _ => j += 1,
    }
    (chars.get(j) == Some(&'\'')).then_some(j + 1)
}

/// The attributes `tokens` applies, in `@[…]` and `attribute […]`: the
/// first name of each entry, after `scoped` or `local`.
fn attributes(tokens: &[Token]) -> impl Iterator<Item = &Token> {
    let mut names = Vec::new();
    let mut i = 0;
    while i + 1 < tokens.len() {
        let opens =
            matches!(tokens[i].text.as_str(), "@" | "attribute") && tokens[i + 1].text == "[";
        i += 1;
        if !opens {
            continue;
        }
        // Inside the list: brackets and parentheses nest, and a comma at
        // the top starts the next entry.
        let (mut depth, mut entry_start) = (0usize, true);
        i += 1;
        while i < tokens.len() {
            let t = &tokens[i];
            match t.text.as_str() {
                "[" | "(" => depth += 1,
                "]" if depth == 0 => break,
                "]" | ")" => depth = depth.saturating_sub(1),
                "," if depth == 0 => entry_start = true,
                "scoped" | "local" if entry_start => {}
                "@" | "," => {}
                _ if entry_start && depth == 0 => {
                    names.push(t);
                    entry_start = false;
                }
                _ => {}
            }
            i += 1;
        }
    }
    names.into_iter()
}

/// A hint at the first error in `messages` that `autoImplicit` being off may
/// cause, unless `body` sets the option itself: an unknown identifier or
/// universe level, which code written for a library that turns the option on
/// (Physlib does) meets here, or Lean's own note on the option.
pub fn auto_implicit_hint(body: &str, messages: &[Message]) -> Option<Message> {
    if tokens(body).iter().any(|t| t.text == "autoImplicit") {
        return None;
    }
    let cause = messages.iter().find(|m| {
        m.severity == Severity::Error
            && (m.data.starts_with("Unknown identifier")
                || m.data.starts_with("unknown universe level")
                || m.data.contains("`autoImplicit`"))
    })?;
    let name = cause
        .data
        .split('`')
        .nth(1)
        .filter(|n| !n.is_empty() && *n != "autoImplicit");
    let bind = match name {
        Some(n) => format!("bind `{n}` as an argument, as in `{{{n}}}`"),
        None => "bind it as an argument".to_string(),
    };
    Some(Message {
        severity: Severity::Info,
        pos: cause.pos,
        end_pos: None,
        data: format!(
            "pisa checks code with `autoImplicit` off. If the name is meant to be bound \
             automatically, as in a library that turns the option on, {bind}, or add \
             `set_option autoImplicit true`"
        ),
    })
}

/// The line, from 1, and column, from 0 in code points, of the character at
/// `at` in `src`.
fn position(src: &str, at: usize) -> (u64, u64) {
    let (mut line, mut column) = (1, 0);
    for c in src.chars().take(at) {
        if c == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
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
    fn a_status_tells_assumptions_from_sorry_and_the_rest() {
        let p = policy();
        let decl = |axioms: &[&str], assumes: &[&str]| ReportedDeclaration {
            name: "t".into(),
            axioms: axioms.iter().map(|a| a.to_string()).collect(),
            assumes: assumes.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        };
        let none: &[&str] = &[];
        for (axioms, assumes, status) in [
            (&["propext"][..], none, ProofStatus::Proved),
            (
                &["propext", "kunneth"],
                &["kunneth"][..],
                ProofStatus::Conditional,
            ),
            (&["sorryAx"], none, ProofStatus::Sorry),
            (&["sorryAx", "kunneth"], &["kunneth"], ProofStatus::Sorry),
            (
                &["t._native.native_decide.ax_1_1"],
                none,
                ProofStatus::Rejected,
            ),
            (
                &["sorryAx", "Lean.trustCompiler", "kunneth"],
                &["kunneth"],
                ProofStatus::Rejected,
            ),
        ] {
            assert_eq!(p.status(&decl(axioms, assumes)), status, "{axioms:?}");
        }
        assert!(ProofStatus::Rejected > ProofStatus::Sorry);
        assert!(ProofStatus::Sorry > ProofStatus::Conditional);
        assert!(ProofStatus::Conditional > ProofStatus::Proved);
        // Judging sets each declaration's status, and `ok` stays as it was.
        let v = p.judge_decls(false, Some(vec![decl(&["kunneth"], &["kunneth"])]));
        assert_eq!(v.declarations[0].status, ProofStatus::Conditional);
        assert!(!v.ok);
    }

    #[test]
    fn scan_flags_metaprogramming_and_options_outside_comments_and_strings() {
        let p = policy();
        assert!(
            p.scan(
                "theorem x : True := trivial -- #eval 1\n/- run_cmd -/",
                false
            )
            .is_empty()
        );
        assert!(
            p.scan("example : String := \"#eval macro\"", false)
                .is_empty()
        );
        assert!(
            p.scan(
                "set_option maxHeartbeats 400000 in\nset_option linter.unusedVariables false",
                false
            )
            .is_empty()
        );
        assert_eq!(p.scan("#eval 1", false), ["forbidden: #eval"]);
        assert_eq!(
            p.scan(
                "set_option debug.skipKernelTC true in\ntheorem x : False := sorry",
                false
            ),
            ["option: debug.skipKernelTC"]
        );
        assert!(
            p.scan("set_option backward.isDefEq.respectTransparency false in\ntheorem x : True := trivial", false)
                .is_empty()
        );
        assert_eq!(
            p.scan("@[command_elab foo] def f := 1", false),
            ["forbidden: command_elab"]
        );
        // Names merely containing a forbidden word are fine.
        assert!(
            p.scan(
                "theorem elab_ok : True := trivial\n#check Lean.Elab.Command",
                false
            )
            .is_empty()
        );
    }

    fn response(v: serde_json::Value) -> CommandResponse {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn header_command_imports_the_report_once_and_turns_auto_implicit_off() {
        let off = "set_option autoImplicit false";
        assert_eq!(AUTO_IMPLICIT_OFF, off);
        assert_eq!(
            header_command(""),
            "import Pisa\nset_option autoImplicit false"
        );
        assert_eq!(
            header_command("import Mathlib"),
            "import Mathlib\nimport Pisa\nset_option autoImplicit false"
        );
        assert_eq!(
            header_command("import Mathlib\nimport Pisa"),
            "import Mathlib\nimport Pisa\nset_option autoImplicit false"
        );
        assert_eq!(
            header_command("module\npublic import Foo"),
            "module\npublic import Foo\nmeta import Pisa\nset_option autoImplicit false"
        );
        // A module needs the `meta` import even if it imports the module.
        assert_eq!(
            header_command("module\nimport Pisa"),
            "module\nimport Pisa\nmeta import Pisa\nset_option autoImplicit false"
        );
        assert_eq!(
            header_command("module\npublic meta import Pisa"),
            "module\npublic meta import Pisa\nset_option autoImplicit false"
        );
        for header in ["", "import Mathlib", "module\npublic import Foo"] {
            assert_eq!(header_command(header).lines().last(), Some(off));
        }
    }

    #[test]
    fn an_unknown_name_hints_that_auto_implicit_is_off() {
        use pisa_protocol::repl::{Message, Pos};
        let error = |data: &str| Message {
            severity: Severity::Error,
            pos: Pos {
                line: 2,
                column: 12,
            },
            end_pos: None,
            data: data.into(),
        };
        let code = "theorem t (x : Fin d) : x = x := rfl";
        let hint = auto_implicit_hint(code, &[error("Unknown identifier `d`")]).unwrap();
        assert_eq!(
            (hint.severity, hint.pos.line, hint.pos.column),
            (Severity::Info, 2, 12)
        );
        assert!(hint.data.contains("`{d}`"), "{}", hint.data);
        assert!(
            hint.data.contains("set_option autoImplicit true"),
            "{}",
            hint.data
        );
        // Universe levels too.
        assert!(auto_implicit_hint(code, &[error("unknown universe level `u`")]).is_some());
        // Not if the code sets the option itself, nor for other errors.
        let set = format!("set_option autoImplicit false\n{code}");
        assert!(auto_implicit_hint(&set, &[error("Unknown identifier `d`")]).is_none());
        assert!(auto_implicit_hint(code, &[error("Type mismatch")]).is_none());
    }

    #[test]
    fn options_that_lean_suggests_may_be_set() {
        let p = policy();
        for code in [
            "set_option diagnostics true in\ntheorem t : True := trivial",
            "set_option maxSynthPendingDepth 3 in\nexample : True := trivial",
            "set_option quotPrecheck false in\nnotation \"d\" => 2",
            "set_option quotPrecheck.allowSectionVars true",
        ] {
            assert!(p.scan(code, false).is_empty(), "{code}");
        }
    }

    #[test]
    fn a_quoted_name_or_an_attribute_s_name_is_just_a_name() {
        let p = policy();
        for code in [
            "def «initialize» : Nat := 1",
            "def «#eval» : Nat := 1",
            "def extern : Nat := 1",
            "theorem implemented_by : True := trivial",
        ] {
            assert!(p.scan(code, false).is_empty(), "{code}");
        }
        // As attributes they are refused.
        for (code, reason) in [
            ("@[extern \"c_f\"] opaque f : Nat", "forbidden: extern"),
            (
                "@[implemented_by g] def f := 1",
                "forbidden: implemented_by",
            ),
            ("attribute [command_elab foo] f", "forbidden: command_elab"),
            ("@[term_elab foo] def e := 1", "forbidden: term_elab"),
            (
                "@[builtin_tactic foo] def t := 1",
                "forbidden: builtin_tactic",
            ),
        ] {
            assert_eq!(p.scan(code, false), [reason], "{code}");
            assert!(uses_macros(code), "{code}");
        }
    }

    #[test]
    fn a_finding_says_where_and_what_to_do() {
        let p = policy();
        let found = p.findings("theorem t : True := trivial\n  syntax \"x\" : term", false);
        let first = &found[0];
        assert_eq!(
            (first.reason.as_str(), first.line, first.column),
            ("forbidden: syntax", 2, 2)
        );
        assert!(first.hint.contains("POST /snapshots"), "{}", first.hint);
        let found = p.findings("set_option debug.skipKernelTC true", false);
        assert_eq!(
            (found[0].reason.as_str(), found[0].line, found[0].column),
            ("option: debug.skipKernelTC", 1, 11)
        );
        assert!(found[0].hint.contains("maxHeartbeats"), "{}", found[0].hint);
        let found = p.findings("@[init] def f : IO Unit := pure ()", false);
        assert_eq!((found[0].line, found[0].column), (1, 2));
    }

    #[test]
    fn attributes_that_install_code_are_refused_where_they_are_attributes() {
        let p = policy();
        for (code, reason) in [
            ("@[init] def f : IO Unit := pure ()", "forbidden: init"),
            (
                "@[simp, builtin_init] def f : IO Unit := pure ()",
                "forbidden: builtin_init",
            ),
            (
                "def f : IO Unit := pure ()\nattribute [init] f",
                "forbidden: init",
            ),
            ("@[app_delab f] def d := 1", "forbidden: app_delab"),
            ("@[delab app.f] def d := 1", "forbidden: delab"),
            (
                "@[app_unexpander f] def d := 1",
                "forbidden: app_unexpander",
            ),
            ("@[tactic foo] def t := 1", "forbidden: tactic"),
            ("@[local simproc] def s := 1", "forbidden: simproc"),
            ("@[norm_num _ + _] def e := 1", "forbidden: norm_num"),
            ("@[positivity _ + _] def e := 1", "forbidden: positivity"),
            ("@[«init»] def f : IO Unit := pure ()", "forbidden: init"),
            (
                "@[«implemented_by» f] def g := 1",
                "forbidden: implemented_by",
            ),
        ] {
            assert_eq!(p.scan(code, true), [reason], "{code}");
            assert!(uses_macros(code), "{code}");
        }
        for code in [
            "def f (init : Nat) : Nat := init",
            "theorem t : True := by\n  exact (fun init => init) trivial",
            "@[simp] theorem init_eq : True := trivial",
            "attribute [simp] Nat.add_zero",
        ] {
            assert!(p.scan(code, false).is_empty(), "{code}");
        }
        // A tactic macro is a macro, not a tactic attribute.
        assert!(
            p.scan("macro \"x\" : tactic => `(tactic| rfl)", true)
                .is_empty()
        );
    }

    #[test]
    fn simprocs_are_refused() {
        let p = policy();
        for word in [
            "simproc",
            "dsimproc",
            "simproc_decl",
            "dsimproc_decl",
            "builtin_simproc",
        ] {
            let code = format!("{word} reduceFoo (foo _) := fun _ => pure .continue");
            assert_eq!(p.scan(&code, true), [format!("forbidden: {word}")]);
        }
    }

    #[test]
    fn literals_and_quoted_names_cant_hide_code() {
        let p = policy();
        for code in [
            "def c : Char := '\"'\n#eval 1\ndef d := \"\"",
            "def c : Char := '\\''\ndef s := \"x\"\n#eval 1",
            "def c : Char := '\\u{22}'\n#eval 1",
            "def «a\"» := 1\n#eval 1\ndef b := \"\"",
            "def «x/-» : Nat := 1\n#eval 1\n-- -/",
            "def s := r#\"a\"b\"#\n#eval 1",
            "def s := r\"a\\\"\n#eval 1",
            "def s : String := s!\"{\"\\\"\"}\"\n#eval 1\ndef z := \"x\"",
        ] {
            assert_eq!(p.scan(code, false), ["forbidden: #eval"], "{code}");
        }
        // Names with primes, and strings, are still read as before.
        assert!(
            p.scan("theorem h' : True := trivial\ndef s := \"#eval\"", false)
                .is_empty()
        );
    }

    #[test]
    fn tokens_glued_to_a_name_are_still_seen() {
        let p = policy();
        for (code, reason) in [
            ("def a := x#eval IO.println \"hi\"", "forbidden: #eval"),
            ("def z := 1#eval 1", "forbidden: #eval"),
            ("def y := «x»#eval 1", "forbidden: #eval"),
            ("def y := «x»unsafe def f := 1", "forbidden: unsafe"),
            ("def f := 1\n«x»attribute [init] f", "forbidden: init"),
            (
                "«x»set_option debug.skipKernelTC true in\ntheorem t : True := trivial",
                "option: debug.skipKernelTC",
            ),
        ] {
            assert_eq!(p.scan(code, false), [reason], "{code}");
        }
        // A name in «» goes on through a dot.
        let texts: Vec<String> = tokens("«x».y z").into_iter().map(|t| t.text).collect();
        assert_eq!(texts, ["x.y", "z"]);
        assert!(p.scan("def «a b».c := 1\n#check «a b».c", false).is_empty());
    }

    #[test]
    fn deeply_nested_interpolation_doesnt_overflow_the_stack() {
        let depth = 20_000;
        let code = format!("def s := {}{}", "s!\"{".repeat(depth), "}\"".repeat(depth));
        // The worker's threads have tokio's 2 MiB stacks.
        let scanned = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || policy().scan(&code, false))
            .unwrap()
            .join()
            .unwrap();
        assert!(scanned.is_empty());
    }

    #[test]
    fn snapshots_allow_the_macro_family_only() {
        let p = policy();
        let macros = "macro \"twice\" : term => `(2)\nmacro_rules | `(twice) => `(2)\n\
                      syntax \"x\" : term\ndeclare_syntax_cat foo";
        assert_eq!(
            p.scan(macros, false),
            [
                "forbidden: macro",
                "forbidden: macro_rules",
                "forbidden: syntax",
                "forbidden: declare_syntax_cat"
            ]
        );
        assert!(p.scan(macros, true).is_empty());
        // Code that runs at elaboration time stays forbidden.
        assert_eq!(
            p.scan("elab \"x\" : term => pure x", true),
            ["forbidden: elab"]
        );
        assert_eq!(p.scan("run_cmd pure ()", true), ["forbidden: run_cmd"]);
        assert_eq!(p.scan("#eval 1", true), ["forbidden: #eval"]);
    }

    #[test]
    fn reports_are_rooted_calls_of_the_library() {
        assert_eq!(AXIOM_REPORT, "run_cmd _root_.Pisa.reportNew");
        assert_eq!(
            module_report("PisaSnap.s0f"),
            "run_cmd _root_.Pisa.reportModule `PisaSnap.s0f"
        );
    }

    #[test]
    fn module_export_names_the_module_and_parses() {
        let export = module_export("PisaSnap.s0f");
        assert!(!export.contains("{module}"), "{export}");
        assert!(
            export.contains("env.getModuleIdx? `PisaSnap.s0f |"),
            "{export}"
        );
        let report: CommandResponse = serde_json::from_value(json!({
            "env": 1,
            "messages": [{"severity": "info", "pos": {"line": 1, "column": 0},
                "data": "PISA_EXPORT [{\"name\":\"b\",\"kind\":\"def\",\"signature\":\"b : Nat\",\"type_pp_full\":\"Nat\",\"type_refs\":[\"Nat\"],\"value_refs\":[],\"docstring\":\"B.\",\"deprecated_to\":null},{\"name\":\"a\",\"kind\":\"theorem\",\"signature\":\"a : True\",\"type_pp_full\":\"True\",\"type_refs\":[],\"value_refs\":[],\"docstring\":null,\"deprecated_to\":null}]"}]
        }))
        .unwrap();
        let decls = parse_export(&report).unwrap();
        let names: Vec<&str> = decls.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(decls[1].docstring.as_deref(), Some("B."));
    }

    #[test]
    fn macros_are_found_outside_comments_and_strings() {
        for code in [
            "macro \"x\" : term => `(1)",
            "macro_rules | `(x) => `(1)",
            "syntax \"x\" : term",
            "declare_syntax_cat foo",
            "elab \"x\" : term => pure x",
            "theorem t : True := trivial\nrun_cmd pure ()",
        ] {
            assert!(uses_macros(code), "{code}");
        }
        for code in [
            "theorem t : True := trivial -- macro\n/- syntax -/",
            "def s : String := \"macro\"",
            "theorem macro_free : True := trivial",
            "notation \"x\" => 1",
        ] {
            assert!(!uses_macros(code), "{code}");
        }
    }

    #[test]
    fn judge_rejects_disallowed_axioms_and_errors() {
        let p = policy();
        let report = |decls: serde_json::Value| {
            response(json!({"env": 2, "messages": [{"severity": "info",
                "pos": {"line": 1, "column": 0}, "data": format!("PISA_DECLS {decls}")}]}))
        };
        let ok = report(json!([{"name": "x", "kind": "theorem",
            "axioms": ["propext", "Quot.sound"], "assumes": [], "type": "1",
            "type_refs": [], "value_refs": []}]));
        let v = p.judge(&response(json!({"env": 1})), Some(&ok));
        assert!(v.ok, "{v:?}");
        assert_eq!(v.declarations[0].name, "x");

        let bad = report(json!([{"name": "x", "kind": "theorem",
            "axioms": ["sorryAx"], "assumes": [], "type": "1", "type_refs": [], "value_refs": []},
            {"name": "y", "kind": "theorem", "axioms": ["Lean.ofReduceBool"], "assumes": [],
             "type": "2", "type_refs": [], "value_refs": []}]));
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

    #[test]
    fn a_report_says_what_it_couldnt_describe_and_why_it_failed() {
        let p = policy();
        let message = |severity: &str, data: String| json!({"severity": severity, "pos": {"line": 1, "column": 0}, "data": data});
        let x = json!([{"name": "x", "kind": "theorem", "axioms": ["propext"], "assumes": [],
            "type": "1", "type_refs": [], "value_refs": []}]);
        let failed = json!([{"name": "y", "error": "describing y: a metavariable"}]);
        let report = response(json!({"env": 2, "messages": [
            message("info", format!("PISA_DECLS {x}")),
            message("info", format!("PISA_REPORT_FAILED {failed}"))]}));
        let v = p.judge(&response(json!({"env": 1})), Some(&report));
        assert_eq!(v.declarations[0].name, "x");
        assert_eq!(
            v.reasons,
            ["error: axiom report: describing y: a metavariable"]
        );
        assert!(!v.ok);
        // A module's report is all or nothing.
        assert_eq!(parse_report(&report), None);

        // One that failed outright says why.
        let broken = response(json!({"env": 2, "messages": [
            message("error", "unknown constant 'Pisa.reportNew'".into())]}));
        let v = p.judge(&response(json!({"env": 1})), Some(&broken));
        assert_eq!(
            v.reasons,
            ["error: axiom report unavailable: unknown constant 'Pisa.reportNew'"]
        );
    }

    #[test]
    fn a_report_without_refs_does_not_parse() {
        let report = |decl: &serde_json::Value| {
            response(json!({"env": 2, "messages": [{"severity": "info",
                "pos": {"line": 1, "column": 0}, "data": format!("PISA_DECLS {}", json!([decl]))}]}))
        };
        let decl = json!({"name": "x", "kind": "theorem", "axioms": [], "assumes": [],
            "type": "1", "value": null, "type_refs": [], "value_refs": [],
            "generated": false});
        assert!(parse_report(&report(&decl)).is_some());
        for field in ["type", "type_refs", "value_refs", "assumes"] {
            let mut without = decl.clone();
            without.as_object_mut().unwrap().remove(field);
            assert!(parse_report(&report(&without)).is_none(), "{field}");
        }
    }
}
