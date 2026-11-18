//! `lake comparator` as acceptance runs it, on real Lean: snapshot modules
//! are built as a worker builds them, and a run's pairs judged with
//! [`runs::judge`]. What a judged solution's code does while it is judged
//! must not reach what comparator judges after it: the next pair's
//! challenge, a later judgment's, or the programs an export runs. The judged
//! code here runs with `#eval`, which a worker's policy refuses unless it
//! allows metaprogramming, so these test the comparator layer, not what a
//! default worker would store. Each test needs elan on `PATH`,
//! `docker/pisa-lean` built and a `bwrap` that works, so they are ignored by
//! default:
//!
//! ```text
//! (cd docker/pisa-lean && lake build)
//! cargo test -p pisa-worker --test real_comparator -- --ignored
//! ```

mod lean;

use std::path::Path;
use std::time::Duration;

use clap::Parser;
use pisa_protocol::api::PairErrorKind;
use pisa_protocol::repl::Severity;
use pisa_protocol::snapshot;
use pisa_protocol::wire::ExportedFile;
use pisa_worker::WorkerConfig;
use pisa_worker::launcher::Launcher;
use pisa_worker::runs::{self, JudgeOptions, Judgment, ModulesToJudge, PairToJudge};

use lean::docker;

/// A spec: `t` states something false.
const S: &str = "theorem t : 1 = 2 := sorry\n";
/// A head that proves another statement than S's.
const H: &str = "theorem t : 1 = 1 := rfl\n";
/// A spec, and a head that proves it.
const C: &str = "theorem c : 2 = 2 := sorry\n";
const G: &str = "theorem c : 2 = 2 := rfl\n";

