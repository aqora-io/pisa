//! A run as a Lake project, for `lake comparator` to judge apart from pisa's
//! own audit (`/snapshots/export`), and the run's acceptance: the audit and
//! comparator's verdict on the pairs it compared, stored
//! (`/snapshots/accept`).
//!
//! The project holds every module the run's spec snapshots and heads need,
//! as their code was stored, and a comparator configuration for each spec
//! snapshot and head it reaches, the pairs the audit compares: the spec's
//! module is the challenge and the head's the solution. Its Lake
//! configuration is the worker's project's, so the modules build on what they
//! were built on.
//!
//! Comparator runs on a slot of the worker, in its cgroup, and sandboxes the
//! code it judges itself. The project is kept under the worker's
//! [`runs_dir`](crate::launcher::Launcher::runs_dir), named by its modules and
//! Lake configuration, so that judging the same run again doesn't build it
//! again: only its configurations and README are written afresh.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pisa_protocol::api::{
    AcceptPair, AcceptResponse, AuditSnapshotsResponse, DEFAULT_REVIEW_LIMIT, Error, ErrorKind,
    KernelOutcome, KernelVerdict, LeftOut, LeftOutReason, PairError, PairErrorKind,
};
use pisa_protocol::snapshot;
use pisa_protocol::verify::Declaration;
use pisa_protocol::wire::{
    ExportedFile, ExportedPair, ExportedRun, SnapshotAcceptJob, SnapshotExportJob,
};
use pisa_snapshots::review::review;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::catalog::{ModuleMeta, record_fits};
use crate::launcher::Ran;
use crate::pool::Pool;
use crate::snapshots::Snapshots;

mod judge;
mod palomar;

pub use judge::{JudgeOptions, Judgment, ModulesToJudge, PairToJudge, judge};

/// How many of comparator's last lines of output a pair keeps.
const OUTPUT_LINES: usize = 40;

/// How many judged runs a worker keeps built.
const KEPT_RUNS: usize = 4;

/// The project's Lake package.
const NAME: &str = "pisa_run";

/// The axioms `#print axioms` counts a proof clean with.
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Quot.sound", "Classical.choice"];

/// The run that spec snapshots `job.against` start, with `job.allow` naming
/// what its heads may change besides the spec's holes, as a Lake project, or
/// one of its pairs as a Palomar package. `project` is the worker's, whose
/// toolchain and packages it takes.
pub async fn export(
    snapshots: &Snapshots,
    project: Option<&Path>,
    job: &SnapshotExportJob,
) -> Result<ExportedRun, Error> {
    let run = audited_export(snapshots, project, &job.against, &job.allow)
        .await?
        .1;
    match &job.palomar {
        Some(palomar) => palomar::package(&run, palomar, &job.against, &job.allow),
        None => Ok(run),
    }
}

/// [`export`], with the audit whose pairs it exports.
async fn audited_export(
    snapshots: &Snapshots,
    project: Option<&Path>,
    against: &[String],
    allow: &[String],
) -> Result<(AuditSnapshotsResponse, ExportedRun), Error> {
    let run = snapshots.reader().audit_run(against, allow).await?;
    let catalog = snapshots.catalog();
    let mut metas: HashMap<String, ModuleMeta> = HashMap::new();
    let mut record = async |m: &str| -> Result<ModuleMeta, Error> {
        if let Some(meta) = metas.get(m) {
            return Ok(meta.clone());
        }
        let meta = catalog.module(m).await?;
        metas.insert(m.to_string(), meta.clone());
        Ok(meta)
    };
    let mut needed = BTreeSet::new();
    for pair in &run.pairs {
        for m in [&pair.spec_module, &pair.head_module] {
            needed.extend(record(m).await?.modules.iter().map(|m| m.id.clone()));
        }
    }
    // The pairs of a named run are named by the run, the others by their
    // hash.
    let modules: Vec<String> = needed.iter().cloned().collect();
    let names = snapshots.reader().readable_names(&run.pairs).await?;
    let named = |m: &str| names.get(m).cloned().unwrap_or_else(|| snapshot::module(m));
    let path = |m: &str| format!("{}.lean", named(m).replace('.', "/"));
    let mut files = Vec::new();
    let mut pairs = Vec::new();
    for (n, (compared, pair)) in run.audit.comparisons.iter().zip(&run.pairs).enumerate() {
        let spec_meta = record(&pair.spec_module).await?;
        let head_meta = record(&pair.head_module).await?;
        // What the spec permits: the axioms its module, and what it builds
        // on, declare. And the definitions each side declares, which are
        // all comparator takes as holes.
        let mut assumptions = Vec::new();
        let mut spec_defs = HashSet::new();
        for m in &spec_meta.modules {
            let meta = record(&m.id).await?;
            for d in &meta.result.declarations {
                match d.kind.as_str() {
                    "axiom" => assumptions.push(d.name.clone()),
                    "def" => {
                        spec_defs.insert(d.name.clone());
                    }
                    _ => {}
                }
            }
        }
        let mut head_defs = HashSet::new();
        let mut head_axioms = HashSet::new();
        for m in &head_meta.modules {
            let meta = record(&m.id).await?;
            for d in &meta.result.declarations {
                match d.kind.as_str() {
                    "axiom" => head_axioms.insert(d.name.clone()),
                    "def" => head_defs.insert(d.name.clone()),
                    _ => false,
                };
            }
        }
        let holes: Vec<String> = pair.holes.iter().chain(allow).cloned().collect();
        let (challenge, solution) = (named(&pair.spec_module), named(&pair.head_module));
        let named = config(
            &pair.spec_module,
            (&challenge, &solution),
            &spec_meta.result.declarations,
            &holes,
            &assumptions,
            (&spec_defs, &head_defs),
            &head_axioms,
        );
        let path = format!("comparator/{}.json", n + 1);
        files.push(file(&path, named.text));
        pairs.push(ExportedPair {
            spec: compared.spec.clone(),
            head: compared.head.clone(),
            spec_module: snapshot::module(&pair.spec_module),
            head_module: snapshot::module(&pair.head_module),
            challenge_module: challenge,
            solution_module: solution,
            config: path,
            theorems: named.theorems,
            left_out: named.left_out,
        });
    }
    for m in &needed {
        let code = record(m).await?.code;
        files.push(file(&path(m), snapshot::rename_imports(&code, &names)));
    }
    // The snapshots on each module, for the README's table.
    let mut on: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (entry, entry_meta) in catalog.entries_on(&modules, &[]).await? {
        let labels: Vec<String> = entry_meta
            .labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        on.entry(entry_meta.module)
            .or_default()
            .push((snapshots.id(&entry), labels.join(", ")));
    }
    let mut roots: Vec<&str> = (modules.iter())
        .map(|m| match names.get(m) {
            Some(name) => name.split('.').next().unwrap_or(snapshot::ROOT),
            None => snapshot::ROOT,
        })
        .collect();
    roots.sort();
    roots.dedup();
    let project_files = project_files(project, &roots)?;
    let toolchain = project_files
        .iter()
        .find(|f| f.path == "lean-toolchain")
        .map(|f| f.text.trim().to_string());
    let mathlib = project_files
        .iter()
        .any(|f| f.path == "lake-manifest.json" && f.text.contains("\"name\": \"mathlib\""));
    files.extend(project_files);
    let table: Vec<(String, Vec<(String, String)>)> = modules
        .iter()
        .map(|m| (path(m), on.remove(m).unwrap_or_default()))
        .collect();
    let about = About {
        toolchain,
        mathlib,
        files: table,
    };
    files.push(file("README.md", readme(&pairs, &about)));
    let forks = run.audit.forks.clone();
    Ok((
        run.audit,
        ExportedRun {
            files,
            pairs,
            forks,
        },
    ))
}

