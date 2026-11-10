//! The axiom report and the kernel verifier on real Lean: code is compiled to
//! snapshot modules as a worker builds them, reported as the builder reports
//! them, and re-checked by `docker/verifier/PisaVerify.lean`. Each test needs
//! elan's Lean 4.35.0 on `PATH`, so they are ignored by default:
//!
//! ```text
//! cargo test -p pisa-worker --test real_lean -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use pisa_protocol::header::split_header;
use pisa_protocol::repl::{CommandResponse, Message};
use pisa_protocol::snapshot;
use pisa_protocol::verify::{Declaration, ReportedDeclaration, statement_changes};
use pisa_worker::launcher::{Verified, compile_args};
use pisa_worker::policy::{
    DEFAULT_AXIOMS, DEFAULT_OPTIONS, Policy, header_command, module_report, parse_report,
};

fn verifier() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/verifier/PisaVerify.lean")
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

/// A built module: its id, and its declarations as the builder's report and
/// the verifier give them.
struct Built {
    id: String,
    reported: Vec<Declaration>,
    verified: Vec<Declaration>,
}

impl Snapshots {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(snapshot::ROOT)).unwrap();
        Snapshots { root }
    }

    fn lean(&self, args: &[&str]) -> std::process::Output {
        Command::new("lean")
            .args(args)
            .current_dir(self.root.path())
            .env("LEAN_PATH", self.root.path())
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
        let out = self.lean(&args);
        assert!(
            out.status.success(),
            "lean -o failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Built {
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
        let out = self.lean(&["--json", &file.to_string_lossy()]);
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
        let verifier = verifier();
        let out = self.lean(&["--run", &verifier.to_string_lossy(), &snapshot::module(id)]);
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
        let mut decls: Vec<ReportedDeclaration> =
            serde_json::from_value(verified.declarations).unwrap();
        decls.sort_by(|a, b| a.name.cmp(&b.name));
        policy().judge_decls(false, Some(decls)).declarations
    }
}

impl Built {
    /// Asserts that the report and the verifier agree, apart from what only
    /// the report can tell (`generated`), and returns the declarations.
    fn agreed(&self) -> &[Declaration] {
        let plain = |decls: &[Declaration]| -> Vec<Declaration> {
            let mut decls: Vec<Declaration> = decls
                .iter()
                .map(|d| Declaration {
                    generated: false,
                    ..d.clone()
                })
                .collect();
            decls.sort_by(|a, b| a.name.cmp(&b.name));
            decls
        };
        assert_eq!(
            plain(&self.reported),
            plain(&self.verified),
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
