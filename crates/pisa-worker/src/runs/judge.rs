//! Judging a run's pairs with `lake comparator`, each in turn, on exports of
//! the modules pisa built and kernel-checked when their snapshots were
//! created: comparator builds nothing, and no code of the run runs while it
//! is judged. The exports are made with Lean's bundled `leanexport`, in the
//! slot's sandbox, and kept by what they are made of, so that a module is
//! exported once for every pair and judgment that needs it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use pisa_protocol::api::{
    AcceptPhase, Error, ErrorKind, KernelVerdict, PairError, PairErrorKind, PairPhase, PairSide,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    OUTPUT_LINES, Progress, accepted_kernels, classify, comparator_args, external_config,
    failed_kernels, hex, leaves_out_declining, run_kernels, tail, with_cap,
};
use crate::catalog::Module;
use crate::launcher::{Launcher, LeanExportError};
use crate::snapshots::link_or_copy;

/// How many exports a worker keeps.
const KEPT_EXPORTS: usize = 64;

/// The constants the kernel builds in, which Lake exports from both sides
/// of a pair, as Lake 4.36.0 lists them (`primitiveTargets`,
/// `src/lean/lake/Lake/CLI/Check.lean:474-507`).
const PRIMITIVE_TARGETS: [&str; 26] = [
    "Nat.add",
    "Nat.sub",
    "Nat.mul",
    "Nat.pow",
    "Nat.gcd",
    "Nat.div",
    "Nat.mod",
    "Nat.beq",
    "Nat.ble",
    "Nat.land",
    "Nat.lor",
    "Nat.xor",
    "Nat.shiftLeft",
    "Nat.shiftRight",
    "String.ofList",
    "Char.ofNat",
    "List",
    "eagerReduce",
    "Nat",
    "String",
    "String.mk",
    "Char",
    "optParam",
    "autoParam",
    "semiOutParam",
    "outParam",
];

/// What Lake exports besides when `Quot.sound` is permitted
/// (`builtinTargets`, `Check.lean:509-513`).
const QUOT_TARGETS: [&str; 4] = ["Quot", "Quot.mk", "Quot.lift", "Quot.ind"];

/// The modules a run's pairs are judged on.
pub struct ModulesToJudge {
    /// A directory of the judgment's own, whose `PisaSnap/` holds every
    /// module the pairs need, as
    /// [`Snapshots::link_verified`](crate::snapshots::Snapshots::link_verified)
    /// put them there: the root of the modules on the exporter's search
    /// path. The pairs are judged in it.
    pub root: PathBuf,
    /// The modules each pair's module reads, by its name, `PisaSnap.s<id>`:
    /// what it builds on, and its own, each with its build.
    pub closures: HashMap<String, Vec<Module>>,
}

/// A pair to judge.
pub struct PairToJudge {
    /// The spec snapshot's module, `PisaSnap.s<id>`.
    pub spec_module: String,
    /// The head's module.
    pub head_module: String,
    /// Comparator's configuration for the pair, as JSON, which names the
    /// two modules as they are named here.
    pub configuration: String,
    /// How many of the spec's theorems the configuration names.
    pub theorems: u64,
}

/// What comparator made of a pair.
pub struct Judgment {
    /// Why it wasn't accepted; `None` if it was.
    pub error: Option<PairError>,
    /// Comparator's exit code; `None` if it didn't run, ran out of time, or
    /// a signal killed it.
    pub exit_code: Option<i32>,
    /// The end of what comparator printed.
    pub output: String,
    pub kernels: Vec<KernelVerdict>,
    /// Where its time and memory went: its exports, then comparator.
    pub phases: Vec<PairPhase>,
}

pub struct JudgeOptions {
    /// Whether every bundled kernel checks the solution, not only Lean's.
    pub paranoid: bool,
    /// How long a pair has: the exports of its two modules and comparator.
    pub timeout: Duration,
}

/// What Lake exports of each module of a pair for `configuration`, in its
/// order (`compareIt`, `Check.lean:530-531`): `Quot` and its parts if
/// `Quot.sound` is permitted, the theorems, the permitted axioms, the
/// kernel's own constants and the definition holes.
pub fn export_targets(configuration: &str) -> Vec<String> {
    let config: Value = serde_json::from_str(configuration).unwrap_or_default();
    let names = |key: &str| -> Vec<String> {
        let names = config[key].as_array().into_iter().flatten();
        names.filter_map(Value::as_str).map(String::from).collect()
    };
    let axioms = names("permitted_axioms");
    let mut targets: Vec<String> = Vec::new();
    if axioms.iter().any(|a| a == "Quot.sound") {
        targets.extend(QUOT_TARGETS.map(String::from));
    }
    targets.extend(names("theorem_names"));
    targets.extend(axioms);
    targets.extend(PRIMITIVE_TARGETS.map(String::from));
    targets.extend(names("definition_names"));
    targets
}