/// Judges the run `job` names: audits it, and judges the pairs the audit
/// compared with `lake comparator`, each in turn, in one export, so that
/// Lake builds each module once. Stores the result and returns it.
pub async fn accept(pool: &Arc<Pool>, job: SnapshotAcceptJob) -> Result<AcceptResponse, Error> {
    let project = pool.launcher().project();
    let (mut audit, run) =
        audited_export(pool.snapshots(), project, &job.against, &job.allow).await?;
    // A head that holds only through links would be judged on records that
    // read `sorry`: the run is settled first.
    if !audit.unsettled.is_empty() {
        let heads: Vec<&str> = audit.unsettled.iter().map(|u| u.head.as_str()).collect();
        let body = serde_json::json!({"against": job.against, "allow": job.allow});
        return Err(Error::new(
            ErrorKind::Conflict,
            format!(
                "the run isn't settled: {} hold only through links. Send POST /snapshots/settle \
                 {body}, then accept the run",
                heads.join(", ")
            ),
        ));
    }
    // What the leader reads beside the verdict, kept with it, read with the
    // audit: a snapshot deleted while comparator runs takes nothing from it.
    let reader = pool.snapshots().reader();
    (audit.review, audit.review_left_out) =
        review(reader, &audit.comparisons, &job.allow, DEFAULT_REVIEW_LIMIT).await?;
    let key = key(&run);
    // One judgment at a time of a run, in its directory.
    let lock = pool.snapshots().lock(&format!("run:{key}"));
    let _judging = lock.lock().await;
    let runs = pool.launcher().runs_dir();
    evict(pool, &runs, &key);
    let (sources, to_judge) = judged_run(&run);
    let modules = ModulesToJudge {
        root: runs.join(&key),
        sources,
    };
    let options = JudgeOptions {
        paranoid: job.paranoid,
        timeout: Duration::from_secs(job.timeout_secs),
        build_timeout: Duration::from_secs(job.build_timeout_secs),
    };
    let mut slot = pool.acquire("").await;
    // Comparator builds the modules, so free the slot's memory first.
    slot.retire_and_wait("evict").await;
    let judgments = judge(
        pool.launcher(),
        slot.id,
        &modules,
        project,
        &to_judge,
        &options,
    )
    .await?;
    drop(slot);
    let pairs: Vec<AcceptPair> = (run.pairs.into_iter().zip(judgments))
        .map(|(pair, judged)| AcceptPair {
            spec: pair.spec,
            head: pair.head,
            spec_module: pair.spec_module,
            head_module: pair.head_module,
            challenge_module: pair.challenge_module,
            solution_module: pair.solution_module,
            config: pair.config,
            accepted: judged.error.is_none(),
            error: judged.error,
            exit_code: judged.exit_code,
            output: judged.output,
            kernels: judged.kernels,
            theorems: pair.theorems,
            left_out: pair.left_out,
        })
        .collect();
    let clean = audit.clean == Some(true);
    let snapshots = pool.snapshots();
    let specs = (job.against.iter())
        .map(|a| snapshots.reader().parse(a))
        .collect::<Result<Vec<_>, _>>()?;
    let hex = uuid::Uuid::new_v4().simple().to_string();
    let mut accepted = AcceptResponse {
        id: format!("{}~{hex}", snapshots.reader().version()),
        created: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        against: job.against,
        allow: job.allow,
        paranoid: job.paranoid,
        accepted: clean && !pairs.is_empty() && pairs.iter().all(|p| p.accepted),
        clean,
        heads: audit.heads.clone(),
        audit,
        pairs,
        stale: false,
        truncated: Vec::new(),
    };
    fit(&mut accepted);
    snapshots
        .catalog()
        .put_acceptance(&hex, &specs, &accepted)
        .await?;
    Ok(accepted)
}

/// Fits `accepted` to a record's 6 MiB: its pairs' outputs go first, then
/// the review list, so that the verdict is kept once comparator has run.
fn fit(accepted: &mut AcceptResponse) {
    if !record_fits(accepted) {
        accepted.pairs.iter_mut().for_each(|p| p.output.clear());
        accepted.truncated.push("output".into());
    }
    if !record_fits(accepted) {
        accepted.audit.review.clear();
        accepted.truncated.push("review".into());
    }
}

/// Whether `path` is one of a run's files that don't name what is built:
/// its configurations and README, which an export with another `allow`
/// writes differently.
fn judging_only(path: &str) -> bool {
    path.starts_with("comparator/") || path == "README.md"
}

/// The name of `run`'s directory: a hash of its modules and its Lake
/// configuration, which are what Lake builds.
fn key(run: &ExportedRun) -> String {
    let mut built: Vec<&ExportedFile> = run
        .files
        .iter()
        .filter(|f| !judging_only(&f.path))
        .collect();
    built.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hash = Sha256::new();
    for f in built {
        for part in [f.path.as_bytes(), f.text.as_bytes()] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part);
        }
    }
    hex(&hash.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What [`judge`] takes of `run`: its files but the pairs' configurations,
/// and its pairs, each with its configuration.
fn judged_run(run: &ExportedRun) -> (Vec<ExportedFile>, Vec<PairToJudge>) {
    let text = |path: &str| {
        let file = run.files.iter().find(|f| f.path == path);
        file.map_or_else(String::new, |f| f.text.clone())
    };
    let pairs = (run.pairs.iter())
        .map(|pair| PairToJudge {
            spec_module: pair.spec_module.clone(),
            head_module: pair.head_module.clone(),
            configuration: text(&pair.config),
            theorems: pair.theorems,
        })
        .collect();
    let sources = (run.files.iter())
        .filter(|f| !f.path.starts_with("comparator/"))
        .cloned()
        .collect();
    (sources, pairs)
}

/// Writes a run's `files` into `dir`, with the project's packages, which are
/// built already, as its own. A new run is written beside it and moved into
/// place; a run already there gets only its configurations and README again,
/// so that Lake finds what it built.
fn write(dir: &Path, files: &[&ExportedFile], project: Option<&Path>) -> std::io::Result<()> {
    let put = |root: &Path, f: &ExportedFile| -> std::io::Result<()> {
        let path = root.join(&f.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &f.text)
    };
    if dir.is_dir() {
        let _ = std::fs::remove_dir_all(dir.join("comparator"));
        for f in files.iter().filter(|f| judging_only(&f.path)) {
            put(dir, f)?;
        }
    } else {
        let new = dir.with_extension("tmp");
        let _ = std::fs::remove_dir_all(&new);
        for f in files {
            put(&new, f)?;
        }
        let packages = project.map(|p| p.join(".lake").join("packages"));
        if let Some(packages) = packages.filter(|p| p.is_dir()) {
            std::fs::create_dir_all(new.join(".lake"))?;
            std::os::unix::fs::symlink(std::path::absolute(packages)?, new.join(".lake/packages"))?;
        }
        std::fs::rename(&new, dir)?;
    }
    // When it was last judged, for `evict`.
    std::fs::write(dir.join(".used"), [])
}

/// Removes the runs in `runs` that were judged longest ago, keeping
/// [`KEPT_RUNS`], `current` among them, and any being judged.
fn evict(pool: &Pool, runs: &Path, current: &str) {
    let Ok(entries) = std::fs::read_dir(runs) else {
        return;
    };
    let mut kept: Vec<(std::time::SystemTime, String)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().into_string().ok()?;
            let used = e.path().join(".used").metadata().ok()?.modified().ok()?;
            (name != current && !name.ends_with(".tmp")).then_some((used, name))
        })
        .collect();
    kept.sort();
    let surplus = (kept.len() + 1).saturating_sub(KEPT_RUNS);
    for (_, name) in kept.into_iter().take(surplus) {
        let lock = pool.snapshots().lock(&format!("run:{name}"));
        if let Ok(_free) = lock.try_lock() {
            let _ = std::fs::remove_dir_all(runs.join(&name));
        }
    }
}

