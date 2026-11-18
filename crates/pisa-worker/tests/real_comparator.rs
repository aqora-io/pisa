//! `lake comparator` as acceptance runs it, on real Lean: snapshot modules
//! are built as a worker builds them, linked for a judgment with
//! [`Snapshots::link_verified`], and a run's pairs judged with
//! [`runs::judge`], on exports of those builds. What a judged solution's
//! code does when it is built must not reach what comparator judges after
//! it: the next pair's challenge, a later judgment's, or the programs an
//! export runs. The judged code here runs with `#eval`, which a worker's
//! policy refuses unless it allows metaprogramming, so these test the
//! comparator layer, not what a default worker would store. Each test needs
//! elan on `PATH`, `docker/pisa-lean` built and a `bwrap` that works, so they
//! are ignored by default:
//!
//! ```text
//! (cd docker/pisa-lean && lake build)
//! cargo test -p pisa-worker --test real_comparator -- --ignored
//! ```

mod lean;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use clap::Parser;
use object_store::memory::InMemory;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt};
use pisa_protocol::api::{CheckResult, KernelOutcome, PairErrorKind};
use pisa_protocol::repl::Severity;
use pisa_protocol::snapshot;
use pisa_protocol::verify::ProofStatus;
use pisa_worker::WorkerConfig;
use pisa_worker::catalog::{Catalog, Module, ModuleFile, ModuleMeta};
use pisa_worker::db::Db;
use pisa_worker::launcher::Launcher;
use pisa_worker::policy::Policy;
use pisa_worker::runs::{self, JudgeOptions, Judgment, ModulesToJudge, PairToJudge};
use pisa_worker::snapshots::Snapshots;
use sha2::{Digest, Sha256};

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
/// the project's `.lake/build/bin`, which `lake env` puts first on `PATH`,
/// for the exporter to run. It builds in the project, as a worker builds.
const P: &str = r##"theorem c : 2 = 2 := rfl

#eval show IO Unit from do
  try
    let bin : System.FilePath := ".lake/build/bin"
    IO.FS.createDirAll bin
    let lean := bin / "lean"
    IO.FS.writeFile lean s!"#!/bin/sh\nexec {(← IO.appPath)} \"$@\"\n"
    IO.setAccessRights lean { user := { read := true, write := true, execution := true } }
  catch e =>
    IO.eprintln s!"planting failed: {e}"
"##;

/// The slot everything runs on.
const SLOT: usize = 0;

/// The pool version of the launcher and its snapshots.
const VERSION: &str = "v";

