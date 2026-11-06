//! The verification policy: what submitted code may contain, and which
//! axioms its declarations may depend on.
//!
//! The textual checks run before any code does. They block metaprogramming,
//! which could otherwise rewrite the environment or fake the axiom report, and
//! options that weaken checking, such as `debug.skipKernelTC`.

use pisa_protocol::header::import_parts;
use pisa_protocol::repl::{CommandResponse, Severity};
use pisa_protocol::verify::{Declaration, ProofStatus, Verification};

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

/// Pure syntax transformers: they can't run code, do IO or add declarations,
/// so their only risk is what they expand into. Snapshots allow them because
/// code that uses them is re-checked through the kernel unless the request
/// opts out, and is then marked as not verified; other endpoints don't.
const MACRO_FAMILY: &[&str] = &["macro", "macro_rules", "syntax", "declare_syntax_cat"];

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
/// on, a hash of its type and, for definitions, of its value, and the
/// constants of this code and of snapshots that its type and value use
/// (`type_refs`, `value_refs`; an inductive's "value" is its constructors and
/// recursor). Names are rooted so user declarations can't shadow them.
///
/// Internal auxiliaries, such as the `f._unary` holding the body of a
/// well-founded definition, aren't listed: what uses one has its hashes fold
/// in theirs, and its refs go through to what they use. Private names are
/// hashed without their module, which differs between a module and its
/// replacement. `generated` marks what Lean makes on demand, such as equation
/// lemmas, which a replacement needn't have. The kernel verifier
/// (`docker/verifier/PisaVerify.lean`) computes the same.
pub const AXIOM_REPORT: &str = r#"run_cmd do
  let env : _root_.Lean.Environment ← _root_.Lean.MonadEnv.getEnv
  let tracked : _root_.Lean.Name → Bool := fun m =>
    match env.getModuleIdxFor? m with
    | some i => (`PisaSnap).isPrefixOf (env.header.moduleNames[i.toNat]!)
    | none => true
  let hidden : _root_.Lean.Name → Bool := fun m => m.isInternal && !(_root_.Lean.isPrivateName m)
  let norm : _root_.Lean.Name → _root_.Lean.Name := fun m =>
    match _root_.Lean.privateToUserName? m with
    | some u => `_private ++ u
    | none => m
  let hashOf : _root_.Lean.Expr → UInt64 := fun e =>
    hash (e.replace fun x => match x with
      | .const m ls => if _root_.Lean.isPrivateName m then some (.const (norm m) ls) else none
      | _ => none)
  let deep : _root_.Lean.Name → _root_.Lean.Expr → UInt64 × List _root_.Lean.Name := fun self e => Id.run do
    let mut seen : _root_.Lean.NameSet := {}
    let mut refs : _root_.Lean.NameSet := {}
    let mut reached : Array _root_.Lean.Name := #[]
    let mut todo : List _root_.Lean.Name := e.getUsedConstants.toList
    repeat
      match todo with
      | [] => break
      | m :: rest =>
        todo := rest
        if seen.contains m || !tracked m then continue
        seen := seen.insert m
        if hidden m then
          reached := reached.push m
          if let some ci := env.find? m then
            todo := ci.type.getUsedConstants.toList ++ todo
            if let .defnInfo d := ci then
              todo := d.value.getUsedConstants.toList ++ todo
        else if m != self then
          refs := refs.insert m
    let mut h := hashOf e
    for m in reached.qsort (fun a b => a.toString < b.toString) do
      if let some ci := env.find? m then
        h := mixHash h (mixHash (hash (norm m)) (hashOf ci.type))
        if let .defnInfo d := ci then
          h := mixHash h (hashOf d.value)
    return (h, refs.toList)
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
    let assumes := axioms.filter fun a => tracked a && !hidden a &&
      (match env.find? a with | some (.axiomInfo _) => true | _ => false)
    let (typeHash, typeRefs) := deep n ci.type
    let (valueHash, valueRefs) : _root_.Lean.Json × List _root_.Lean.Name := match ci with
      | .defnInfo d => let (h, refs) := deep n d.value; (_root_.Lean.ToJson.toJson (toString h), refs)
      | .inductInfo i => (_root_.Lean.Json.null, i.ctors ++ [_root_.Lean.mkRecName i.name])
      | _ => (_root_.Lean.Json.null, [])
    let generated := _root_.Lean.isReservedName env n ||
      ((_root_.Lean.privateToUserName? n).map (·.isInternal)).getD false
    out := out.push (_root_.Lean.Json.mkObj [
      ("name", _root_.Lean.ToJson.toJson n),
      ("kind", _root_.Lean.ToJson.toJson kind),
      ("axioms", _root_.Lean.ToJson.toJson axioms),
      ("assumes", _root_.Lean.ToJson.toJson assumes),
      ("type_hash", _root_.Lean.ToJson.toJson (toString typeHash)),
      ("value_hash", valueHash),
      ("type_refs", _root_.Lean.ToJson.toJson typeRefs),
      ("value_refs", _root_.Lean.ToJson.toJson valueRefs),
      ("generated", _root_.Lean.ToJson.toJson generated)])
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
    tokens(body)
        .iter()
        .any(|t| FORBIDDEN.contains(&t.as_str()) || MACRO_FAMILY.contains(&t.as_str()))
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
        let tokens = tokens(body);
        let mut reasons = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            let t_str = t.as_str();
            let forbidden =
                FORBIDDEN.contains(&t_str) || (!allow_macros && MACRO_FAMILY.contains(&t_str));
            if !self.allow_metaprogramming && forbidden {
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
        let decls = report.and_then(parse_report);
        self.judge_decls(response.has_errors(), decls)
    }

    /// Judges from whether the body had errors and the declarations the axiom
    /// report found (from the REPL report, or a snapshot's kernel re-check).
    pub fn judge_decls(
        &self,
        errors: bool,
        declarations: Option<Vec<Declaration>>,
    ) -> Verification {
        let mut v = Verification {
            ok: true,
            reasons: Vec::new(),
            declarations: Vec::new(),
        };
        if errors {
            v.reasons.push("error: Lean reported errors".into());
        }
        match declarations {
            Some(decls) => v.declarations = decls,
            None if errors => {}
            None => v.reasons.push("error: axiom report unavailable".into()),
        }
        for d in &mut v.declarations {
            d.status = Some(self.status(d));
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
    pub fn status(&self, d: &Declaration) -> ProofStatus {
        let assumes = d.assumes.as_deref().unwrap_or_default();
        let each = d
            .axioms
            .iter()
            .filter(|a| !self.allowed_axioms.contains(a))
            .map(|a| {
                if assumes.contains(a) {
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
    fn a_status_tells_assumptions_from_sorry_and_the_rest() {
        let p = policy();
        let decl = |axioms: &[&str], assumes: Option<&[&str]>| Declaration {
            name: "t".into(),
            axioms: axioms.iter().map(|a| a.to_string()).collect(),
            assumes: assumes.map(|a| a.iter().map(|a| a.to_string()).collect()),
            ..Default::default()
        };
        let none: Option<&[&str]> = Some(&[]);
        for (axioms, assumes, status) in [
            (&["propext"][..], none, ProofStatus::Proved),
            (
                &["propext", "kunneth"],
                Some(&["kunneth"][..]),
                ProofStatus::Conditional,
            ),
            (&["sorryAx"], none, ProofStatus::Sorry),
            (
                &["sorryAx", "kunneth"],
                Some(&["kunneth"]),
                ProofStatus::Sorry,
            ),
            (
                &["t._native.native_decide.ax_1_1"],
                none,
                ProofStatus::Rejected,
            ),
            (
                &["sorryAx", "Lean.trustCompiler", "kunneth"],
                Some(&["kunneth"]),
                ProofStatus::Rejected,
            ),
            // A record from before assumptions were reported.
            (&["kunneth"], None, ProofStatus::Rejected),
        ] {
            assert_eq!(p.status(&decl(axioms, assumes)), status, "{axioms:?}");
        }
        assert!(ProofStatus::Rejected > ProofStatus::Sorry);
        assert!(ProofStatus::Sorry > ProofStatus::Conditional);
        assert!(ProofStatus::Conditional > ProofStatus::Proved);
        // Judging sets each declaration's status, and `ok` stays as it was.
        let v = p.judge_decls(false, Some(vec![decl(&["kunneth"], Some(&["kunneth"]))]));
        assert_eq!(v.declarations[0].status, Some(ProofStatus::Conditional));
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
