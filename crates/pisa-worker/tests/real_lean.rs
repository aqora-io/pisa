//! The axiom report and the kernel verifier on real Lean: code is compiled to
//! snapshot modules as a worker builds them, reported as the builder reports
//! them, and re-checked by `docker/verifier/PisaVerify.lean`. Each test needs
//! elan's Lean 4.35.0 on `PATH` and the `Pisa` library built, so they are
//! ignored by default:
//!
//! ```text
//! (cd docker/pisa-lean && lake build)
//! cargo test -p pisa-worker --test real_lean -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use pisa_protocol::header::split_header;
use pisa_protocol::repl::{CommandResponse, Message};
use pisa_protocol::snapshot;
use pisa_protocol::verify::{Declaration, statement_changes};
use pisa_worker::launcher::{Verified, compile_args};
use pisa_worker::policy::{
    DEFAULT_AXIOMS, DEFAULT_OPTIONS, Policy, header_command, module_report, parse_declarations,
    parse_report,
};

fn docker() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker")
}

fn policy() -> Policy {
    Policy {
        allowed_axioms: DEFAULT_AXIOMS.split(',').map(String::from).collect(),
        allowed_options: DEFAULT_OPTIONS.split(',').map(String::from).collect(),
        allow_metaprogramming: false,
    }
}

/// A root of snapshot modules, as a worker's cache holds them.
struct Snapshots {
    root: tempfile::TempDir,
}

/// A built module: its id, whether it is module-system code, and its
/// declarations as the builder's report and the verifier give them.
struct Built {
    id: String,
    module_system: bool,
    reported: Vec<Declaration>,
    verified: Vec<Declaration>,
}

impl Snapshots {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(snapshot::ROOT)).unwrap();
        Snapshots { root }
    }

    /// Runs `lean` on the snapshot modules, and with `pisa` on the `Pisa`
    /// library, as the worker's REPL and verifier run.
    fn lean(&self, args: &[&str], pisa: bool) -> std::process::Output {
        let mut path = vec![self.root.path().to_path_buf()];
        if pisa {
            path.push(docker().join("pisa-lean/.lake/build/lib/lean"));
        }
        Command::new("lean")
            .args(args)
            .current_dir(self.root.path())
            .env("LEAN_PATH", std::env::join_paths(path).unwrap())
            .output()
            .expect("running lean (is elan's Lean 4.35.0 on PATH?)")
    }

    /// Compiles `code` to its snapshot module, then reports and re-checks it.
    fn build(&self, code: &str) -> Built {
        let id = snapshot::id(code);
        let file = self
            .root
            .path()
            .join(snapshot::ROOT)
            .join(format!("s{id}.lean"));
        std::fs::write(&file, code).unwrap();
        let args = compile_args(self.root.path(), &file, &file.with_extension("olean"));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.lean(&args, false);
        assert!(
            out.status.success(),
            "lean -o failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Built {
            module_system: split_header(code).header.lines().next() == Some("module"),
            reported: self.report(code, &id),
            verified: self.verify(&id),
            id,
        }
    }

    /// The per-module report, in a file headed as the builder's warm process
    /// is: the code's header with its snapshots replaced by the module.
    fn report(&self, code: &str, id: &str) -> Vec<Declaration> {
        let header = snapshot::next_header(&split_header(code).header, id);
        let text = format!(
            "{}\n{}\n",
            header_command(&header),
            module_report(&snapshot::module(id))
        );
        let file = self.root.path().join(format!("report_{id}.lean"));
        std::fs::write(&file, text).unwrap();
        let out = self.lean(&["--json", &file.to_string_lossy()], true);
        let messages: Vec<Message> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let response = CommandResponse {
            env: 0,
            messages,
            sorries: Vec::new(),
            tactics: Vec::new(),
            at: Vec::new(),
            infotree: None,
        };
        let decls = parse_report(&response).unwrap_or_else(|| {
            panic!(
                "no axiom report for s{id}: {:?}\n{}",
                response.messages,
                String::from_utf8_lossy(&out.stderr)
            )
        });
        policy().judge_decls(false, Some(decls)).declarations
    }

    /// The kernel verifier's declarations.
    fn verify(&self, id: &str) -> Vec<Declaration> {
        let verifier = docker().join("verifier/PisaVerify.lean");
        let out = self.lean(
            &["--run", &verifier.to_string_lossy(), &snapshot::module(id)],
            true,
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find_map(|l| l.strip_prefix("PISA_VERIFY "))
            .unwrap_or_else(|| {
                panic!(
                    "no verdict for s{id}:\n{stdout}{}",
                    String::from_utf8_lossy(&out.stderr)
                )
            });
        let verified: Verified = serde_json::from_str(line).unwrap();
        assert!(verified.ok, "s{id} rejected: {:?}", verified.reason);
        let decls = parse_declarations(verified.declarations).unwrap();
        policy().judge_decls(false, Some(decls)).declarations
    }
}