/// A worker's launcher, sandboxed with bwrap, on a copy of
/// `docker/lean-core` as its project, and its snapshots, on a store and a
/// catalog of its own.
struct Harness {
    launcher: Launcher,
    snapshots: Snapshots,
    store: Arc<dyn ObjectStore>,
    catalog: Catalog,
    project: tempfile::TempDir,
    _work: tempfile::TempDir,
    /// How many judgments it has run, which names the next one's directory.
    judgments: AtomicUsize,
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
        let launcher = Launcher::new(&cfg, "real-comparator", VERSION)
            .await
            .expect("starting the launcher (is elan on PATH?)");
        let store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let db = Db::memory().await;
        let policy = Policy {
            allowed_axioms: (pisa_worker::policy::DEFAULT_AXIOMS.split(','))
                .map(String::from)
                .collect(),
            allowed_options: Vec::new(),
            allow_metaprogramming: false,
        };
        let snapshots = Snapshots::new(
            store.clone(),
            db.clone(),
            VERSION.into(),
            launcher.snapshot_root().to_path_buf(),
            Duration::from_secs(60),
            policy,
        );
        Harness {
            launcher,
            snapshots,
            store,
            catalog: Catalog::new(db, VERSION.into()),
            project,
            _work: work,
            judgments: AtomicUsize::new(0),
        }
    }

    /// Builds each of `codes` as a worker builds a snapshot: compiled in
    /// the slot's sandbox, its files moved into the snapshot root as
    /// `install` moves them and uploaded to the store, and its record,
    /// which imports nothing of the others, kept in the catalog. Returns
    /// their ids.
    async fn build(&self, codes: &[&str]) -> Vec<String> {
        let root = self.launcher.slot_dir(SLOT).join("build");
        let dir = root.join(snapshot::ROOT);
        std::fs::create_dir_all(&dir).unwrap();
        let installed = self.launcher.snapshot_root().join(snapshot::ROOT);
        let mut ids = Vec::new();
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
            let build = "real-comparator";
            let prefix = format!("s{id}.");
            let mut files = Vec::new();
            for entry in std::fs::read_dir(&dir).unwrap() {
                let name = entry.unwrap().file_name().into_string().unwrap();
                let Some(ext) = name.strip_prefix(&prefix).filter(|ext| *ext != "lean") else {
                    continue;
                };
                let bytes = std::fs::read(dir.join(&name)).unwrap();
                files.push(ModuleFile {
                    ext: ext.to_string(),
                    sha256: hex::encode(Sha256::digest(&bytes)),
                });
                // Where a worker uploads a build's file.
                let blob = format!("snapshots/{VERSION}/{id}.{build}.{ext}.zst");
                let compressed = zstd::encode_all(&bytes[..], 3).unwrap();
                (self.store.put(&ObjectPath::from(blob), compressed.into()))
                    .await
                    .unwrap();
                std::fs::rename(dir.join(&name), installed.join(&name)).unwrap();
            }
            let meta = ModuleMeta {
                imports: Vec::new(),
                modules: vec![Module {
                    id: id.clone(),
                    build: build.into(),
                    files,
                }],
                header: String::new(),
                code: code.to_string(),
                status: ProofStatus::Proved,
                verified: true,
                macros: false,
                result: CheckResult::default(),
                created: 0,
            };
            self.catalog.commit_module(&id, &meta, None).await.unwrap();
            ids.push(id);
        }
        ids
    }

    /// Writes a `lean` into the project's `.lake/build/bin`, which `lake env`
    /// puts first on the `PATH` of the programs the worker runs, the
    /// exporter among them, as judged code that could write there would.
    /// Run for a program, it writes `PLANTED` into that program's standard
    /// output (`/proc/$PPID/fd/1`), which is the exporter's export, 16 MiB
    /// in: past the export's end, as the exporter writes its own from the
    /// start of the file. Then it runs the real `lean`.
    async fn plant_lean(&self) {
        let bin = self.project.path().join(".lake/build/bin");
        let out = std::process::Command::new("lake")
            .args(["env", "printenv", "PATH"])
            .current_dir(self.project.path())
            .output()
            .unwrap();
        let path = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            std::env::split_paths(path.trim()).next(),
            Some(bin.clone()),
            "`lake env` doesn't put {} first on PATH: {path}",
            bin.display()
        );
        let lean = self
            .launcher
            .toolchain_prefix()
            .await
            .unwrap()
            .join("bin/lean");
        std::fs::create_dir_all(&bin).unwrap();
        let planted = bin.join("lean");
        std::fs::write(
            &planted,
            format!(
                "#!/bin/sh\nprintf 'PLANTED %s\\n' \"$0\" | \
                 dd of=/proc/$PPID/fd/1 bs=1M seek=16 conv=notrunc 2>/dev/null\n\
                 exec {} \"$@\"\n",
                lean.display()
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&planted).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&planted, perms).unwrap();
    }

    /// Judges `pairs` on the modules `ids` as acceptance does, without
    /// `--paranoid`.
    async fn judge(&self, ids: &[String], pairs: &[PairToJudge]) -> Vec<Judgment> {
        self.judge_with(ids, pairs, false).await
    }

    /// Judges `pairs` on the modules `ids` as acceptance does: in a
    /// directory of the judgment's own, which holds the modules' files as
    /// [`Snapshots::link_verified`] links them, each hashed again against
    /// its record.
    async fn judge_with(
        &self,
        ids: &[String],
        pairs: &[PairToJudge],
        paranoid: bool,
    ) -> Vec<Judgment> {
        let n = self.judgments.fetch_add(1, Ordering::Relaxed);
        let root = self.launcher.judging_dir().join(n.to_string());
        let closures = self.snapshots.link_verified(ids, &root).await.unwrap();
        let judging = ModulesToJudge {
            root,
            closures: (closures.into_iter())
                .map(|(id, closure)| (snapshot::module(&id), closure))
                .collect(),
        };
        let options = JudgeOptions {
            paranoid,
            timeout: Duration::from_secs(300),
        };
        let said = runs::ProgressSender::new(None);
        let mut progress = runs::Progress::new(&self.launcher, &said);
        let judged = runs::judge(
            &self.launcher,
            SLOT,
            &judging,
            Some(self.project.path()),
            pairs,
            &options,
            &mut progress,
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
    pair_with(spec, head, theorem, &[])
}

/// [`pair`] with `holes` as its definition holes.
fn pair_with(spec: &str, head: &str, theorem: &str, holes: &[&str]) -> PairToJudge {
    let spec = snapshot::module(&snapshot::id(spec));
    let head = snapshot::module(&snapshot::id(head));
    let configuration = serde_json::json!({
        "challenge_module": spec,
        "solution_module": head,
        "theorem_names": [theorem],
        "definition_names": holes,
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

/// E's code, were it run as Lake builds E for the first pair, would write
/// over the `.olean` of S, the next pair's challenge too: Lake would replay
/// S from it, as its trace is intact, and comparator would compare H with a
/// forged S. Judged on exports of pisa's builds, nothing is built.
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

/// A judgment of E against S, were E built for it, would leave its forged S
/// behind for a later judgment of a run with the same modules. Each judgment
/// has a directory of its own, and an export is kept by what it is made of.
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

/// A `lean` first on the exporter's `PATH`, which the exporter would run to
/// find the toolchain without `LEAN_SYSROOT`, for each export. P's code, as
/// it builds, can't write one: the project is read-only in the slot's
/// sandbox. So the harness plants one there itself, as judged code that
/// found a way would, which writes into each export it is run for. The
/// exporter has `LEAN_SYSROOT` and runs no `lean`: both pairs are judged on
/// their exports, accepted and refused as they should be, and no export
/// holds what the planted `lean` writes.
#[tokio::test]
#[ignore = "real Lean and bwrap"]
async fn an_export_runs_no_program_the_judged_code_wrote() {
    let h = Harness::new().await;
    let modules = h.build(&[C, P, S, H]).await;
    let planted = h.project.path().join(".lake/build/bin/lean");
    assert!(!planted.exists(), "P's build wrote into the project");
    h.plant_lean().await;
    let judged = h.judge(&modules, &[pair(C, P, "c"), pair(S, H, "t")]).await;
    assert!(
        judged[0].error.is_none() && judged[0].exit_code == Some(0),
        "{}",
        judged[0].output
    );
    assert_mismatch(&judged[1]);
    let exports = h.launcher.judging_dir().join("0/exports");
    let mut read = 0;
    for entry in std::fs::read_dir(&exports).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        let planted = bytes.windows(7).any(|w| w == b"PLANTED");
        assert!(!planted, "the planted lean wrote into {}", path.display());
        read += 1;
    }
    // Each pair's challenge and solution.
    assert_eq!(read, 4, "{}", exports.display());
}

/// A spec with a hole, and a head that fills it and proves the spec's
/// theorem, are accepted from the exports of their builds, by every kernel
/// Lean bundles and Lean's own.
#[tokio::test]
#[ignore = "real Lean and bwrap"]
async fn a_spec_and_head_are_accepted_from_their_exports() {
    let h = Harness::new().await;
    let spec = "def cost : Nat := sorry\ntheorem t : cost = cost := sorry\n";
    let head = "def cost : Nat := 3\ntheorem t : cost = cost := rfl\n";
    let modules = h.build(&[spec, head]).await;
    let judged = h
        .judge_with(&modules, &[pair_with(spec, head, "t", &["cost"])], true)
        .await;
    assert!(
        judged[0].error.is_none() && judged[0].exit_code == Some(0),
        "{}",
        judged[0].output
    );
    assert!(judged[0].output.contains("Your solution is okay!"));
    let kernels: Vec<(&str, KernelOutcome)> = (judged[0].kernels.iter())
        .map(|k| (k.name.as_str(), k.verdict))
        .collect();
    let names = [
        "Lean paranoid",
        "con-leche",
        "con-ron",
        "lean4lean",
        "nanoda",
        "Lean default",
    ];
    assert_eq!(kernels, names.map(|n| (n, KernelOutcome::Accepted)));
    // Each kernel ran, as comparator says.
    for name in names {
        let ran = format!("{name} kernel accepts the solution");
        assert!(
            judged[0].output.contains(&ran),
            "{name}: {}",
            judged[0].output
        );
    }
}

/// con-leche and con-ron take their worker count as one `--jobs=N` argument,
/// which pisa passes from the slot's memory: real comparator accepts the
/// configuration and runs each kernel under the name pisa lists. The harness
/// has no cgroup, so the judged pair's configuration gets the flags as pisa
/// writes them for a cap, and comparator runs it again on the same exports.
#[tokio::test]
#[ignore = "real Lean and bwrap"]
async fn kernels_run_with_the_jobs_pisa_passes() {
    let h = Harness::new().await;
    let spec = "def cost : Nat := sorry\ntheorem t : cost = cost := sorry\n";
    let head = "def cost : Nat := 3\ntheorem t : cost = cost := rfl\n";
    let modules = h.build(&[spec, head]).await;
    let judged = h
        .judge_with(&modules, &[pair_with(spec, head, "t", &["cost"])], true)
        .await;
    assert_eq!(judged[0].exit_code, Some(0), "{}", judged[0].output);
    let judging = h.launcher.judging_dir().join("0");
    let config = judging.join("comparator/1.json");
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    for (name, jobs) in [("con-leche", "--jobs=3"), ("con-ron", "--jobs=2")] {
        let command = json["external_kernels"][name].as_array_mut().unwrap();
        assert_eq!(command.len(), 1, "{name}: {command:?}");
        command.push(jobs.into());
    }
    std::fs::write(&config, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    let exports: Vec<String> = std::fs::read_dir(judging.join("exports"))
        .unwrap()
        .map(|e| format!("exports/{}", e.unwrap().file_name().to_str().unwrap()))
        .collect();
    assert_eq!(exports.len(), 2, "{exports:?}");
    // The challenge is the spec's, whose theorem is proved by `sorry`.
    let challenge = exports
        .iter()
        .find(|e| {
            std::fs::read_to_string(judging.join(e))
                .unwrap()
                .contains("sorryAx")
        })
        .unwrap();
    let solution = exports.iter().find(|e| *e != challenge).unwrap();
    let args = [
        "comparator",
        "--challenge-from-export",
        challenge,
        "--solution-from-export",
        solution,
        "--config",
        "comparator/1.json",
    ];
    let ran = (h.launcher)
        .lake(SLOT, &args, &judging, Duration::from_secs(300))
        .await
        .unwrap();
    eprintln!("{}", ran.output);
    assert_eq!(ran.code, Some(0), "{}", ran.output);
    for name in [
        "Lean paranoid",
        "con-leche",
        "con-ron",
        "lean4lean",
        "nanoda",
    ] {
        let accepts = format!("{name} kernel accepts the solution");
        assert!(ran.output.contains(&accepts), "{name}: {}", ran.output);
    }
    // In Lake's order, as `accepted_kernels` lists them.
    let at = |n: &str| ran.output.find(&format!("Running {n} kernel")).unwrap();
    let order = [
        "Lean paranoid",
        "con-leche",
        "con-ron",
        "lean4lean",
        "nanoda",
        "Lean default",
    ];
    assert!(
        order.windows(2).all(|w| at(w[0]) < at(w[1])),
        "{}",
        ran.output
    );
}

/// pisa exports a pair's modules with the targets Lake's own comparator
/// exports, in its order: as it prints them when it builds the challenge
/// itself and exports it.
#[tokio::test]
#[ignore = "real Lean and bwrap"]
async fn exports_name_the_targets_lake_would() {
    assert!(bwrap_works(), "bwrap can't sandbox here");
    let dir = tempfile::tempdir().unwrap();
    let put = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let toolchain = std::fs::read_to_string(docker().join("lean-core/lean-toolchain")).unwrap();
    put("lean-toolchain", &toolchain);
    put(
        "lakefile.toml",
        "name = \"drift\"\ndefaultTargets = [\"PisaSnap\"]\n\n\
         [[lean_lib]]\nname = \"PisaSnap\"\nglobs = [\"PisaSnap.+\"]\n",
    );
    put(
        "lake-manifest.json",
        r#"{"version": "1.3.0", "packagesDir": ".lake/packages", "packages": [],
            "name": "drift", "lakeDir": ".lake", "fixedToolchain": false}"#,
    );
    put(
        "PisaSnap/sa.lean",
        "def cost : Nat := 1\naxiom lit : 1 = 1\ntheorem t : cost = cost := rfl\n\
         theorem u : 1 = 1 := lit\n",
    );
    let configuration = serde_json::json!({
        "challenge_module": "PisaSnap.sa",
        "solution_module": "PisaSnap.sa",
        "theorem_names": ["t", "u"],
        "definition_names": ["cost"],
        "permitted_axioms": ["propext", "Quot.sound", "Classical.choice", "lit"],
    })
    .to_string();
    put("comparator.json", &configuration);
    let out = std::process::Command::new("lake")
        .args(["comparator", "--config", "comparator.json"])
        .current_dir(dir.path())
        .output()
        .expect("running lake (is elan on PATH?)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = (stdout.lines())
        .find_map(|l| l.strip_prefix("Exporting #["))
        .unwrap_or_else(|| {
            let stderr = String::from_utf8_lossy(&out.stderr);
            panic!("Lake exported nothing:\n{stdout}\n{stderr}")
        });
    let (names, module) = line.split_once("] from ").unwrap();
    assert_eq!(module, "PisaSnap.sa");
    let lakes: Vec<&str> = names.split(", ").collect();
    assert_eq!(runs::export_targets(&configuration), lakes);
}
