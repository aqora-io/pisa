//! The axiom report and the kernel verifier on real Lean: code is compiled to
//! snapshot modules, and described, by `pisa build` (`docker/pisa-lean`) as
//! a worker builds them, and re-checked by `pisa verify`. Each test needs
//! elan on `PATH` and `docker/pisa-lean` built, and runs on the library's
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
use pisa_worker::catalog::SnapshotDecl;
use pisa_worker::policy::{
    AXIOM_REPORT, DEFAULT_AXIOMS, DEFAULT_OPTIONS, Policy, header_command, parse_declarations,
    parse_export,
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

/// A built module: its id, whether it is module-system code, its
/// declarations as the build's report and the verifier give them, and its
/// export.
struct Built {
    id: String,
    module_system: bool,
    reported: Vec<Declaration>,
    verified: Vec<Declaration>,
    export: Vec<SnapshotDecl>,
}

impl Snapshots {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(snapshot::ROOT)).unwrap();
        Snapshots { root }
    }

    /// Runs `lean` on the snapshot modules, on the library's toolchain,
    /// whatever elan's default, as the image builds both with one.
    fn lean(&self, args: &[&str]) -> std::process::Output {
        let toolchain = std::fs::read_to_string(docker().join("pisa-lean/lean-toolchain")).unwrap();
        Command::new("lean")
            .args(args)
            .current_dir(self.root.path())
            .env("ELAN_TOOLCHAIN", toolchain.trim())
            .env("LEAN_PATH", self.root.path())
            .output()
            .expect("running lean (is elan on PATH?)")
    }

    /// [`lean`](Self::lean) with the `Pisa` library on the path too, for
    /// code that runs its report.
    fn lean_on_pisa(&self, args: &[&str]) -> std::process::Output {
        let toolchain = std::fs::read_to_string(docker().join("pisa-lean/lean-toolchain")).unwrap();
        let path = [
            self.root.path().to_path_buf(),
            docker().join("pisa-lean/.lake/build/lib/lean"),
        ];
        Command::new("lean")
            .args(args)
            .current_dir(self.root.path())
            .env("ELAN_TOOLCHAIN", toolchain.trim())
            .env("LEAN_PATH", std::env::join_paths(path).unwrap())
            .output()
            .expect("running lean (is elan on PATH?)")
    }

    /// Compiles `code` to its snapshot module with `lean -o`, as the worker
    /// once did.
    fn lean_build(&self, code: &str) -> String {
        let (file, olean) = self.file(code);
        let root = self.root.path().to_string_lossy().into_owned();
        let (file, olean) = (file.to_string_lossy(), olean.to_string_lossy());
        let out = self.lean(&[
            "-R",
            &root,
            "-o",
            &olean,
            "-DautoImplicit=false",
            "--json",
            &file,
        ]);
        assert!(
            out.status.success(),
            "lean -o failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        snapshot::id(code)
    }

    /// Compiles and describes `code` with `pisa build`, then re-checks it.
    fn build(&self, code: &str) -> Built {
        let id = snapshot::id(code);
        let module_system = split_header(code).header.lines().next() == Some("module");
        let built = self.tool_build(code);
        let reported = match module_system {
            true => Vec::new(),
            false => {
                let decls = parse_declarations(built.one("PISA_DECLS").clone()).unwrap();
                policy().judge_decls(false, Some(decls)).declarations
            }
        };
        Built {
            module_system,
            reported,
            verified: self.verify(&id),
            export: parse_export(built.one("PISA_EXPORT").clone()).unwrap(),
            id,
        }
    }

    /// The kernel verifier's declarations.
    fn verify(&self, id: &str) -> Vec<Declaration> {
        verdict_declarations(&self.tool_verify(&[id])[0])
    }
}