impl Built {
    /// The declarations the worker records: the verifier's for module-system
    /// code, which the builder sees only as importers do (it is always
    /// re-checked), and otherwise the report's, which must agree with the
    /// verifier's.
    fn agreed(&self) -> &[Declaration] {
        if self.module_system {
            return &self.verified;
        }
        let sorted = |decls: &[Declaration]| -> Vec<Declaration> {
            let mut decls = decls.to_vec();
            decls.sort_by(|a, b| a.name.cmp(&b.name));
            decls
        };
        assert_eq!(
            sorted(&self.reported),
            sorted(&self.verified),
            "the report and the verifier disagree on s{}",
            self.id
        );
        &self.reported
    }
}

/// The reasons a replacement of `old` by `new` is refused for.
fn changes(old: &str, new: &str) -> Vec<String> {
    let snapshots = Snapshots::new();
    let old = snapshots.build(old);
    let new = snapshots.build(new);
    statement_changes(old.agreed(), new.agreed())
        .iter()
        .map(ToString::to_string)
        .collect()
}

const SPEC: &str = "def f (n : Nat) : Nat := n + 1
private def g : Nat := 2
theorem t (n : Nat) : f n = n + 1 := sorry
theorem u : g = 2 := sorry
";

#[test]
#[ignore = "needs elan Lean 4.35.0"]
fn the_same_text_in_another_module_keeps_every_statement() {
    let moved = format!("-- another module\n{SPEC}");
    assert_eq!(changes(SPEC, &moved), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.35.0"]
fn a_changed_type_is_refused() {
    let retyped = SPEC.replace("f n = n + 1 :=", "f n = n + 2 :=");
    assert_eq!(
        changes(SPEC, &retyped),
        ["statement: t has a different type"]
    );
}

#[test]
#[ignore = "needs elan Lean 4.35.0"]
fn only_what_lean_makes_counts_as_generated() {
    let snapshots = Snapshots::new();
    // Lean lets code declare `bar.induct` for a definition that doesn't
    // recurse: it is the code's own, a target like any.
    let own = snapshots.build(
        "def bar : Nat := 1\ntheorem bar.induct : True := trivial\n\
         def f : Nat → Nat\n  | 0 => 0\n  | n + 1 => f n\n\
         theorem uses : True := by\n  have := @f.induct\n  trivial\n",
    );
    let generated = |name: &str| {
        let decls = own.agreed();
        let d = decls.iter().find(|d| d.name == name);
        d.unwrap_or_else(|| panic!("no {name}: {decls:?}"))
            .generated
    };
    assert!(!generated("bar.induct"));
    // Lean's own, made on demand for a recursive definition.
    assert!(generated("f.induct"));
}

#[test]
#[ignore = "needs elan Lean 4.35.0"]
fn names_that_read_alike_are_told_apart() {
    // `x.y` has two components, `«x.y»` one: different constants.
    let base = "def x.y : Nat := 1\ndef «x.y» : Nat := 1\n";
    let spec = format!("{base}theorem t : x.y = 1 := sorry\n");
    let swapped = format!("{base}theorem t : «x.y» = 1 := sorry\n");
    assert_eq!(
        changes(&spec, &swapped),
        ["statement: t has a different type"]
    );
}

#[test]
#[ignore = "needs elan Lean 4.35.0"]
fn a_binder_made_implicit_is_refused() {
    let explicit = "theorem u (n : Nat) (h : 0 < n) : n ≠ 0 := sorry\n";
    let implicit = "theorem u {n : Nat} {h : 0 < n} : n ≠ 0 := sorry\n";
    assert_eq!(
        changes(explicit, implicit),
        ["statement: u has a different type"]
    );
}

#[test]
#[ignore = "needs elan Lean 4.35.0"]
fn an_imported_private_cannot_stand_in_for_the_specs() {
    // With `import all`, `h` in the head's `t` is the other module's private
    // `h` (1), not the head's own (0), which it declares after `t`.
    let snapshots = Snapshots::new();
    let other = snapshots.build("module\nprivate def h : Nat := 1\n");
    let spec = snapshots.build("module\nprivate def h : Nat := 0\ntheorem t : h = 1 := sorry\n");
    let head = snapshots.build(&format!(
        "module\nimport all {}\ntheorem t : h = 1 := rfl\nprivate def h : Nat := 0\n",
        snapshot::module(&other.id)
    ));
    let changes: Vec<String> = statement_changes(spec.agreed(), head.agreed())
        .iter()
        .map(ToString::to_string)
        .collect();
    let t = format!("_private.{}.0.t", snapshot::module(&spec.id));
    assert_eq!(changes, [format!("statement: {t} has a different type")]);
}