/// What E adds to S: code that runs as Lake builds E. It compiles H's
/// theorem as a module named as S's (`{spec}`), and writes it over S's
/// `.olean` wherever Lean finds modules, saying so with `FORGED`.
const FORGE: &str = r#"
#eval show IO Unit from do
  try
    let forge : System.FilePath := "/tmp/pisa-forge"
    let src := forge / "PisaSnap" / "s{spec}.lean"
    let olean := src.withExtension "olean"
    IO.FS.createDirAll (forge / "PisaSnap")
    IO.FS.writeFile src "theorem t : 1 = 1 := rfl\n"
    let lean := (← IO.appPath).toString
    let out ← IO.Process.output
      { cmd := lean, args := #["-R", forge.toString, "-o", olean.toString, src.toString] }
    if out.exitCode != 0 then throw (IO.userError out.stderr)
    let forged ← IO.FS.readBinFile olean
    let path := System.SearchPath.parse ((← IO.getEnv "LEAN_PATH").getD "")
    for entry in path ++ [(".lake/build/lib/lean" : System.FilePath)] do
      let target := entry / "PisaSnap" / "s{spec}.olean"
      if ← target.pathExists then
        if (← (IO.FS.writeBinFile target forged).toBaseIO) matches .ok _ then
          IO.eprintln s!"FORGED {target}"
  catch e =>
    IO.eprintln s!"forge failed: {e}"
"#;

/// S's statement, still unproved, and [`FORGE`] aimed at S.
fn e() -> String {
    format!("{S}{}", FORGE.replace("{spec}", &snapshot::id(S)))
}

/// G's proof, and code that runs as Lake builds it: it writes a `lean` into
/// the project's `.lake/build/bin`, which `lake env` puts first on `PATH`.
/// That `lean` says `PLANTED` on the stderr of the program that ran it, as
/// whoever runs `lean --print-prefix` swallows its own, and then runs the
/// real one.
const P: &str = r##"theorem c : 2 = 2 := rfl

#eval show IO Unit from do
  try
    let bin : System.FilePath := ".lake/build/bin"
    IO.FS.createDirAll bin
    let lean := bin / "lean"
    IO.FS.writeFile lean
      s!"#!/bin/sh\necho \"PLANTED $0 $*\" >/proc/$PPID/fd/2\nexec {(← IO.appPath)} \"$@\"\n"
    IO.setAccessRights lean { user := { read := true, write := true, execution := true } }
  catch e =>
    IO.eprintln s!"planting failed: {e}"
"##;

/// The slot everything runs on.
const SLOT: usize = 0;

/// A worker's launcher, sandboxed with bwrap, on a copy of
/// `docker/lean-core` as its project.
struct Harness {
    launcher: Launcher,
    project: tempfile::TempDir,
    _work: tempfile::TempDir,
}

fn bwrap_works() -> bool {
    std::process::Command::new("bwrap")
        .args(["--unshare-all", "--ro-bind", "/", "/", "true"])
        .status()
        .is_ok_and(|s| s.success())
}

fn path(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

impl Harness {
    async fn new() -> Self {
        assert!(
            bwrap_works(),
            "bwrap can't sandbox here: install bubblewrap, and let it create user namespaces \
             (`sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`)"
        );
        // Not under /tmp: the sandbox puts its own /tmp there, and runs Lean
        // in the project.
        let project = tempfile::Builder::new()
            .prefix("pisa-real-comparator-")
            .tempdir_in("/var/tmp")
            .unwrap();
        for name in ["lakefile.toml", "lean-toolchain"] {
            std::fs::copy(
                docker().join("lean-core").join(name),
                project.path().join(name),
            )
            .unwrap();
        }
        std::fs::write(
            project.path().join("lake-manifest.json"),
            r#"{"version": "1.3.0", "packagesDir": ".lake/packages", "packages": [],
                "name": "pisa_core", "lakeDir": ".lake", "fixedToolchain": false}"#,
        )
        .unwrap();
        let work = tempfile::tempdir().unwrap();
        let cfg = WorkerConfig::try_parse_from([
            "pisa-worker",
            "--database-url",
            "x",
            "--sandbox",
            "bwrap",
            "--tool",
            &path(&docker().join("pisa-lean/.lake/build/bin/pisa")),
            "--project",
            &path(project.path()),
            "--work-dir",
            &path(work.path()),
        ])
        .unwrap();
        let launcher = Launcher::new(&cfg, "real-comparator", "v")
            .await
            .expect("starting the launcher (is elan on PATH?)");
        Harness {
            launcher,
            project,
            _work: work,
        }
    }

    /// Builds each of `codes` as a worker builds a snapshot, compiled in
    /// the slot's sandbox and its files moved into the snapshot root as
    /// `install` moves them, and gives the modules to judge them on.
    async fn build(&self, codes: &[&str]) -> ModulesToJudge {
        let root = self.launcher.slot_dir(SLOT).join("build");
        let dir = root.join(snapshot::ROOT);
        std::fs::create_dir_all(&dir).unwrap();
        let installed = self.launcher.snapshot_root().join(snapshot::ROOT);
        let mut sources =
            runs::project_files(Some(self.project.path()), &[snapshot::ROOT]).unwrap();
        for code in codes {
            let id = snapshot::id(code);
            let file = dir.join(format!("s{id}.lean"));
            std::fs::write(&file, code).unwrap();
            let olean = file.with_extension("olean");
            let compiled = (self.launcher)
                .compile(SLOT, &root, &file, &olean, Duration::from_secs(300))
                .await
                .unwrap();
            let failed = compiled
                .messages
                .iter()
                .any(|m| m.severity == Severity::Error);
            assert!(
                compiled.status.is_some_and(|s| s.success()) && !failed,
                "building s{id} failed: {compiled:?}"
            );
            let prefix = format!("s{id}.");
            for entry in std::fs::read_dir(&dir).unwrap() {
                let name = entry.unwrap().file_name().into_string().unwrap();
                if name.strip_prefix(&prefix).is_some_and(|ext| ext != "lean") {
                    std::fs::rename(dir.join(&name), installed.join(&name)).unwrap();
                }
            }
            sources.push(ExportedFile {
                path: format!("{}/s{id}.lean", snapshot::ROOT),
                text: code.to_string(),
            });
        }
        ModulesToJudge {
            root: self.launcher.runs_dir().join("run"),
            sources,
        }
    }

    /// Judges `pairs` on `modules` as acceptance does, without `--paranoid`.
    async fn judge(&self, modules: &ModulesToJudge, pairs: &[PairToJudge]) -> Vec<Judgment> {
        let options = JudgeOptions {
            paranoid: false,
            timeout: Duration::from_secs(300),
            build_timeout: Duration::from_secs(300),
        };
        let judged = runs::judge(
            &self.launcher,
            SLOT,
            modules,
            Some(self.project.path()),
            pairs,
            &options,
        )
        .await
        .unwrap();
        for (n, j) in judged.iter().enumerate() {
            let kind = j.error.as_ref().map(|e| e.kind);
            eprintln!(
                "--- pair {}: exit {:?}, error {kind:?}\n{}",
                n + 1,
                j.exit_code,
                j.output
            );
        }
        judged
    }
}

/// A pair as pisa configures it: `spec`'s module the challenge and `head`'s
/// the solution, compared on `theorem`, with the standard axioms.
fn pair(spec: &str, head: &str, theorem: &str) -> PairToJudge {
    let spec = snapshot::module(&snapshot::id(spec));
    let head = snapshot::module(&snapshot::id(head));
    let configuration = serde_json::json!({
        "challenge_module": spec,
        "solution_module": head,
        "theorem_names": [theorem],
        "definition_names": [],
        "permitted_axioms": ["propext", "Quot.sound", "Classical.choice"],
    });
    PairToJudge {
        spec_module: spec,
        head_module: head,
        configuration: configuration.to_string(),
        theorems: 1,
    }
}

/// Asserts that comparator refused `judged` because the head proves another
/// statement than the spec's, as it does H's against S's.
fn assert_mismatch(judged: &Judgment) {
    let kind = judged.error.as_ref().map(|e| e.kind);
    assert!(
        kind == Some(PairErrorKind::Refused)
            && judged.output.contains("statement do not match: 't'"),
        "H's `t : 1 = 1` wasn't refused against S's `t : 1 = 2` ({kind:?}):\n{}",
        judged.output
    );
}

#[tokio::test]
#[ignore = "real Lean and bwrap"]
async fn the_harness_judges_a_good_pair_and_a_mismatch() {
    let h = Harness::new().await;
    let modules = h.build(&[C, G, S, H]).await;
    let judged = h.judge(&modules, &[pair(C, G, "c"), pair(S, H, "t")]).await;
    assert!(
        judged[0].error.is_none() && judged[0].exit_code == Some(0),
        "{}",
        judged[0].output
    );
    assert_mismatch(&judged[1]);
}

/// E's code, run as Lake builds E for the first pair, writes over the
/// `.olean` of S, the next pair's challenge too. Lake replays S from it, as
/// its trace is intact, so comparator would compare H with a forged S.
///
/// Expected to fail until acceptance stops building in a shared `.lake`.
#[tokio::test]
#[ignore = "real Lean and bwrap"]
async fn a_pair_cannot_forge_the_next_pairs_challenge() {
    let h = Harness::new().await;
    let e = e();
    let modules = h.build(&[S, &e, H]).await;
    let judged = h
        .judge(&modules, &[pair(S, &e, "t"), pair(S, H, "t")])
        .await;
    assert_mismatch(&judged[1]);
}

/// A judgment of E against S leaves its forged S behind, in the run's
/// directory, for a later judgment of a run with the same modules.
///
/// Expected to fail until acceptance stops building in a shared `.lake`.
#[tokio::test]
#[ignore = "real Lean and bwrap"]
async fn a_rejudgment_cannot_use_what_the_last_one_left() {
    let h = Harness::new().await;
    let e = e();
    let modules = h.build(&[S, &e, H]).await;
    h.judge(&modules, &[pair(S, &e, "t")]).await;
    let judged = h.judge(&modules, &[pair(S, H, "t")]).await;
    assert_mismatch(&judged[0]);
}

/// P's code, run as Lake builds P, plants a `lean` that the exporter runs
/// to find its prefix: for P's own export and for every export after it.
///
/// Expected to fail until acceptance stops building in a shared `.lake`.
#[tokio::test]
#[ignore = "real Lean and bwrap"]
async fn an_export_runs_no_program_the_judged_code_wrote() {
    let h = Harness::new().await;
    let modules = h.build(&[C, P, S, H]).await;
    let judged = h.judge(&modules, &[pair(C, P, "c"), pair(S, H, "t")]).await;
    // Both pairs were judged through their exports, so a lean P planted
    // would have run.
    assert!(
        judged[0].error.is_none() && judged[0].exit_code == Some(0),
        "{}",
        judged[0].output
    );
    assert_mismatch(&judged[1]);
    for (n, j) in judged.iter().enumerate() {
        assert!(
            !j.output.contains("PLANTED"),
            "pair {} ran the lean P planted:\n{}",
            n + 1,
            j.output
        );
    }
}