/// The lines with which comparator says what it is doing, so that the last
/// one before an error says which step failed.
const STEPS: [&str; 4] = ["Resolving dependencies", "Building", "Exporting", "Running"];

/// The bundled kernels that check proofs with non-standard axioms, as Lake
/// names them and runs them: their executables in the toolchain's `bin`, and
/// their arguments. A configuration's `external_kernels` lists them.
const CHECKERS: [(&str, &str, &[&str]); 3] = [
    (
        "Lean paranoid",
        "leanchecker-paranoid",
        &["--silent", "--from-export"],
    ),
    ("lean4lean", "lean4lean", &["--import"]),
    ("nanoda", "nanoda_bin", &[]),
];

/// The bundled kernels that don't support non-standard axioms, which a run
/// of a spec that permits one leaves out.
const DECLINING: [&str; 2] = ["con-leche", "con-ron"];

/// Lake's name for its own kernel, which runs last.
const DEFAULT_KERNEL: &str = "Lean default";

/// Whether a comparator configuration permits an axiom beyond the standard
/// ones, which only a spec's assumptions add.
fn permits_spec_axiom(config: &str) -> bool {
    let config: Value = serde_json::from_str(config).unwrap_or_default();
    (config["permitted_axioms"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .any(|a| !STANDARD_AXIOMS.contains(&a))
}

/// Whether a pair is run with `external_kernels` and not `--paranoid`: it is
/// `paranoid`, and its `config` permits a non-standard axiom. Of the bundled
/// kernels only some support one, so `--paranoid`, which runs them all and
/// fails when one declines, would refuse a proof no kernel rejected.
fn needs_external(config: &str, paranoid: bool) -> bool {
    paranoid && permits_spec_axiom(config)
}

/// `config` with the kernels that support non-standard axioms as its
/// `external_kernels`, from the toolchain's `bin`.
fn external_config(config: &str, bin: &Path) -> String {
    let mut config: Value = serde_json::from_str(config).unwrap_or_default();
    let kernels: serde_json::Map<String, Value> = CHECKERS
        .iter()
        .map(|(name, exe, args)| {
            let command: Vec<String> = std::iter::once(bin.join(exe).display().to_string())
                .chain(args.iter().map(|a| a.to_string()))
                .collect();
            (name.to_string(), json!(command))
        })
        .collect();
    config["external_kernels"] = Value::Object(kernels);
    pretty(&config)
}

/// The arguments of `lake` for a pair's `config`: `--paranoid` runs every
/// bundled kernel, unless the config names `external` kernels instead.
fn comparator_args(config: &str, paranoid: bool, external: bool) -> Vec<&str> {
    let mut args = vec!["comparator", "--config", config];
    if paranoid && !external {
        args.push("--paranoid");
    }
    args
}

fn kernel(name: &str, verdict: KernelOutcome, reason: Option<&str>) -> KernelVerdict {
    KernelVerdict {
        name: name.to_string(),
        verdict,
        reason: reason.map(String::from),
    }
}

/// The kernels con-leche and con-ron, which a run with `external_kernels`
/// doesn't run.
fn skipped() -> impl Iterator<Item = KernelVerdict> {
    DECLINING.iter().map(|name| {
        let why = "doesn't support non-standard axioms, which the spec permits, and wasn't run";
        kernel(name, KernelOutcome::Unsupported, Some(why))
    })
}

/// The kernels of a pair comparator accepted, which it did when it exited 0:
/// all that it was set to run, from what pisa configured (`paranoid`, and
/// `external` kernels), not from its output.
fn accepted_kernels(paranoid: bool, external: bool) -> Vec<KernelVerdict> {
    let accepts = |name: &str| kernel(name, KernelOutcome::Accepted, None);
    let mut kernels = Vec::new();
    if paranoid {
        kernels.extend(CHECKERS.iter().map(|(name, ..)| accepts(name)));
        match external {
            true => kernels.extend(skipped()),
            false => kernels.extend(DECLINING.iter().map(|name| accepts(name))),
        }
    }
    kernels.push(accepts(DEFAULT_KERNEL));
    kernels
}

/// The kernels of a pair comparator didn't accept, as it printed them: a
/// kernel's `accepts`, `rejected` or `Error while interacting` line, in
/// order, which the solution's own build or a declaration's name could
/// imitate. They are for the reader and never settle a pair. The kernels an
/// `external` run left out follow its own.
fn failed_kernels(output: &str, external: bool) -> Vec<KernelVerdict> {
    let mut kernels: Vec<KernelVerdict> = output
        .lines()
        .filter_map(|line| {
            if let Some(name) = line.strip_suffix(" kernel accepts the solution") {
                return Some(kernel(name, KernelOutcome::Accepted, None));
            }
            if let Some(name) = line.strip_suffix(" kernel rejected the solution") {
                return Some(kernel(name, KernelOutcome::Rejected, None));
            }
            let name = line.strip_prefix("Error while interacting with ")?;
            Some(kernel(
                name.strip_suffix(" kernel")?,
                KernelOutcome::CannotRun,
                None,
            ))
        })
        .collect();
    if external && !kernels.is_empty() {
        let last = kernels.iter().position(|k| k.name == DEFAULT_KERNEL);
        let at = last.unwrap_or(kernels.len());
        kernels.splice(at..at, skipped());
    }
    kernels
}

/// `error`, if it is an out-of-memory, naming the limit of the slot that ran
/// out (`cap`, as [`Launcher::memory_max_mb`] gives it).
fn with_cap(mut error: PairError, cap: Option<(&'static str, u64)>) -> PairError {
    if error.kind == PairErrorKind::OutOfMemory {
        error.message.push_str(&crate::limits::cap_suffix(cap));
    }
    error
}

/// Why comparator didn't accept a pair, from how its run ended (`ran`, and
/// `oom` if the worker's memory limit killed something meanwhile); `None`
/// if it accepted it, which only its exit code says: its output can say
/// anything, as the solution prints some of it. Comparator exits 0 for a
/// pair it accepts, 2 when it can't start, and 1 for any error, a failed
/// build in its sandbox included: its last error line, and the step it was
/// on, tell them apart. That can only label a failure, never excuse one.
fn classify(ran: &Ran, oom: bool) -> Option<PairError> {
    if ran.code == Some(0) {
        return None;
    }
    let lines: Vec<&str> = ran.output.lines().collect();
    let error = lines
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("error: "))
        .map(str::trim);
    let message = || error.map_or_else(|| tail(&ran.output, 1), String::from);
    let (kind, message) = match ran.code {
        _ if oom => (
            PairErrorKind::OutOfMemory,
            "comparator exceeded the worker's memory limit".to_string(),
        ),
        None => (PairErrorKind::Timeout, ran.output.clone()),
        Some(1) => {
            let step = lines
                .iter()
                .rev()
                .find(|l| STEPS.iter().any(|s| l.starts_with(s)));
            let kind = match error {
                Some(e) if e.starts_with("Child exited with") => match step {
                    Some(s) if s.starts_with("Resolving dependencies") => PairErrorKind::CannotRun,
                    _ => PairErrorKind::BuildFailed,
                },
                Some(e) if e.starts_with("Error while interacting with") => {
                    PairErrorKind::CannotRun
                }
                Some(e) if DECLINING.iter().any(|k| e == format!("{k} exited with 2")) => {
                    PairErrorKind::Unsupported
                }
                _ => PairErrorKind::Refused,
            };
            (kind, message())
        }
        Some(_) => (PairErrorKind::CannotRun, message()),
    };
    Some(PairError { kind, message })
}

/// The last `n` lines of `text`.
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn file(path: &str, text: String) -> ExportedFile {
    ExportedFile {
        path: path.to_string(),
        text,
    }
}

/// The Lake configuration: the project's toolchain and manifest, and a
/// lakefile with its requirements.
#[doc(hidden)]
pub fn project_files(project: Option<&Path>, roots: &[&str]) -> Result<Vec<ExportedFile>, Error> {
    let Some(project) = project else {
        // As in tests, which build nothing.
        let manifest = json!({"version": "1.3.0", "packagesDir": ".lake/packages",
                              "packages": [], "name": NAME, "lakeDir": ".lake",
                              "fixedToolchain": false});
        return Ok(vec![
            file("lake-manifest.json", pretty(&manifest)),
            file("lakefile.toml", lakefile(&manifest, roots)),
        ]);
    };
    let read = |name: &str| {
        std::fs::read_to_string(project.join(name)).map_err(|e| {
            Error::new(
                ErrorKind::Internal,
                format!("reading the project's {name}: {e}"),
            )
        })
    };
    let mut manifest: Value = serde_json::from_str(&read("lake-manifest.json")?).map_err(|e| {
        Error::new(
            ErrorKind::Internal,
            format!("reading the project's lake-manifest.json: {e}"),
        )
    })?;
    manifest["name"] = NAME.into();
    Ok(vec![
        file("lean-toolchain", read("lean-toolchain")?),
        file("lakefile.toml", lakefile(&manifest, roots)),
        file("lake-manifest.json", pretty(&manifest)),
    ])
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// A TOML basic string, which JSON's escapes are valid in.
fn toml(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// A lakefile that requires what `manifest` requires directly, at the
/// revisions it names, with the snapshot modules as libraries, one for each
/// of `roots`, built with `autoImplicit` off, as the worker builds them.
fn lakefile(manifest: &Value, roots: &[&str]) -> String {
    let targets: Vec<String> = roots.iter().map(|r| toml(r)).collect();
    let mut out = format!(
        "# A pisa run, for `lake comparator`: see README.md.\nname = {}\n\
         defaultTargets = [{}]\n\n[leanOptions]\nautoImplicit = false\n",
        toml(NAME),
        targets.join(", ")
    );
    let packages = manifest["packages"].as_array().into_iter().flatten();
    for p in packages.filter(|p| p["inherited"] == false) {
        let field = |key: &str| p[key].as_str().filter(|s| !s.is_empty());
        let Some(name) = field("name") else { continue };
        out.push_str(&format!("\n[[require]]\nname = {}\n", toml(name)));
        match (field("type"), field("url"), field("dir")) {
            (Some("git"), Some(url), _) => {
                out.push_str(&format!("git = {}\n", toml(url)));
                if let Some(rev) = field("inputRev").or(field("rev")) {
                    out.push_str(&format!("rev = {}\n", toml(rev)));
                }
                if let Some(sub) = field("subDir") {
                    out.push_str(&format!("subDir = {}\n", toml(sub)));
                }
            }
            (_, _, Some(dir)) => out.push_str(&format!("path = {}\n", toml(dir))),
            _ => {}
        }
    }
    for root in roots {
        out.push_str(&format!(
            "\n[[lean_lib]]\nname = {}\nglobs = [{}]\n",
            toml(root),
            toml(&format!("{root}.+"))
        ));
    }
    out
}

/// A pair's comparator configuration, with what it names and leaves out.
struct Named {
    text: String,
    /// How many of the spec's theorems it names.
    theorems: u64,
    left_out: Vec<LeftOut>,
}

/// Comparator's configuration for the spec's module, snapshot `spec` (its
/// id), and the head's, named `modules` in the export (the challenge and the
/// solution): the spec's theorems among
/// `declarations`, `holes` as its definition holes, and the standard axioms
/// and `assumptions` as permitted. Left out, and listed with why: private
/// names, which differ between the two modules, and holes that aren't plain
/// definitions on both sides, the names in `defs` (spec's, head's), which
/// comparator would refuse as definition holes.
fn config(
    spec: &str,
    modules: (&str, &str),
    declarations: &[Declaration],
    holes: &[String],
    assumptions: &[String],
    defs: (&HashSet<String>, &HashSet<String>),
    head_axioms: &HashSet<String>,
) -> Named {
    let mut left_out: Vec<LeftOut> = Vec::new();
    let mut leave = |name: &String, reason: LeftOutReason| {
        if !left_out.iter().any(|l| l.name == *name) {
            left_out.push(LeftOut {
                name: name.clone(),
                reason,
            });
        }
    };
    let private = |name: &str| name.starts_with("_private.");
    // What comparator would refuse the pair for, though the audit doesn't.
    let hidden = private_in_statement(spec, declarations);
    let mut targets = Vec::new();
    for d in declarations {
        if d.kind != "theorem" || d.generated || d.parent.is_some() {
            continue;
        }
        if !private(&d.name) && hidden.contains(d.name.as_str()) {
            leave(&d.name, LeftOutReason::PrivateInStatement);
        } else {
            targets.push(&d.name);
        }
    }
    let mut kept_axioms: Vec<&String> = Vec::new();
    for a in assumptions {
        if !private(a) && !head_axioms.contains(a) {
            leave(a, LeftOutReason::AxiomNotInHead);
        } else {
            kept_axioms.push(a);
        }
    }
    let mut name = |names: &mut dyn Iterator<Item = &String>, holes: bool| -> Vec<String> {
        let mut kept: Vec<String> = Vec::new();
        for n in names {
            let reason = if private(n) {
                Some(LeftOutReason::Private)
            } else if holes && !defs.0.contains(n) {
                Some(LeftOutReason::NotADefinitionInSpec)
            } else if holes && !defs.1.contains(n) {
                Some(LeftOutReason::NotADefinitionInHead)
            } else {
                None
            };
            match reason {
                Some(reason) => leave(n, reason),
                None if !kept.contains(n) => kept.push(n.clone()),
                None => {}
            }
        }
        kept
    };
    let theorems = name(&mut targets.into_iter(), false);
    let holes = name(&mut holes.iter(), true);
    let standard: Vec<String> = STANDARD_AXIOMS.iter().map(|a| a.to_string()).collect();
    let axioms = name(&mut standard.iter().chain(kept_axioms), false);
    let config = json!({
        "challenge_module": modules.0,
        "solution_module": modules.1,
        "theorem_names": theorems,
        "definition_names": holes,
        "permitted_axioms": axioms,
    });
    Named {
        text: pretty(&config),
        theorems: theorems.len() as u64,
        left_out,
    }
}

/// The spec's theorems whose statement reaches a private name of the spec's
/// own module, snapshot `spec`'s as pisa built it, directly or through the
/// definitions of that module
/// that its type uses. A private name of a snapshot below is one constant on
/// both sides, so it doesn't count.
fn private_in_statement<'a>(spec: &str, declarations: &'a [Declaration]) -> HashSet<&'a str> {
    let own = format!("_private.{}.", snapshot::module(spec));
    let mut reaching: HashSet<&str> = HashSet::new();
    let reaches =
        |r: &String, reaching: &HashSet<&str>| r.starts_with(&own) || reaching.contains(r.as_str());
    loop {
        let before = reaching.len();
        for d in declarations.iter().filter(|d| d.kind != "theorem") {
            let refs = d.type_refs.iter().chain(&d.value_refs);
            if !reaching.contains(d.name.as_str()) && refs.clone().any(|r| reaches(r, &reaching)) {
                reaching.insert(&d.name);
            }
        }
        if reaching.len() == before {
            break;
        }
    }
    declarations
        .iter()
        .filter(|d| d.kind == "theorem" && d.type_refs.iter().any(|r| reaches(r, &reaching)))
        .map(|d| d.name.as_str())
        .collect()
}

/// What the README says of the project beside its pairs.
struct About {
    /// The project's `lean-toolchain`, if it has one.
    toolchain: Option<String>,
    /// Whether it requires Mathlib, whose build `lake exe cache get` fetches.
    mathlib: bool,
    /// Each module's file, with the snapshots on the module and their labels.
    files: Vec<(String, Vec<(String, String)>)>,
}

fn readme(pairs: &[ExportedPair], about: &About) -> String {
    let mut out = String::from(
        "# A pisa run\n\n\
         The run's spec snapshots and the heads that replace them, as a Lake project for \
         `lake comparator`. Each configuration in `comparator/` pairs a spec snapshot's module, \
         the challenge, with a head's, the solution.\n\n## Building it\n\n",
    );
    if let Some(toolchain) = &about.toolchain {
        out.push_str(&format!(
            "The toolchain is `{toolchain}`, in `lean-toolchain`: elan installs it.\n"
        ));
    }
    if about.mathlib {
        out.push_str(
            "\nRun `lake exe cache get` first, so that Mathlib comes built: comparator builds in a \
             sandbox without network, and would otherwise compile Mathlib from source.\n",
        );
    }
    out.push_str(
        "\n`lake build` builds every module, with `autoImplicit` off as pisa built them. pisa \
         built each under its code's hash, `PisaSnap/s<hash>.lean`. Here a pair whose spec is \
         of a named run is named by the run instead: its head `PisaRun/<name>.lean`, and its \
         spec, if it isn't the head, `PisaSpec/<name>.lean`. Other modules, those the pairs \
         import among them, keep their hash names. Each imports the others by these names, so \
         this is a renamed build: the code is pisa's, but module names, and private names \
         with them, differ from pisa's build.\n\n\
         ## Judging it\n\n\
         Judge each pair with `lake comparator --paranoid --config comparator/<n>.json`, which \
         needs `bwrap`. A pair whose spec permits an axiom of its own is the exception, as \
         con-leche and con-ron don't support one and `--paranoid` fails when they decline: pisa \
         judges it without `--paranoid`, with `external_kernels` in the configuration, \
         `\"Lean paranoid\": [\"<prefix>/bin/leanchecker-paranoid\", \"--silent\", \
         \"--from-export\"]`, `\"lean4lean\": [\"<prefix>/bin/lean4lean\", \"--import\"]` and \
         `\"nanoda\": [\"<prefix>/bin/nanoda_bin\"]`, `<prefix>` being `lean --print-prefix`. \
         Add them to such a configuration to judge it as pisa does.\n\n\
         Comparator accepts a pair when each theorem of the spec is proved in the head \
         with the same statement, the definitions those statements use are the same but for \
         the spec's holes (`definition_names`), and the proofs use no axioms but the standard \
         ones and the assumptions the spec declares (`permitted_axioms`).\n\n\
         | Configuration | Spec snapshot | Head | Theorems | Left out |\n\
         | --- | --- | --- | --- | --- |\n",
    );
    for p in pairs {
        let left_out: Vec<String> = p
            .left_out
            .iter()
            .map(|l| format!("`{}` ({})", l.name, reason_text(l.reason)))
            .collect();
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | {} | {} |\n",
            p.config,
            p.spec,
            p.head,
            p.theorems,
            left_out.join(", ")
        ));
    }
    out.push_str(
        "\nWhat comparator doesn't see, which pisa's audit compares:\n\n\
         - Private declarations, which can't be named across two modules, and holes that \
         aren't plain definitions (`def`) on both sides, which it can't take as definition \
         holes: each pair lists what it leaves out.\n\
         - Definitions that no theorem's statement uses.\n\
         - It refuses some heads that keep the spec's statements, such as code that opens \
         `Classical` in its header, or whose auxiliary declarations differ.\n\n\
         ## The files\n\n| File | Snapshots | Labels |\n| --- | --- | --- |\n",
    );
    for (path, snapshots) in &about.files {
        let ids: Vec<String> = snapshots.iter().map(|(id, _)| format!("`{id}`")).collect();
        let labels: Vec<&str> = snapshots
            .iter()
            .map(|(_, l)| l.as_str())
            .filter(|l| !l.is_empty())
            .collect();
        out.push_str(&format!(
            "| `{path}` | {} | {} |\n",
            ids.join(", "),
            labels.join("; ")
        ));
    }
    out
}