impl Built {
    /// The declarations the worker records: the verifier's for module-system
    /// code, which only the verifier describes (it is always re-checked), and
    /// otherwise the report's, which must agree with the verifier's.
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

/// The verdict on `body`, checked as `/check` checks it: under the header the
/// worker gives it, then the axiom report on what it declares.
fn checked(body: &str) -> pisa_protocol::verify::Verification {
    let snapshots = Snapshots::new();
    let file = snapshots.root.path().join("checked.lean");
    std::fs::write(
        &file,
        format!(
            "{}
{body}
{AXIOM_REPORT}
",
            header_command("")
        ),
    )
    .unwrap();
    let out = snapshots.lean_on_pisa(&["--json", &file.to_string_lossy()]);
    let messages: Vec<Message> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let (report, body): (Vec<Message>, Vec<Message>) = messages
        .into_iter()
        .partition(|m| m.data.starts_with("PISA_"));
    let response = |messages| CommandResponse {
        env: 0,
        messages,
        sorries: Vec::new(),
        tactics: Vec::new(),
        at: Vec::new(),
        infotree: None,
    };
    policy().judge(&response(body), Some(&response(report)))
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
fn a_broken_lemma_leaves_the_others_their_status() {
    let v = checked("theorem good : 1 = 1 := rfl\ntheorem bad : 1 = 2 := rfl\n");
    let status = |name: &str| {
        v.declarations
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.status)
    };
    use pisa_protocol::verify::ProofStatus;
    assert_eq!(
        (status("good"), status("bad")),
        (Some(ProofStatus::Proved), Some(ProofStatus::Sorry)),
        "{v:?}"
    );
    assert_eq!(
        v.reasons,
        ["error: Lean reported errors", "axiom: bad uses sorryAx"]
    );
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
    let decls = s.build(code).export;
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

/// What the `pisa` tool printed: its `PISA_*` lines by marker, in order, and
/// whether it exited 0.
struct Tooled {
    ok: bool,
    lines: Vec<(String, serde_json::Value)>,
    output: String,
}

impl Tooled {
    fn all(&self, marker: &str) -> Vec<&serde_json::Value> {
        self.lines
            .iter()
            .filter(|(m, _)| m == marker)
            .map(|(_, v)| v)
            .collect()
    }

    fn one(&self, marker: &str) -> &serde_json::Value {
        let all = self.all(marker);
        assert_eq!(all.len(), 1, "{marker} in:\n{}", self.output);
        all[0]
    }
}

impl Snapshots {
    /// Runs the `pisa` tool on the snapshot modules, on the library's
    /// toolchain.
    fn tool(&self, args: &[&str]) -> Tooled {
        let toolchain = std::fs::read_to_string(docker().join("pisa-lean/lean-toolchain")).unwrap();
        let out = Command::new(docker().join("pisa-lean/.lake/build/bin/pisa"))
            .args(args)
            .current_dir(self.root.path())
            .env("ELAN_TOOLCHAIN", toolchain.trim())
            .env("LEAN_PATH", self.root.path())
            .output()
            .expect("running pisa (is docker/pisa-lean built?)");
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let lines = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let (marker, json) = l.split_once(' ')?;
                marker.starts_with("PISA_").then_some(())?;
                Some((marker.to_string(), serde_json::from_str(json).ok()?))
            })
            .collect();
        Tooled {
            ok: out.status.success(),
            lines,
            output,
        }
    }

    /// `code`'s module file, and its `.olean`.
    fn file(&self, code: &str) -> (PathBuf, PathBuf) {
        let file = self
            .root
            .path()
            .join(snapshot::ROOT)
            .join(format!("s{}.lean", snapshot::id(code)));
        std::fs::write(&file, code).unwrap();
        let olean = file.with_extension("olean");
        (file, olean)
    }

    /// Builds `code` with `pisa build`, as the worker does.
    fn tool_build(&self, code: &str) -> Tooled {
        let (file, olean) = self.file(code);
        let root = self.root.path().to_string_lossy().into_owned();
        let (file, olean) = (file.to_string_lossy(), olean.to_string_lossy());
        let built = self.tool(&[
            "build",
            "-R",
            &root,
            "-o",
            &olean,
            "-D",
            "autoImplicit=false",
            "--value-chars",
            "4000",
            &file,
        ]);
        assert!(built.ok, "pisa build failed:\n{}", built.output);
        built
    }

    /// The verdicts of `pisa verify` on the modules of `ids`, in order.
    fn tool_verify(&self, ids: &[&str]) -> Vec<serde_json::Value> {
        let modules: Vec<String> = ids.iter().map(|id| snapshot::module(id)).collect();
        let mut args = vec!["verify"];
        args.extend(modules.iter().map(String::as_str));
        let verified = self.tool(&args);
        assert!(verified.ok, "pisa verify failed:\n{}", verified.output);
        verified.one("PISA_IMPORTED");
        verified.all("PISA_VERIFY").into_iter().cloned().collect()
    }

