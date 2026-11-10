//! The axiom report and the kernel verifier on real Lean: code is compiled to
//! snapshot modules as a worker builds them, reported as the builder reports
//! them, and re-checked by `docker/verifier/PisaVerify.lean`. Each test needs
//! elan on `PATH` and the `Pisa` library built, and runs on the library's
//! toolchain, so they are ignored by default:
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
use pisa_protocol::verify::{Declaration, grouped, parent_in, statement_changes};
use pisa_worker::launcher::{Verified, compile_args};
use pisa_worker::policy::{
    DEFAULT_AXIOMS, DEFAULT_OPTIONS, Policy, header_command, module_export, module_report,
    parse_declarations, parse_export, parse_report,
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
    /// library, as the worker's REPL and verifier run. It runs on the
    /// library's toolchain, whatever elan's default, as the image builds both
    /// with one.
    fn lean(&self, args: &[&str], pisa: bool) -> std::process::Output {
        let mut path = vec![self.root.path().to_path_buf()];
        if pisa {
            path.push(docker().join("pisa-lean/.lake/build/lib/lean"));
        }
        let toolchain = std::fs::read_to_string(docker().join("pisa-lean/lean-toolchain")).unwrap();
        Command::new("lean")
            .args(args)
            .current_dir(self.root.path())
            .env("ELAN_TOOLCHAIN", toolchain.trim())
            .env("LEAN_PATH", std::env::join_paths(path).unwrap())
            .output()
            .expect("running lean (is elan on PATH?)")
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

    /// The search export of module `id`, run as the builder runs it.
    fn export(&self, code: &str, id: &str) -> Vec<pisa_worker::catalog::SnapshotDecl> {
        let header = snapshot::next_header(&split_header(code).header, id);
        let text = format!(
            "{}\n{}\n",
            header_command(&header),
            module_export(&snapshot::module(id))
        );
        let file = self.root.path().join(format!("export_{id}.lean"));
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
        parse_export(&response).unwrap_or_else(|| {
            panic!(
                "no export for s{id}: {:?}\n{}",
                response.messages,
                String::from_utf8_lossy(&out.stderr)
            )
        })
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
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn the_same_text_in_another_module_keeps_every_statement() {
    let moved = format!("-- another module\n{SPEC}");
    assert_eq!(changes(SPEC, &moved), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_changed_type_is_refused() {
    let retyped = SPEC.replace("f n = n + 1 :=", "f n = n + 2 :=");
    assert_eq!(
        changes(SPEC, &retyped),
        ["statement: t has a different type"]
    );
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
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
#[ignore = "needs elan Lean 4.36.0-rc2"]
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
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_binder_made_implicit_is_refused() {
    let explicit = "theorem u (n : Nat) (h : 0 < n) : n ≠ 0 := sorry\n";
    let implicit = "theorem u {n : Nat} {h : 0 < n} : n ≠ 0 := sorry\n";
    assert_eq!(
        changes(explicit, implicit),
        ["statement: u has a different type"]
    );
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
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

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn an_untouched_stubbed_definition_keeps_its_value() {
    // Lean labels each `sorry` with where it was written.
    let spec = "def H : Nat := sorry\ntheorem t : H = H := sorry\n";
    let proved = "def H : Nat := sorry\n\ntheorem t : H = H := rfl\n";
    assert_eq!(changes(spec, proved), Vec::<String>::new());
    let filled = "def H : Nat := 0\ntheorem t : H = H := rfl\n";
    assert_eq!(
        changes(spec, filled),
        ["statement: H has a different value"]
    );
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn what_lean_makes_on_demand_for_a_matcher_neednt_stay() {
    // `simp [g]` has Lean make `g`'s and its matcher's equations.
    let spec = "private def g : Nat → Nat\n  | 0 => 0\n  | n + 1 => n\n\
                theorem t (n : Nat) : g (n + 1) = n := by simp [g]\n";
    let proved = "private def g : Nat → Nat\n  | 0 => 0\n  | n + 1 => n\n\
                  theorem t (n : Nat) : g (n + 1) = n := rfl\n";
    assert_eq!(changes(spec, proved), Vec::<String>::new());
}

/// Notation and syntax of every kind: Lean makes a parser for each, which is
/// a definition by type alone (code may take its name), and macros.
const NOTATION: &str = "local notation \"d\" => (2 : Nat)
notation \"e\" => Nat.succ 1
infixl:65 \" +!+ \" => Nat.add
syntax \"zz\" : term
macro_rules | `(zz) => `(3)
macro \"mm\" : term => `(4)
declare_syntax_cat foo
theorem t : d + e = 4 := sorry
theorem u : 1 +!+ zz = mm := sorry
";

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn notation_reads_the_same_in_another_module() {
    // A local notation's parser names its syntax kind by the module.
    let moved = format!("-- another module\n{NOTATION}");
    assert_eq!(changes(NOTATION, &moved), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn notation_dropped_or_reworded_keeps_every_statement() {
    let proved = "local notation \"two\" => (2 : Nat)
infixl:65 \" ⊕⊕ \" => Nat.add
theorem t : two + Nat.succ 1 = 4 := rfl
theorem u : 1 ⊕⊕ 3 = 4 := rfl
";
    assert_eq!(changes(NOTATION, proved), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn what_lean_makes_on_demand_in_module_system_code_neednt_stay() {
    // There Lean makes the equations of the public `f`, whose body isn't
    // exposed, as private names: `rw [f]` makes `_private.….f.eq_1` and
    // `eq_2`, `rw [f.eq_def]` makes `eq_def` instead.
    let f = "module\n\npublic section\n\ndef f : Nat → Nat\n  | 0 => 0\n  | n + 1 => f n + 1\n";
    let spec = format!("{f}theorem t (n : Nat) : f (n + 1) = f n + 1 := by rw [f]\n");
    let proved = format!("{f}theorem t (n : Nat) : f (n + 1) = f n + 1 := by rw [f.eq_def]\n");
    assert_eq!(changes(&spec, &proved), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_helper_prepended_keeps_every_statement() {
    // The helper's matcher is cached and reused for `f`, which then names
    // `helper.match_1` where it named `f.match_1`.
    let spec = "def f : Nat → Nat\n  | 0 => 0\n  | n + 1 => n\n";
    let helped = format!("def helper : Nat → Nat\n  | 0 => 0\n  | n + 1 => n\n{spec}");
    assert_eq!(changes(spec, &helped), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_proof_by_match_replaces_another_proof() {
    let by_match =
        "theorem t (a b : Prop) (h : a ∧ b) : b ∧ a :=\n  match h with\n  | ⟨x, y⟩ => ⟨y, x⟩\n";
    let direct = "theorem t (a b : Prop) (h : a ∧ b) : b ∧ a := ⟨h.2, h.1⟩\n";
    assert_eq!(changes(by_match, direct), Vec::<String>::new());
    // Its matcher is no declaration of its own.
    let snapshots = Snapshots::new();
    let names: Vec<String> = snapshots
        .build(by_match)
        .agreed()
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert_eq!(names, ["t"]);
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_private_well_founded_body_cannot_change() {
    // Mutually recursive, the bodies are in the private auxiliary
    // `ev._mutual`, which `ev` and `od` use.
    let defs = |base: &str| {
        format!(
            "mutual\n\
             private def ev (n : Nat) : Bool := if h : n = 0 then true else od (n - 1)\n\
             termination_by n\n\
             private def od (n : Nat) : Bool := if h : n = 0 then {base} else ev (n - 1)\n\
             termination_by n\n\
             end\n"
        )
    };
    let snapshots = Snapshots::new();
    let spec = snapshots.build(&defs("false"));
    let changed = snapshots.build(&defs("true"));
    let changes: Vec<String> = statement_changes(spec.agreed(), changed.agreed())
        .iter()
        .map(ToString::to_string)
        .collect();
    let private = |name: &str| format!("_private.{}.0.{name}", snapshot::module(&spec.id));
    assert_eq!(
        changes,
        [
            format!("statement: {} has a different value", private("ev")),
            format!("statement: {} has a different value", private("od")),
        ]
    );
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn an_instance_field_proved_by_a_lemma_keeps_its_value() {
    let class = "class C (n : Nat) : Prop where h : n + 0 = n\n";
    let stub = format!("{class}instance : C 1 := ⟨sorry⟩\n");
    let proved = format!("{class}instance : C 1 := ⟨Nat.add_zero 1⟩\n");
    assert_eq!(changes(&stub, &proved), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_field_with_a_parameter_proved_by_a_lemma_keeps_its_value() {
    let structure = "structure S (n : Nat) where\n  x : Nat\n  h : n + 0 = n\n";
    let stub = format!("{structure}def s (n : Nat) : S n := ⟨n, sorry⟩\n");
    let proved = format!("{structure}def s (n : Nat) : S n := ⟨n, Nat.add_zero n⟩\n");
    assert_eq!(changes(&stub, &proved), Vec::<String>::new());
    // Data still counts.
    let other = format!("{structure}def s (n : Nat) : S n := ⟨n + 1, Nat.add_zero n⟩\n");
    assert_eq!(
        changes(&stub, &other),
        ["statement: s has a different value"]
    );
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn an_enumeration_reads_the_same_in_another_module() {
    // `noConfusionType`'s universe is a hygienic name with the module in it.
    let spec =
        "inductive Color\n  | red\n  | green\ntheorem t : Color.red ≠ Color.green := sorry\n";
    let moved = format!("-- another module\n{spec}");
    assert_eq!(changes(spec, &moved), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_private_structure_reads_the_same_in_another_module() {
    // Its projections are `fun self => self.1` on the private structure.
    let spec = "private structure P where\n  x : Nat\ntheorem t (p : P) : p.x = p.x := sorry\n";
    let moved = format!("-- another module\n{spec}");
    assert_eq!(changes(spec, &moved), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_renamed_universe_variable_keeps_the_statement() {
    let u =
        "universe u\ndef idf (α : Sort u) (a : α) : α := a\ntheorem t : idf Nat 1 = 1 := sorry\n";
    let v = u.replace('u', "v").replace("vniverse", "universe");
    assert_eq!(changes(u, &v), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_proof_refers_to_what_it_proves_not_to_the_lemma() {
    let snapshots = Snapshots::new();
    let lemmas = snapshots.build(
        "structure S (n : Nat) where\n  x : Nat\n  h : n + 0 = n\n\
         theorem lem (n : Nat) : n + 0 = n := Nat.add_zero n\n",
    );
    let code = format!(
        "import {}\ndef s (n : Nat) : S n := ⟨n, lem n⟩\n",
        snapshot::module(&lemmas.id)
    );
    let built = snapshots.build(&code);
    let s = built.agreed().iter().find(|d| d.name == "s").unwrap();
    assert_eq!(s.value_refs, ["S.mk"]);
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_universe_polymorphic_proof_in_a_definition_keeps_its_value() {
    let stub = "def c.{u} (α : Sort u) (x : α) : {y : α // y = x} := ⟨x, sorry⟩\n";
    let proved = "def c.{u} (α : Sort u) (x : α) : {y : α // y = x} := ⟨x, rfl⟩\n";
    assert_eq!(changes(stub, proved), Vec::<String>::new());
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn what_code_declares_isnt_what_lean_makes_from_a_type() {
    let snapshots = Snapshots::new();
    // Named as Lean names what it makes, or shaped like a projection, but
    // written by the code.
    let built = snapshots.build(
        "structure S where\n  x : Nat\n\
         def S.below : Nat := 5\n\
         def S.foo (s : S) : Nat := s.1\n\
         theorem S.mk.inj' : True := trivial\n",
    );
    let parent = |name: &str| {
        let d = built.agreed().iter().find(|d| d.name == name).unwrap();
        d.parent.clone()
    };
    for own in ["S.below", "S.foo", "S.mk.inj'"] {
        assert_eq!(parent(own), None, "{own}");
    }
    for made in ["S.mk", "S.x", "S.rec", "S.casesOn", "S.mk.injEq"] {
        assert_eq!(parent(made).as_deref(), Some("S"), "{made}");
    }
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_structure_with_another_field_is_one_change() {
    let one = "structure S where\n  x : Nat\ntheorem t (s : S) : s.x = s.x := rfl\n";
    let two = "structure S where\n  x : Nat\n  y : Nat\ntheorem t (s : S) : s.x = s.x := rfl\n";
    let snapshots = Snapshots::new();
    let (one, two) = (snapshots.build(one), snapshots.build(two));
    let parents: Vec<(&str, Option<&str>)> = one
        .agreed()
        .iter()
        .filter(|d| d.name.starts_with("S."))
        .map(|d| (d.name.as_str(), d.parent.as_deref()))
        .collect();
    for (name, parent) in &parents {
        assert_eq!(*parent, Some("S"), "{name}: {parents:?}");
    }
    let changes = grouped(statement_changes(one.agreed(), two.agreed()), |c| {
        parent_in(one.agreed(), &c.name)
    });
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].name, "S");
    for derived in ["S.mk", "S.rec"] {
        assert!(
            changes[0].includes.iter().any(|n| n == derived),
            "{changes:?}"
        );
    }
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn an_enumeration_with_another_constructor_is_one_change() {
    // Lean 4.36 builds an enumeration's `ctorElimType` from its
    // constructors, its `noConfusion` from `ctorIdx`, and an `elim` for each
    // constructor: all of them are the type's.
    let two = "inductive Gate\n  | h\n  | cnot\ntheorem t : Gate.h ≠ Gate.cnot := sorry\n";
    let three =
        "inductive Gate\n  | h\n  | cnot\n  | magic\ntheorem t : Gate.h ≠ Gate.cnot := sorry\n";
    let snapshots = Snapshots::new();
    let (two, three) = (snapshots.build(two), snapshots.build(three));
    for built in [&two, &three] {
        for d in built
            .agreed()
            .iter()
            .filter(|d| d.name.starts_with("Gate."))
        {
            assert!(
                d.parent.as_deref() == Some("Gate") || d.generated,
                "{} has no parent: {:?}",
                d.name,
                built.agreed()
            );
        }
    }
    let changes = grouped(statement_changes(two.agreed(), three.agreed()), |c| {
        parent_in(two.agreed(), &c.name)
    });
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].name, "Gate");
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn the_export_prints_a_definitions_value_without_notation() {
    let s = Snapshots::new();
    let code = "def double (n : Nat) : Nat := 2 * n\n\nnotation \"twice\" => double\n\n\
                def four : Nat := twice 2\n\ntheorem t : four = 4 := rfl\n";
    let built = s.build(code);
    let decls = s.export(code, &built.id);
    let value = |name: &str| {
        let d = decls.iter().find(|d| d.name == name);
        d.unwrap_or_else(|| panic!("no {name} in {decls:?}"))
            .value
            .clone()
    };
    // Notation can't make a value read as something else.
    assert_eq!(value("four").as_deref(), Some("double 2"));
    let double = value("double").unwrap();
    assert!(
        double.contains("HMul.hMul") && !double.contains('*'),
        "{double}"
    );
    // A proof isn't printed.
    assert_eq!(value("t"), None);
}