/// Why a name is left out, as the export's README says it.
fn reason_text(reason: LeftOutReason) -> &'static str {
    match reason {
        LeftOutReason::Private => "private",
        LeftOutReason::NotADefinitionInSpec => "not a definition in the spec",
        LeftOutReason::NotADefinitionInHead => "not a definition in the head",
        LeftOutReason::PrivateInStatement => "its statement uses a private name of the spec",
        LeftOutReason::AxiomNotInHead => "an axiom the head doesn't build on",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::verify::ProofStatus;

    fn decl(name: &str, kind: &str) -> Declaration {
        Declaration {
            name: name.into(),
            kind: kind.into(),
            axioms: Vec::new(),
            assumes: Vec::new(),
            status: ProofStatus::Sorry,
            type_hash: "t".into(),
            value_hash: None,
            type_refs: Vec::new(),
            value_refs: Vec::new(),
            generated: false,
            parent: None,
            negates: None,
            own_axioms: None,
            inherits: Vec::new(),
            own_status: None,
        }
    }

    #[test]
    fn the_lakefile_requires_what_the_project_requires() {
        let manifest = json!({"packages": [
            {"type": "git", "name": "Physlib", "url": "https://example.com/physlib.git",
             "rev": "20d6", "inputRev": "lean-v4.36.0-rc2", "subDir": null, "inherited": false},
            {"type": "git", "name": "mathlib", "url": "https://example.com/mathlib4.git",
             "rev": "9d58", "inputRev": "v4.36.0-rc2", "inherited": true},
            {"type": "path", "name": "local", "dir": "../local", "inherited": false},
        ]});
        assert_eq!(
            lakefile(&manifest, &["PisaRun", "PisaSnap"]),
            "# A pisa run, for `lake comparator`: see README.md.\nname = \"pisa_run\"\n\
             defaultTargets = [\"PisaRun\", \"PisaSnap\"]\n\n[leanOptions]\nautoImplicit = false\n\n\
             [[require]]\nname = \"Physlib\"\ngit = \"https://example.com/physlib.git\"\n\
             rev = \"lean-v4.36.0-rc2\"\n\n\
             [[require]]\nname = \"local\"\npath = \"../local\"\n\n\
             [[lean_lib]]\nname = \"PisaRun\"\nglobs = [\"PisaRun.+\"]\n\n\
             [[lean_lib]]\nname = \"PisaSnap\"\nglobs = [\"PisaSnap.+\"]\n"
        );
    }

    #[test]
    fn a_pair_names_the_specs_public_theorems_holes_and_assumptions() {
        let mut generated = decl("t.eq_1", "theorem");
        generated.generated = true;
        let mut derived = decl("S.mk.injEq", "theorem");
        derived.parent = Some("S".into());
        let declarations = [
            decl("cost", "def"),
            decl("t", "theorem"),
            generated,
            derived,
            decl("_private.PisaSnap.sa.0.lemma", "theorem"),
            decl("t_nonvacuous", "theorem"),
            decl("lit", "axiom"),
        ];
        let holes = [
            "cost".to_string(),
            "_private.h".into(),
            "cost".into(),
            "Gate".into(),
            "gone".into(),
        ];
        let assumptions = ["lit".to_string(), "_private.PisaSnap.sa.0.a".into()];
        let spec_defs: HashSet<String> = ["cost".to_string(), "gone".into()].into();
        let head_defs: HashSet<String> = ["cost".to_string()].into();
        let head_axioms: HashSet<String> = ["lit".to_string()].into();
        let named = config(
            "a",
            ("PisaSnap.sa", "PisaSnap.sb"),
            &declarations,
            &holes,
            &assumptions,
            (&spec_defs, &head_defs),
            &head_axioms,
        );
        let config: Value = serde_json::from_str(&named.text).unwrap();
        assert_eq!(
            config,
            json!({
                "challenge_module": "PisaSnap.sa",
                "solution_module": "PisaSnap.sb",
                "theorem_names": ["t", "t_nonvacuous"],
                "definition_names": ["cost"],
                "permitted_axioms": ["propext", "Quot.sound", "Classical.choice", "lit"],
            })
        );
        assert_eq!(named.theorems, 2);
        let left_out = |name: &str, reason| LeftOut {
            name: name.into(),
            reason,
        };
        assert_eq!(
            named.left_out,
            [
                left_out("_private.PisaSnap.sa.0.lemma", LeftOutReason::Private),
                left_out("_private.h", LeftOutReason::Private),
                // An inductive type, and a definition the head doesn't have.
                left_out("Gate", LeftOutReason::NotADefinitionInSpec),
                left_out("gone", LeftOutReason::NotADefinitionInHead),
                left_out("_private.PisaSnap.sa.0.a", LeftOutReason::Private),
            ]
        );
    }

    /// A target whose statement reaches a private name of the spec's own
    /// module, directly or through the spec's definitions, and a spec axiom
    /// the head no longer builds on, are left out and say why: comparator
    /// would refuse the pair for either. The records name the spec's private
    /// names by its hash module, whatever the export names it.
    #[test]
    fn a_pair_leaves_out_what_comparator_cant_judge() {
        let using = |name: &str, kind: &str, type_refs: &[&str], value_refs: &[&str]| Declaration {
            type_refs: type_refs.iter().map(|r| r.to_string()).collect(),
            value_refs: value_refs.iter().map(|r| r.to_string()).collect(),
            ..decl(name, kind)
        };
        let h = "_private.PisaSnap.sa.0.h";
        let declarations = [
            using(h, "def", &[], &[]),
            using("f", "def", &[], &[h]),
            using("t", "theorem", &["f"], &[]),
            using("u", "theorem", &[h], &[]),
            // A snapshot below's private name is the same on both sides.
            using("v", "theorem", &["_private.PisaSnap.sz.0.g"], &[]),
            using("w", "theorem", &["Nat"], &[]),
        ];
        let assumptions = ["lit".to_string(), "dropped".into()];
        let none = HashSet::new();
        let head_axioms: HashSet<String> = ["lit".to_string()].into();
        let named = config(
            "a",
            ("PisaSpec.p", "PisaRun.p"),
            &declarations,
            &[],
            &assumptions,
            (&none, &none),
            &head_axioms,
        );
        let config: Value = serde_json::from_str(&named.text).unwrap();
        assert_eq!(config["theorem_names"], json!(["v", "w"]));
        assert_eq!(
            config["permitted_axioms"],
            json!(["propext", "Quot.sound", "Classical.choice", "lit"])
        );
        let left_out = |name: &str, reason| LeftOut {
            name: name.into(),
            reason,
        };
        assert_eq!(
            named.left_out,
            [
                left_out("t", LeftOutReason::PrivateInStatement),
                left_out("u", LeftOutReason::PrivateInStatement),
                left_out("dropped", LeftOutReason::AxiomNotInHead),
            ]
        );
    }

    /// An acceptance too large to store loses its pairs' outputs, then its
    /// review list, and says so; the verdict stays.
    #[test]
    fn an_acceptance_too_large_drops_outputs_then_the_review() {
        let pair = |output: String| AcceptPair {
            spec: "v~s".into(),
            head: "v~h".into(),
            spec_module: "PisaSnap.ss".into(),
            head_module: "PisaSnap.sh".into(),
            challenge_module: "PisaSnap.ss".into(),
            solution_module: "PisaSnap.sh".into(),
            config: "comparator/1.json".into(),
            accepted: true,
            error: None,
            exit_code: Some(0),
            output,
            kernels: Vec::new(),
            theorems: 1,
            left_out: Vec::new(),
        };
        let accepted = |output: String| AcceptResponse {
            id: "v~a".into(),
            created: 0,
            against: vec!["v~s".into()],
            allow: Vec::new(),
            paranoid: true,
            accepted: true,
            clean: true,
            heads: vec!["v~h".into()],
            audit: AuditSnapshotsResponse::default(),
            pairs: vec![pair(output)],
            stale: false,
            truncated: Vec::new(),
        };
        let mut small = accepted("ok".into());
        fit(&mut small);
        assert!(small.truncated.is_empty());
        let mut large = accepted("x".repeat(7 << 20));
        fit(&mut large);
        assert_eq!(large.truncated, ["output"]);
        assert_eq!((large.pairs[0].output.as_str(), large.accepted), ("", true));
    }

    /// What comparator printed, as Lake 4.36.0-rc2's `CLI/Check.lean`,
    /// `Check/Compare.lean` and `Check/Axioms.lean` print it.
    fn ran(code: Option<i32>, output: &str) -> Ran {
        Ran {
            code,
            output: output.into(),
        }
    }

    fn kind(ran: &Ran, oom: bool) -> Option<PairErrorKind> {
        classify(ran, oom).map(|e| e.kind)
    }

    /// What `lake comparator --paranoid` printed for the 882 run, in the
    /// order it was written: con-leche and con-ron decline the spec's
    /// axiom, Lean's kernel accepts.
    const DECLINED: &str = "Exporting #[Quot] from PisaRun.x\n\
        Running Lean paranoid kernel on solution\n\
        Lean paranoid kernel accepts the solution\n\
        Running con-leche kernel on solution\n\
        con-leche: not implemented yet: non-standard axiom (Erdos882.lower_bound_explicit)\n\
        con-leche kernel rejected the solution\n\
        Running con-ron kernel on solution\n\
        con-ron: declined: non-standard axiom [at axiom Erdos882.lower_bound_explicit]\n\
        con-ron kernel rejected the solution\n\
        Running Lean default kernel on solution\n\
        Lean default kernel accepts the solution\n\
        error: con-leche exited with 2\n";

    const CONFIG: &str = r#"{"challenge_module": "A", "solution_module": "B",
        "theorem_names": ["t"], "permitted_axioms": ["propext", "Quot.sound",
        "Classical.choice"]}"#;
    const CONFIG_WITH_AXIOM: &str = r#"{"challenge_module": "A", "solution_module": "B",
        "theorem_names": ["t"], "permitted_axioms": ["propext", "Quot.sound",
        "Classical.choice", "lit"]}"#;

    /// Kernels that don't support a spec's axiom aren't run: the pair runs
    /// with the ones that do as `external_kernels`, without `--paranoid`,
    /// exactly when it is paranoid and the spec permits one.
    #[test]
    fn a_spec_axiom_swaps_paranoid_for_the_kernels_that_support_it() {
        let bin = Path::new("/tc/bin");
        for (config, paranoid, external) in [
            (CONFIG_WITH_AXIOM, true, true),
            (CONFIG_WITH_AXIOM, false, false),
            (CONFIG, true, false),
            (CONFIG, false, false),
        ] {
            assert_eq!(
                needs_external(config, paranoid),
                external,
                "{config} {paranoid}"
            );
            let args = comparator_args("comparator/1.json", paranoid, external);
            let mut expected = vec!["comparator", "--config", "comparator/1.json"];
            if paranoid && !external {
                expected.push("--paranoid");
            }
            assert_eq!(args, expected);
        }
        let text = external_config(CONFIG_WITH_AXIOM, bin);
        let config: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            config["external_kernels"],
            json!({
                "Lean paranoid": ["/tc/bin/leanchecker-paranoid", "--silent", "--from-export"],
                "lean4lean": ["/tc/bin/lean4lean", "--import"],
                "nanoda": ["/tc/bin/nanoda_bin"],
            })
        );
        assert_eq!(config["enable_nanoda"], Value::Null);
        assert_eq!(config["permitted_axioms"][3], "lit");
        let plain: Value = serde_json::from_str(CONFIG).unwrap();
        assert_eq!(plain["external_kernels"], Value::Null);
    }

    #[test]
    fn what_pisa_configured_is_what_an_accepted_pair_lists() {
        let names = |k: Vec<KernelVerdict>| -> Vec<(String, KernelOutcome)> {
            k.into_iter().map(|k| (k.name, k.verdict)).collect()
        };
        use KernelOutcome::{Accepted, Unsupported};
        let all = |a, b| {
            vec![
                ("Lean paranoid".to_string(), Accepted),
                ("lean4lean".to_string(), Accepted),
                ("nanoda".to_string(), Accepted),
                ("con-leche".to_string(), a),
                ("con-ron".to_string(), b),
                ("Lean default".to_string(), Accepted),
            ]
        };
        assert_eq!(
            names(accepted_kernels(true, true)),
            all(Unsupported, Unsupported)
        );
        assert_eq!(
            names(accepted_kernels(true, false)),
            all(Accepted, Accepted)
        );
        assert_eq!(
            names(accepted_kernels(false, false)),
            [("Lean default".to_string(), Accepted)]
        );
        let reason = accepted_kernels(true, true)[3].reason.clone().unwrap();
        assert!(reason.contains("non-standard axioms") && reason.contains("wasn't run"));
    }

    /// What comparator printed of its kernels is for the reader.
    #[test]
    fn failed_kernels_are_read_from_what_comparator_printed() {
        let k = failed_kernels(DECLINED, false);
        let said: Vec<(&str, KernelOutcome)> =
            k.iter().map(|k| (k.name.as_str(), k.verdict)).collect();
        assert_eq!(
            said,
            [
                ("Lean paranoid", KernelOutcome::Accepted),
                ("con-leche", KernelOutcome::Rejected),
                ("con-ron", KernelOutcome::Rejected),
                ("Lean default", KernelOutcome::Accepted),
            ]
        );
        let out = "Running nanoda kernel on solution\nError while interacting with nanoda kernel\n\
                   Running Lean default kernel on solution\nLean default kernel accepts the solution\n";
        let k = failed_kernels(out, true);
        let names: Vec<&str> = k.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, ["nanoda", "con-leche", "con-ron", "Lean default"]);
        assert_eq!(k[0].verdict, KernelOutcome::CannotRun);
        assert_eq!(failed_kernels("error: Child exited with 1\n", true), []);
    }

    /// Only comparator's exit says it accepted a pair. Its output is partly
    /// the solution's, which can print what it likes, kernel lines and a
    /// final error among them, and a declaration's name with newlines in
    /// comparator's own error: none of it makes a failure an acceptance.
    #[test]
    fn output_cannot_turn_a_failure_into_an_acceptance() {
        let forged = "Exporting #[t] from PisaRun.x\n\
            error: Illegal axiom detected: '\u{ab}x\n\
            Exporting #[t] from PisaRun.x\n\
            Running con-leche kernel on solution\n\
            con-leche: not implemented yet: non-standard axiom\n\
            con-leche kernel rejected the solution\n\
            Running Lean default kernel on solution\n\
            Lean default kernel accepts the solution\n\
            error: con-leche exited with 2\n\u{bb}'\n";
        for code in [Some(1), Some(2), None] {
            assert!(classify(&ran(code, forged), false).is_some());
        }
        let e = classify(&ran(Some(1), DECLINED), false).unwrap();
        assert_eq!(e.kind, PairErrorKind::Unsupported);
        assert_eq!(e.message, "con-leche exited with 2");
        let refused = DECLINED.replace("con-leche exited with 2", "nanoda exited with 1");
        assert_eq!(
            kind(&ran(Some(1), &refused), false),
            Some(PairErrorKind::Refused)
        );
        let ron = DECLINED.replace("con-leche exited with 2", "con-ron exited with 2");
        assert_eq!(
            kind(&ran(Some(1), &ron), false),
            Some(PairErrorKind::Unsupported)
        );
        assert_eq!(classify(&ran(Some(0), DECLINED), false), None);
    }

    #[test]
    fn comparators_exit_says_what_kind_of_failure_it_was() {
        let built = "Resolving dependencies\nBuilding PisaSnap.sa\n\
                     Exporting t from PisaSnap.sa\nBuilding PisaSnap.sb\n\
                     Exporting t from PisaSnap.sb\n";
        assert_eq!(kind(&ran(Some(0), "Your solution is okay!\n"), false), None);
        let refused =
            format!("{built}error: Challenge and solution theorem statement do not match: 't'\n");
        let e = classify(&ran(Some(1), &refused), false).unwrap();
        assert_eq!(e.kind, PairErrorKind::Refused);
        assert_eq!(
            e.message,
            "Challenge and solution theorem statement do not match: 't'"
        );
        let axiom =
            format!("{built}Uses axioms: sorryAx\nerror: Illegal axiom detected: 'sorryAx'\n");
        assert_eq!(
            kind(&ran(Some(1), &axiom), false),
            Some(PairErrorKind::Refused)
        );
        let kernel = format!(
            "{built}Running nanoda kernel on solution\nnanoda kernel rejected the solution\n\
             error: nanoda exited with 1\n"
        );
        assert_eq!(
            kind(&ran(Some(1), &kernel), false),
            Some(PairErrorKind::Refused)
        );
        let unbuilt = "Resolving dependencies\nBuilding PisaSnap.sa\nerror: Child exited with 1\n";
        assert_eq!(
            kind(&ran(Some(1), unbuilt), false),
            Some(PairErrorKind::BuildFailed)
        );
        let unresolved = "Resolving dependencies\nerror: Child exited with 1\n";
        assert_eq!(
            kind(&ran(Some(1), unresolved), false),
            Some(PairErrorKind::CannotRun)
        );
        let interacting = format!(
            "{built}Running nanoda kernel on solution\nError while interacting with nanoda kernel\n\
             error: Error while interacting with nanoda kernel: no such file\n"
        );
        assert_eq!(
            kind(&ran(Some(1), &interacting), false),
            Some(PairErrorKind::CannotRun)
        );
        let cannot = "error: `lake comparator` needs `bwrap` to sandbox the code it checks\n";
        assert_eq!(
            kind(&ran(Some(2), cannot), false),
            Some(PairErrorKind::CannotRun)
        );
        assert_eq!(
            kind(&ran(None, "lake ran for more than 1s"), false),
            Some(PairErrorKind::Timeout)
        );
        let oom = classify(&ran(Some(1), ""), true).unwrap();
        let named = with_cap(oom, Some(("PISA_MEMORY_MAX_MB", 16000)));
        assert!(
            named.message.ends_with(" (PISA_MEMORY_MAX_MB = 16000 MiB)"),
            "{}",
            named.message
        );
        let other = with_cap(
            classify(&ran(None, "slow"), false).unwrap(),
            Some(("PISA_MEMORY_MAX_MB", 1)),
        );
        assert_eq!(other.message, "slow");
        // A build the memory limit killed exits like a failed one.
        let killed = "Resolving dependencies\nBuilding PisaSnap.sa\nerror: Child exited with 137\n";
        assert_eq!(
            kind(&ran(Some(1), killed), true),
            Some(PairErrorKind::OutOfMemory)
        );
    }

    /// A run's directory is named by what Lake builds, so an export with
    /// other configurations or README, as another `allow` gives, uses it.
    #[test]
    fn a_runs_key_is_what_lake_builds() {
        let run = |files: &[(&str, &str)]| ExportedRun {
            files: files.iter().map(|(p, t)| file(p, t.to_string())).collect(),
            pairs: Vec::new(),
            forks: Vec::new(),
        };
        let one = run(&[
            ("PisaSnap/sa.lean", "theorem t : 1 = 1 := rfl"),
            ("lakefile.toml", "name = \"pisa_run\""),
            ("comparator/1.json", "{}"),
            ("README.md", "one"),
        ]);
        let judged_otherwise = run(&[
            ("README.md", "two"),
            ("lakefile.toml", "name = \"pisa_run\""),
            ("PisaSnap/sa.lean", "theorem t : 1 = 1 := rfl"),
            ("comparator/1.json", "{\"definition_names\": [\"g\"]}"),
            ("comparator/2.json", "{}"),
        ]);
        assert_eq!(key(&one), key(&judged_otherwise));
        let other_code = run(&[
            ("PisaSnap/sa.lean", "theorem t : 1 = 1 := by rfl"),
            ("lakefile.toml", "name = \"pisa_run\""),
        ]);
        assert_ne!(key(&one), key(&other_code));
        assert_eq!(key(&one).len(), 64);
    }

    /// With Mathlib, the README says to fetch its build first, as comparator
    /// builds without network.
    #[test]
    fn the_readme_says_how_to_build_the_run() {
        let about = About {
            toolchain: Some("leanprover/lean4:v4.36.0-rc2".into()),
            mathlib: true,
            files: vec![(
                "PisaSnap/sa.lean".into(),
                vec![("v~a".into(), "run=1".into())],
            )],
        };
        let pair = ExportedPair {
            spec: "v~a".into(),
            head: "v~b".into(),
            spec_module: "PisaSnap.sa".into(),
            head_module: "PisaSnap.sb".into(),
            challenge_module: "PisaSnap.sa".into(),
            solution_module: "PisaSnap.sb".into(),
            config: "comparator/1.json".into(),
            theorems: 2,
            left_out: vec![LeftOut {
                name: "S".into(),
                reason: LeftOutReason::NotADefinitionInSpec,
            }],
        };
        let text = readme(&[pair], &about);
        for wanted in [
            "`leanprover/lean4:v4.36.0-rc2`",
            "Run `lake exe cache get` first",
            "| `comparator/1.json` | `v~a` | `v~b` | 2 | `S` (not a definition in the spec) |",
            "| `PisaSnap/sa.lean` | `v~a` | run=1 |",
        ] {
            assert!(text.contains(wanted), "{wanted}: {text}");
        }
    }

    #[test]
    fn a_pair_keeps_the_end_of_the_output() {
        let text = "a\nb\nc\n";
        assert_eq!(tail(text, 2), "b\nc");
        assert_eq!(tail(text, 9), "a\nb\nc");
    }

    #[test]
    fn without_a_project_the_lakefile_requires_nothing() {
        let files = project_files(None, &["PisaSnap"]).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["lake-manifest.json", "lakefile.toml"]);
        assert!(!files[1].text.contains("[[require]]"), "{}", files[1].text);
    }
}