    /// The files of module `id`, by extension.
    fn files(&self, id: &str) -> std::collections::BTreeMap<String, Vec<u8>> {
        let dir = self.root.path().join(snapshot::ROOT);
        let prefix = format!("s{id}.");
        std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| {
                let name = e.ok()?.file_name().into_string().ok()?;
                let ext = name.strip_prefix(&prefix)?.to_string();
                (ext != "lean").then(|| (ext, std::fs::read(dir.join(&name)).unwrap()))
            })
            .collect()
    }
}

/// A verdict's declarations, judged as the worker judges them.
fn verdict_declarations(verdict: &serde_json::Value) -> Vec<Declaration> {
    assert_eq!(verdict["ok"], true, "{verdict}");
    let decls = parse_declarations(verdict["declarations"].clone()).unwrap();
    policy().judge_decls(false, Some(decls)).declarations
}

const PLAIN: &str = "/-- Doubles. -/\ndef double (n : Nat) : Nat := 2 * n\n\n\
                     theorem double_two : double 2 = 4 := rfl\n\naxiom cheat : 1 = 2\n\n\
                     theorem uses_cheat : 1 = 2 := cheat\n";
const MODULE: &str = "module\n\n/-- Doubles. -/\n@[expose] public def double (n : Nat) : Nat := 2 * n\n\n\
                      public theorem double_two : double 2 = 4 := rfl\n\n\
                      def hidden : Nat := 7\n\ntheorem hidden_eq : hidden = 7 := rfl\n";

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn the_tool_writes_what_lean_writes() {
    let (by_lean, by_tool) = (Snapshots::new(), Snapshots::new());
    let on = |id: &str| {
        format!(
            "module\n\npublic import {}\n\npublic theorem four : double 2 = 4 := double_two\n",
            snapshot::module(id)
        )
    };
    let module_id = snapshot::id(MODULE);
    for code in [PLAIN, MODULE, &on(&module_id)] {
        let id = by_lean.lean_build(code);
        by_tool.tool_build(code);
        let (lean, tool) = (by_lean.files(&id), by_tool.files(&id));
        assert!(lean.contains_key("olean"), "{:?}", lean.keys());
        assert_eq!(
            lean.keys().collect::<Vec<_>>(),
            tool.keys().collect::<Vec<_>>()
        );
        for (ext, bytes) in &lean {
            assert!(tool[ext] == *bytes, "s{id}.{ext} differs");
        }
    }
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn the_builds_report_is_the_verifiers() {
    // `agreed` compares the two.
    assert!(!Snapshots::new().build(PLAIN).agreed().is_empty());
    // Module-system code is described by the verifier alone.
    assert!(
        Snapshots::new()
            .tool_build(MODULE)
            .all("PISA_DECLS")
            .is_empty()
    );
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn the_build_names_its_imports_and_exports_what_importers_see() {
    let s = Snapshots::new();
    let base = s.tool_build(MODULE);
    let id = snapshot::id(MODULE);
    let export = base.one("PISA_EXPORT").as_array().unwrap();
    // A declaration that isn't public is exported too, under its private
    // name, with its value: a hole's value can hide behind one.
    let hidden = export
        .iter()
        .find(|d| d["name"].as_str().unwrap().ends_with(".hidden"));
    let hidden = hidden.unwrap_or_else(|| panic!("no hidden in {export:?}"));
    assert!(
        hidden["name"].as_str().unwrap().starts_with("_private."),
        "{hidden}"
    );
    assert_eq!(
        (&hidden["kind"], &hidden["value"]),
        (&serde_json::json!("def"), &serde_json::json!("7"))
    );
    let double = export.iter().find(|d| d["name"] == "double").unwrap();
    assert_eq!(double["docstring"], "Doubles.");
    // Importers see a module-system theorem as an axiom; the export says
    // what the module declares.
    let theorem = export.iter().find(|d| d["name"] == "double_two").unwrap();
    assert_eq!(theorem["kind"], "theorem");
    // A definition that isn't exposed has its value, and docstring, all the
    // same.
    let costs = "module\n\n/-- The cost. -/\npublic def cost : Nat := 7\n\n\
                 @[expose] public def cost2 : Nat := 8\n\n\
                 public theorem bound : cost ≤ 10 := by decide\n";
    let export = s.tool_build(costs).one("PISA_EXPORT").clone();
    let find = |name: &str| {
        let decls = export.as_array().unwrap();
        decls.iter().find(|d| d["name"] == name).unwrap().clone()
    };
    let (cost, cost2, bound) = (find("cost"), find("cost2"), find("bound"));
    assert_eq!(
        (&cost["kind"], &cost["value"]),
        (&serde_json::json!("def"), &serde_json::json!("7")),
        "{cost}"
    );
    assert_eq!(cost["docstring"], "The cost.");
    assert_eq!(cost2["value"], "8");
    assert_eq!(bound["kind"], "theorem");
    let on = format!(
        "module\n\npublic import {}\n\npublic theorem four : double 2 = 4 := double_two\n",
        snapshot::module(&id)
    );
    let imports = s.tool_build(&on).one("PISA_IMPORTS").clone();
    let imports: Vec<String> = serde_json::from_value(imports).unwrap();
    assert!(imports.contains(&snapshot::module(&id)), "{imports:?}");
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn an_export_of_a_built_module_is_the_one_its_build_printed() {
    let s = Snapshots::new();
    let id = snapshot::id(MODULE);
    let built = s.tool_build(MODULE).one("PISA_EXPORT").clone();
    let on = format!(
        "module\n\npublic import {}\n\n/-- Four. -/\npublic theorem four : double 2 = 4 := double_two\n\n\
         def eight : Nat := double 4\n",
        snapshot::module(&id)
    );
    let on_built = s.tool_build(&on).one("PISA_EXPORT").clone();
    for (code, built) in [(MODULE.to_string(), built), (on, on_built)] {
        let module = snapshot::module(&snapshot::id(&code));
        let exported = s.tool(&["export", "--value-chars", "4000", &module]);
        assert!(exported.ok, "pisa export failed:\n{}", exported.output);
        assert_eq!(exported.one("PISA_EXPORT"), &built, "{module}");
    }
    // Outside the module system too.
    let plain = s.tool_build(PLAIN).one("PISA_EXPORT").clone();
    let module = snapshot::module(&snapshot::id(PLAIN));
    let exported = s.tool(&["export", "--value-chars", "4000", &module]);
    assert_eq!(exported.one("PISA_EXPORT"), &plain);
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_chain_is_verified_as_its_modules_are_one_by_one() {
    let s = Snapshots::new();
    let base = snapshot::id(MODULE);
    s.tool_build(MODULE);
    let on = format!(
        "module\n\npublic import {}\n\npublic theorem four : double 2 = 4 := double_two\n",
        snapshot::module(&base)
    );
    let all = format!(
        "module\n\nimport all {}\n\ntheorem seven : hidden = 7 := hidden_eq\n",
        snapshot::module(&base)
    );
    s.tool_build(&on);
    s.tool_build(&all);
    let ids = [base.clone(), snapshot::id(&on), snapshot::id(&all)];
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let chain = s.tool_verify(&ids);
    let one_by_one: Vec<serde_json::Value> =
        ids.iter().flat_map(|id| s.tool_verify(&[*id])).collect();
    assert_eq!(chain.len(), 3);
    assert_eq!(chain, one_by_one);
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_declaration_added_without_the_kernel_is_rejected_and_ends_the_chain() {
    let s = Snapshots::new();
    let forged = "import Lean\n\nopen Lean Elab Command in\nset_option debug.skipKernelTC true in\n\
                  run_cmd liftCoreM (addDecl (.thmDecl { name := `bad, levelParams := [], \
                  type := mkConst ``False, value := mkConst ``True.intro }))\n\n\
                  theorem oops : False := bad\n";
    s.tool_build(forged);
    s.tool_build(PLAIN);
    let verdicts = s.tool_verify(&[&snapshot::id(forged), &snapshot::id(PLAIN)]);
    assert_eq!(verdicts.len(), 1, "{verdicts:?}");
    assert_eq!(verdicts[0]["ok"], false);
    let reason = verdicts[0]["reason"].as_str().unwrap();
    assert!(reason.contains("bad"), "{reason}");
}

/// Code whose axioms come through an inductive type's constructors: `I.mk`
/// and what builds one depend on what `I.mk2`'s type uses.
const AXIOMS: &str = "def foo : Prop := sorry\n\n\
                      inductive I where\n  | mk : I\n  | mk2 : foo → I\n\n\
                      theorem t : Nonempty I := ⟨I.mk⟩\n\n\
                      axiom ax : 1 = 1\n\ntheorem u : 1 = 1 := ax\n\n\
                      theorem w (p : Prop) : p ∨ ¬p := Classical.em p\n\n\
                      theorem v : True := trivial\n";

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn the_axioms_found_are_leans() {
    let names = ["foo", "I", "I.mk", "I.mk2", "t", "ax", "u", "w", "v"];
    let printed: String = names
        .iter()
        .map(|n| format!("#print axioms {n}\n"))
        .collect();
    let code = format!("{AXIOMS}\n{printed}");
    let s = Snapshots::new();
    let built = s.tool_build(&code);
    // Lean's own, from `#print axioms`.
    let mut lean = std::collections::HashMap::new();
    for line in built.output.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(text) = message["data"].as_str() else {
            continue;
        };
        let Some((name, rest)) = text.strip_prefix('\'').and_then(|t| t.split_once('\'')) else {
            continue;
        };
        let axioms: Vec<String> = match rest.strip_prefix(" depends on axioms: [") {
            Some(list) => list
                .trim_end_matches(']')
                .split(", ")
                .map(String::from)
                .collect(),
            None if rest == " does not depend on any axioms" => Vec::new(),
            None => continue,
        };
        lean.insert(name.to_string(), axioms);
    }
    assert_eq!(lean.len(), names.len(), "{}", built.output);
    assert_eq!(lean["t"], ["sorryAx"]);
    // The build's report and the verifier's find what Lean does.
    let reported = parse_declarations(built.one("PISA_DECLS").clone()).unwrap();
    let verified =
        parse_declarations(s.tool_verify(&[&snapshot::id(&code)])[0]["declarations"].clone())
            .unwrap();
    for decls in [reported, verified] {
        for name in names {
            let d = decls
                .iter()
                .find(|d| d.name == name)
                .unwrap_or_else(|| panic!("{name}"));
            assert_eq!(d.axioms, lean[name], "{name}");
        }
    }
}

/// Two modules declare the same theorem with the same type, one by `sorry`.
/// Lean's replay keeps the first one's and skips the other's, so a chain of
/// both rejects the second, whichever it is, rather than describe it from
/// the first.
#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_theorem_another_module_declares_is_rejected() {
    let s = Snapshots::new();
    let (sorried, proved) = ("theorem t : 1 = 1 := sorry\n", "theorem t : 1 = 1 := rfl\n");
    s.tool_build(sorried);
    s.tool_build(proved);
    let (sorried, proved) = (snapshot::id(sorried), snapshot::id(proved));
    for (first, second) in [(&sorried, &proved), (&proved, &sorried)] {
        let verdicts = s.tool_verify(&[first, second]);
        assert_eq!(verdicts.len(), 2, "{verdicts:?}");
        assert_eq!(verdicts[0]["ok"], true, "{verdicts:?}");
        assert_eq!(verdicts[1]["ok"], false, "{verdicts:?}");
        let reason = verdicts[1]["reason"].as_str().unwrap();
        let clash = format!("`t` is also declared by `{}`", snapshot::module(first));
        assert!(reason.contains(&clash), "{reason}");
        assert!(
            reason.contains("code that imports both means whichever it imports first"),
            "{reason}"
        );
    }
    // Alone, each is described from its own proof.
    for (id, axioms) in [(&sorried, vec!["sorryAx"]), (&proved, vec![])] {
        let verdict = &s.tool_verify(&[id])[0];
        let t = verdict_declarations(verdict)
            .into_iter()
            .find(|d| d.name == "t")
            .unwrap();
        assert_eq!(t.axioms, axioms);
    }
}

/// Code that imports two modules declaring one theorem builds: Lean takes
/// the first import's, so the code is `sorry` or not by the order of its
/// imports. A create refuses such a header from the modules' records, which
/// therefore name the theorem as written in both, not as a name Lean made or
/// a private one.
#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn code_on_two_modules_that_declare_one_theorem_builds() {
    let s = Snapshots::new();
    let (sorried, proved) = ("theorem t : 1 = 1 := sorry\n", "theorem t : 1 = 1 := rfl\n");
    s.tool_build(sorried);
    s.tool_build(proved);
    let (sorried, proved) = (snapshot::id(sorried), snapshot::id(proved));
    for id in [&sorried, &proved] {
        let t = verdict_declarations(&s.tool_verify(&[id])[0])
            .into_iter()
            .find(|d| d.name == "t")
            .unwrap();
        assert_eq!((t.kind.as_str(), t.generated), ("theorem", false), "{t:?}");
    }
    let both = format!(
        "import {}\nimport {}\n\ntheorem u : 1 = 1 := t\n",
        snapshot::module(&sorried),
        snapshot::module(&proved)
    );
    s.tool_build(&both);
}

/// A failure that is no verdict on the code, here a module whose files
/// aren't there, ends the run with `PISA_FAILED` and its reason.
#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_verifier_that_cant_read_a_module_says_so() {
    let s = Snapshots::new();
    let missing = snapshot::module(&snapshot::id("theorem gone : True := trivial\n"));
    let run = s.tool(&["verify", &missing]);
    assert!(!run.ok, "{}", run.output);
    let reason = run.one("PISA_FAILED")["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(reason.contains(&format!("reading `{missing}`")), "{reason}");
    assert!(run.all("PISA_VERIFY").is_empty(), "{}", run.output);
}

/// The export prints a `sorry` as `sorry`, alone or inside a value, rather
/// than `sorryAx` with its type and label, and it exports a private
/// declaration too, marked so that search leaves it out.
#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn the_export_prints_sorry_and_marks_what_is_private() {
    let code = "private def h : Nat := sorry\n\n\
                def cost (b : Bool) : Nat := if b then sorry else 1\n";
    let export = Snapshots::new().tool_build(code).one("PISA_EXPORT").clone();
    let decls = export.as_array().unwrap();
    let h = decls
        .iter()
        .find(|d| d["name"].as_str().unwrap().ends_with(".h"))
        .unwrap_or_else(|| panic!("{export}"));
    assert_eq!(
        (&h["value"], &h["private"]),
        (&serde_json::json!("sorry"), &serde_json::json!(true))
    );
    let cost = decls.iter().find(|d| d["name"] == "cost").unwrap();
    let value = cost["value"].as_str().unwrap();
    assert!(
        value.contains("sorry") && !value.contains("sorryAx"),
        "{value}"
    );
    assert_eq!(cost["private"], false);
}

/// Code whose hashes [`the_hashes_stay_as_they_were`] pins.
const GOLDEN: &str = "universe u\n\ndef pick {α : Sort u} (a : α) : α := a\n\n\
                      private def secret : Nat := 7\n\ndef shown : Nat := secret + 1\n\n\
                      structure Pair where\n  fst : Nat\n  snd : Nat\n\n\
                      theorem pick_eq {α : Sort u} (a : α) : pick a = a := rfl\n\n\
                      theorem shown_eq : shown = 8 := rfl\n\n\
                      theorem no_two : ¬ (1 = 2) := by decide\n";

/// Every stored hash rests on the canonical text staying byte for byte as it
/// was: these are the hashes `GOLDEN` had when they were pinned.
#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn the_hashes_stay_as_they_were() {
    let built = Snapshots::new().build(GOLDEN);
    let hashes: std::collections::BTreeMap<&str, (&str, Option<&str>)> = built
        .agreed()
        .iter()
        .map(|d| {
            (
                d.name.as_str(),
                (d.type_hash.as_str(), d.value_hash.as_deref()),
            )
        })
        .collect();
    let private = "_private.PisaSnap.s1348e034451a3b4f95e593b020e0b164.0.secret";
    for (name, type_hash, value_hash) in [
        (
            "pick",
            "63afc3994e34afd7a807f5cc93e87c77dbd337e09bf7d2ed2487352cf8cc7e92",
            Some("8482dcd182de076b47e37a1f28275127bb72e7e7bcd5bd39f9b0d6560b00ca40"),
        ),
        (
            private,
            "7f15ccf3a21eb28b2284f9c65ee3d181fc35572ec709c8239fe6c95eb724b5e3",
            Some("dbc440a75808e6b49fe7d04de96d1ac62f6417bec221ac4a26b483c7c1dda71e"),
        ),
        (
            "shown",
            "7f15ccf3a21eb28b2284f9c65ee3d181fc35572ec709c8239fe6c95eb724b5e3",
            Some("6c8b16fe2e38f84e52a99dee154d60a176fd8b00567e9336ad1e50fd8ac6dec4"),
        ),
        (
            "Pair",
            "a1d150d6c6c6a66c047b10c9d1208e0760e1bc3ca2c16c565bd911954a141b59",
            None,
        ),
        (
            "Pair.mk",
            "3af6be296acd3f8c1b7bd372d18084b9b6df0e617998a90f35690ea8805b55de",
            None,
        ),
        (
            "pick_eq",
            "5a1a91162dc39fd68c044a0b508bb3266c0fb56ec788d55806f530dc3180ef8d",
            None,
        ),
        (
            "shown_eq",
            "5ac646b80a71c18414dec2b077b557fa1c83bfde5388556fc06cbac511826327",
            None,
        ),
        (
            "no_two",
            "88d3743d03635be4defedec43ab80c582d27ec4ca94e8a87bdef521b57145803",
            None,
        ),
    ] {
        assert_eq!(hashes.get(name), Some(&(type_hash, value_hash)), "{name}");
    }
}