/// The name an export is kept by: a hash of what it is made of, the
/// project of the pool `version`, the modules it reads (`closure`), each as
/// its build and its files' hashes, its `module` and `targets`, and the
/// `exporter`.
fn export_key(
    version: &str,
    closure: &[Module],
    module: &str,
    targets: &[String],
    exporter: &Path,
) -> String {
    let mut hash = Sha256::new();
    let mut part = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part(version.as_bytes());
    for m in closure {
        part(m.id.as_bytes());
        part(m.build.as_bytes());
        for f in &m.files {
            part(f.ext.as_bytes());
            part(f.sha256.as_bytes());
        }
    }
    part(module.as_bytes());
    for t in targets {
        part(t.as_bytes());
    }
    part(exporter.as_os_str().as_encoded_bytes());
    hex(&hash.finalize())
}

/// One export of a key at a time, in this process: judgments that need it
/// meanwhile wait for it.
fn lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;
    static LOCKS: LazyLock<Locks> = LazyLock::new(Locks::default);
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    // Those no one holds or waits for go.
    locks.retain(|_, held| Arc::strong_count(held) > 1);
    locks.entry(key.to_string()).or_default().clone()
}

/// Removes the exports kept in `dir` that were used longest ago, keeping
/// [`KEPT_EXPORTS`]. A judgment reads its own links of them, which stay.
fn evict(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut kept: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            Some((e.metadata().ok()?.modified().ok()?, e.path()))
        })
        .collect();
    kept.sort();
    let surplus = kept.len().saturating_sub(KEPT_EXPORTS);
    for (_, path) in kept.into_iter().take(surplus) {
        let _ = std::fs::remove_file(path);
    }
}

/// The exports of one judgment: in its directory, `exports/<key>`, each a
/// link of one the worker keeps or made for it, and kept for the next.
struct LeanExports<'a> {
    launcher: &'a Launcher,
    slot: usize,
    modules: &'a ModulesToJudge,
}