/// The texts of the declarations of `code`, built on `snapshots` by `pisa
/// build`, then by `pisa verify`, which must agree, by name: `(type,
/// negates)`.
fn texts(
    snapshots: &Snapshots,
    code: &str,
) -> std::collections::BTreeMap<String, (String, Option<String>)> {
    let read = |decls: &serde_json::Value| -> std::collections::BTreeMap<String, (String, Option<String>)> {
        let decls = decls.as_array().unwrap();
        let text = |d: &serde_json::Value, key: &str| d[key].as_str().map(String::from);
        decls
            .iter()
            .map(|d| {
                let name = text(d, "name").unwrap();
                (name, (text(d, "type").unwrap(), text(d, "negates")))
            })
            .collect()
    };
    let built = read(snapshots.tool_build(code).one("PISA_DECLS"));
    let verified = snapshots.tool_verify(&[&snapshot::id(code)]);
    assert_eq!(
        read(&verified[0]["declarations"]),
        built,
        "build and verifier"
    );
    built
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_theorem_of_a_negation_says_what_it_negates() {
    let snapshots = Snapshots::new();
    let spec = "universe u\n\ntheorem t : ∀ n : Nat, n + 0 = n + 1 := sorry\n\n\
                theorem any {α : Sort u} (a b : α) : a = b := sorry\n\n\
                private def h : Nat := 1\n\ntheorem uses_h : h = 2 := sorry\n";
    let spec_texts = texts(&snapshots, spec);
    let refuter = format!(
        "import {}\n\nuniverse v\n\n\
         theorem by_not : ¬ ∀ n : Nat, n + 0 = n + 1 := fun h => by have := h 0; omega\n\n\
         theorem by_false (h : ∀ m : Nat, m + 0 = m + 1) : False := by have := h 0; omega\n\n\
         theorem by_type_of : ¬ type_of% @t := fun h => by have := h 0; omega\n\n\
         theorem renamed : ¬ ∀ {{β : Sort v}} (x y : β), x = y := sorry\n\n\
         theorem fixed : ¬ ∀ {{β : Sort 1}} (x y : β), x = y := sorry\n\n\
         private def h : Nat := 1\n\ntheorem own_h : ¬ (h = 2) := by decide\n\n\
         theorem not_one : ¬ (1 = 2) := by decide\n",
        snapshot::module(&snapshot::id(spec))
    );
    let refuter_texts = texts(&snapshots, &refuter);
    let negates = |name: &str| refuter_texts[name].1.clone();
    let t = Some(spec_texts["t"].0.clone());
    for name in ["by_not", "by_false", "by_type_of"] {
        assert_eq!(negates(name), t, "{name}");
    }
    // Universe parameters count by position, so a renamed one matches.
    let any = Some(spec_texts["any"].0.clone());
    assert_eq!(negates("renamed"), any);
    assert!(negates("fixed").is_some() && negates("fixed") != any);
    // The refuter's own private `h` is written whole, the spec's as its own.
    let private = spec_texts.keys().find(|n| n.ends_with(".uses_h")).cloned();
    let uses_h = spec_texts[private.as_deref().unwrap_or("uses_h")].0.clone();
    assert!(negates("own_h").is_some() && negates("own_h") != Some(uses_h));
    assert!(negates("not_one").is_some());
    // What isn't a theorem of a negation says nothing.
    assert_eq!(spec_texts["t"].1, None);
    // Hashed, the worker's records agree.
    let hashes = |code: &str| -> std::collections::BTreeMap<String, Declaration> {
        let built = snapshots.tool_build(code);
        let decls = parse_declarations(built.one("PISA_DECLS").clone()).unwrap();
        let decls = policy().judge_decls(false, Some(decls)).declarations;
        decls.into_iter().map(|d| (d.name.clone(), d)).collect()
    };
    let (spec, refuter) = (hashes(spec), hashes(&refuter));
    assert_eq!(refuter["by_not"].negates, Some(spec["t"].type_hash.clone()));
    assert_eq!(spec["t"].negates, None);
}

/// Each declaration of a verdict or report, by name: `(own_axioms, inherits)`
/// as `[(name, module)]`.
type Linked = std::collections::BTreeMap<String, (Vec<String>, Vec<(String, String)>)>;

fn linked(decls: &serde_json::Value) -> Linked {
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    };
    decls
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            let inherits = d["inherits"].as_array().unwrap().iter().map(|i| {
                let text = |k: &str| i[k].as_str().unwrap().to_string();
                (text("name"), text("module"))
            });
            let name = d["name"].as_str().unwrap().to_string();
            (name, (strings(&d["own_axioms"]), inherits.collect()))
        })
        .collect()
}