impl LeanExports<'_> {
    /// The export of `module`'s `targets`, as its path in the judgment's
    /// directory: one the worker keeps, or one made in at most `limit`.
    async fn export(
        &self,
        module: &str,
        targets: &[String],
        limit: Duration,
    ) -> Result<String, LeanExportError> {
        let worker = |e: std::io::Error| LeanExportError::Worker(e.to_string());
        let closure = self.modules.closures.get(module).ok_or_else(|| {
            LeanExportError::Worker(format!("no modules were linked for {module}"))
        })?;
        let exporter = self.launcher.lean_exporter().await.map_err(worker)?;
        let version = self.launcher.version();
        let key = export_key(version, closure, module, targets, &exporter);
        let name = format!("exports/{key}");
        let here = self.modules.root.join(&name);
        let kept_dir = self.launcher.exports_dir();
        let kept = kept_dir.join(&key);
        let lock = lock(&key);
        let _exporting = lock.lock().await;
        if here.exists() {
            return Ok(name);
        }
        match link_or_copy(&kept, &here) {
            Ok(()) => {
                // Used now, for `evict`.
                let used = std::fs::File::options().write(true).open(&kept);
                let _ = used.and_then(|f| f.set_modified(SystemTime::now()));
                return Ok(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(worker(e)),
        }
        let part = here.with_file_name(format!("{key}.part"));
        let output = std::fs::File::create(&part).map_err(worker)?;
        let root = &self.modules.root;
        (self.launcher)
            .lean_export(self.slot, root, module, targets, output, limit)
            .await?;
        std::fs::rename(&part, &here).map_err(worker)?;
        let keep = std::fs::create_dir_all(&kept_dir).and_then(|()| link_or_copy(&here, &kept));
        match keep {
            Ok(()) => evict(&kept_dir),
            Err(e) => tracing::warn!(module, "keeping an export failed: {e}"),
        }
        Ok(name)
    }
}

/// Why a pair has no verdict when the export of its `module` failed with
/// `error`, or the worker's error if the worker couldn't export it. `limit`
/// is the pair's time, as `timed_out` says it.
fn export_failed(
    launcher: &Launcher,
    slot: usize,
    module: &str,
    error: LeanExportError,
    limit: Duration,
) -> Result<PairError, Error> {
    let (kind, message) = match error {
        LeanExportError::Worker(why) => {
            let message = format!("exporting {module}: {why}");
            return Err(Error::new(ErrorKind::Internal, message));
        }
        LeanExportError::Timeout => (PairErrorKind::Timeout, timed_out(limit)),
        LeanExportError::OutOfMemory => (
            PairErrorKind::OutOfMemory,
            format!("the export of {module} exceeded the worker's memory limit"),
        ),
        LeanExportError::Failed(why) => (
            PairErrorKind::ExportFailed,
            format!("exporting {module} failed: {why}"),
        ),
    };
    Ok(with_cap(
        PairError { kind, message },
        launcher.memory_max_mb(slot),
    ))
}

/// What a pair that ran out of `limit` says.
fn timed_out(limit: Duration) -> String {
    format!(
        "the pair took more than {}s, its exports and comparator together",
        limit.as_secs()
    )
}

/// Judges `pairs` with `lake comparator`, each in turn, in `modules.root`,
/// on `slot`, whose memory should be free: each on the exports of its two
/// modules, `exports/<key>` there, with its configuration as
/// `comparator/<n>.json`, `n` counting from 1. `project` is the worker's,
/// whose `lean-toolchain` has elan run the project's `lake`. Each export and
/// comparator is a phase of `progress`.
pub async fn judge(
    launcher: &Launcher,
    slot: usize,
    modules: &ModulesToJudge,
    project: Option<&Path>,
    pairs: &[PairToJudge],
    options: &JudgeOptions,
    progress: &mut Progress<'_>,
) -> Result<Vec<Judgment>, Error> {
    progress.on_slot(slot);
    let dir = &modules.root;
    let internal =
        |what: &str, e: std::io::Error| Error::new(ErrorKind::Internal, format!("{what}: {e}"));
    for sub in ["comparator", "exports"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(|e| internal("writing the judgment", e))?;
    }
    if let Some(project) = project {
        std::fs::copy(project.join("lean-toolchain"), dir.join("lean-toolchain"))
            .map_err(|e| internal("copying the project's lean-toolchain", e))?;
    }
    let exports = LeanExports {
        launcher,
        slot,
        modules,
    };
    let mut judgments = Vec::new();
    for (n, pair) in pairs.iter().enumerate() {
        // Nothing for comparator to judge, which it would refuse to start.
        if pair.theorems == 0 {
            judgments.push(Judgment {
                error: Some(PairError {
                    kind: PairErrorKind::NothingToJudge,
                    message: "the spec names no theorem comparator can judge".into(),
                }),
                exit_code: None,
                output: String::new(),
                kernels: Vec::new(),
                phases: Vec::new(),
            });
            continue;
        }
        let number = n as u64 + 1;
        let deadline = Instant::now() + options.timeout;
        let targets = export_targets(&pair.configuration);
        let mut exported = Vec::new();
        let mut failed = None;
        let sides = [
            (&pair.spec_module, PairSide::Challenge),
            (&pair.head_module, PairSide::Solution),
        ];
        for (module, side) in sides {
            progress.begin(AcceptPhase::Exporting { pair: number, side });
            let left = deadline.saturating_duration_since(Instant::now());
            match progress
                .during(exports.export(module, &targets, left))
                .await
            {
                Ok(path) => exported.push(path),
                Err(e) => {
                    failed = Some(export_failed(launcher, slot, module, e, options.timeout)?);
                    break;
                }
            }
        }
        if let Some(error) = failed {
            judgments.push(Judgment {
                error: Some(error),
                exit_code: None,
                output: String::new(),
                kernels: Vec::new(),
                phases: progress.pair_done(),
            });
            continue;
        }
        let left_out = leaves_out_declining(&pair.configuration, options.paranoid);
        let text = match options.paranoid {
            true => {
                let prefix = (launcher.toolchain_prefix().await)
                    .map_err(|e| internal("finding the toolchain's kernels", e))?;
                let cap = launcher.memory_max_mb(slot).map(|(_, mb)| mb);
                external_config(&pair.configuration, &prefix.join("bin"), cap)
            }
            false => pair.configuration.clone(),
        };
        let config = format!("comparator/{}.json", n + 1);
        std::fs::write(dir.join(&config), text)
            .map_err(|e| internal("writing the configuration", e))?;
        let args = comparator_args(&config, (&exported[0], &exported[1]));
        let oom_before = launcher.oom_kills(slot);
        let left = deadline.saturating_duration_since(Instant::now());
        progress.begin(AcceptPhase::Judging { pair: number });
        let mut ran = (progress.during(launcher.lake(slot, &args, dir, left)).await)
            .map_err(|e| internal("running lake comparator", e))?;
        let phases = progress.pair_done();
        if ran.timed_out {
            ran.output.push('\n');
            ran.output.push_str(&timed_out(options.timeout));
        }
        let oom = launcher.oom_kills(slot) > oom_before;
        // From the whole output, before it is cut to its end.
        let kernels = match ran.code {
            Some(0) => accepted_kernels(options.paranoid, left_out),
            code => failed_kernels(&ran.output, code, options.paranoid, left_out, oom),
        };
        judgments.push(Judgment {
            error: classify(&ran, oom, &run_kernels(options.paranoid, left_out))
                .map(|e| with_cap(e, launcher.memory_max_mb(slot))),
            exit_code: ran.code,
            output: tail(&ran.output, OUTPUT_LINES),
            kernels,
            phases,
        });
    }
    Ok(judgments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ModuleFile;

    #[test]
    fn export_targets_follow_lakes_order() {
        let config = r#"{"challenge_module": "PisaSnap.sa", "solution_module": "PisaSnap.sb",
            "theorem_names": ["t", "u"], "definition_names": ["cost"],
            "permitted_axioms": ["propext", "Quot.sound", "Classical.choice", "lit"]}"#;
        let targets = export_targets(config);
        let mut wanted = vec!["Quot", "Quot.mk", "Quot.lift", "Quot.ind", "t", "u"];
        wanted.extend(["propext", "Quot.sound", "Classical.choice", "lit"]);
        wanted.extend(PRIMITIVE_TARGETS);
        wanted.push("cost");
        assert_eq!(targets, wanted);
        assert_eq!(targets.len(), 4 + 2 + 4 + 26 + 1);
        // Without `Quot.sound`, no `Quot`; without holes, nothing after the
        // kernel's constants.
        let config = r#"{"theorem_names": ["t"], "permitted_axioms": ["propext"]}"#;
        let targets = export_targets(config);
        assert_eq!(&targets[..2], ["t", "propext"]);
        assert_eq!(&targets[2..], PRIMITIVE_TARGETS);
    }

    #[test]
    fn the_export_key_changes_with_any_module_build_or_target() {
        let module = |id: &str, build: &str, sha: &str| Module {
            id: id.into(),
            build: build.into(),
            files: vec![ModuleFile {
                ext: "olean".into(),
                sha256: sha.into(),
            }],
        };
        let closure = vec![module("a", "b1", "h1"), module("s", "b2", "h2")];
        let targets = vec!["t".to_string(), "Nat".to_string()];
        let exporter = Path::new("/tc/bin/leanexport");
        let key = |version: &str, closure: &[Module], m: &str, targets: &[String], e: &Path| {
            export_key(version, closure, m, targets, e)
        };
        let base = key("v", &closure, "PisaSnap.ss", &targets, exporter);
        assert_eq!(base.len(), 64);
        assert_eq!(base, key("v", &closure, "PisaSnap.ss", &targets, exporter));
        let mut others = vec![
            key("w", &closure, "PisaSnap.ss", &targets, exporter),
            key("v", &closure[1..], "PisaSnap.ss", &targets, exporter),
            key("v", &closure, "PisaSnap.sa", &targets, exporter),
            key("v", &closure, "PisaSnap.ss", &targets[..1], exporter),
            key("v", &closure, "PisaSnap.ss", &targets, Path::new("/other")),
        ];
        let changed = [
            module("x", "b1", "h1"),
            module("a", "b3", "h1"),
            module("a", "b1", "h3"),
        ];
        for m in changed {
            let closure = [m, closure[1].clone()];
            others.push(key("v", &closure, "PisaSnap.ss", &targets, exporter));
        }
        let mut ext = closure.clone();
        ext[1].files[0].ext = "olean.private".into();
        others.push(key("v", &ext, "PisaSnap.ss", &targets, exporter));
        // Parts are told apart, not run together.
        let moved = ["t".to_string() + "Nat"];
        others.push(key("v", &closure, "PisaSnap.ss", &moved, exporter));
        for (n, other) in others.iter().enumerate() {
            assert_ne!(*other, base, "{n}");
        }
    }
}