#[test]
#[ignore = "needs elan Lean 4.36.0-rc2"]
fn a_theorem_proved_by_another_modules_lemma_inherits_its_axioms() {
    let snapshots = Snapshots::new();
    let lemmas = "def cost : Nat := sorry\n\ntheorem lemma_l : 1 + 1 = 2 := sorry\n\n\
                  def sub : {n : Nat // n = 1} := ⟨1, by sorry⟩\n\n\
                  opaque foo : Nat\n\n@[simp] private theorem foo_eq : foo = 3 := sorry\n\n\
                  theorem proved : 2 = 2 := rfl\n";
    let spec = snapshots.build(lemmas);
    let module = snapshot::module(&spec.id);
    let head = format!(
        "import {module}\n\ntheorem by_lemma : 1 + 1 = 2 := lemma_l\n\n\
         theorem own : 1 = 1 := sorry\n\ntheorem through_def : cost = cost := rfl\n\n\
         theorem through_sub : sub.val = 1 := sub.property\n\n\
         theorem by_simp : foo = 3 := by simp\n\ntheorem by_proved : 2 = 2 := proved\n"
    );
    let built = snapshots.tool_build(&head);
    let head_id = snapshot::id(&head);
    let reported = linked(built.one("PISA_DECLS"));
    let sorry = |own: &[String]| own.iter().any(|a| a == "sorryAx");
    let (own, inherits) = &reported["by_lemma"];
    assert!(!sorry(own), "{own:?}");
    assert_eq!(inherits, &[("lemma_l".to_string(), module.clone())]);
    for name in ["own", "through_def", "through_sub"] {
        let (own, inherits) = &reported[name];
        assert!(
            sorry(own) && inherits.is_empty(),
            "{name}: {own:?} {inherits:?}"
        );
    }
    // A private lemma, reached through `simp`, is inherited by its full name.
    let (own, inherits) = &reported["by_simp"];
    assert!(!sorry(own), "{own:?}");
    assert_eq!(inherits.len(), 1, "{inherits:?}");
    assert!(inherits[0].0.ends_with(".foo_eq") && inherits[0].1 == module);
    // A theorem with only the standard axioms isn't inherited.
    assert_eq!(reported["by_proved"].1, []);
    // The verifier finds the same, alone and after the lemmas' module in one
    // chain, where that module's constants have no module index.
    let alone = snapshots.tool_verify(&[&head_id]);
    assert_eq!(linked(&alone[0]["declarations"]), reported);
    let chain = snapshots.tool_verify(&[&spec.id, &head_id]);
    assert_eq!(linked(&chain[1]["declarations"]), reported);
    // The lemmas' own module inherits nothing from itself.
    assert!(
        linked(&chain[0]["declarations"])
            .values()
            .all(|(_, i)| i.is_empty())
    );
}
