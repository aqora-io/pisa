//! A run's acceptance: the audit and comparator's verdict on the pairs it
//! compared, stored (`/snapshots/accept`); and the run as a Lake project, for
//! `lake comparator` to judge outside pisa (`/snapshots/export`).
//!
//! Both start from the run as acceptance judges it: a comparator
//! configuration for each spec snapshot and head the audit compares, whose
//! module is the challenge and the head's the solution, named by their hash
//! as pisa built them. Acceptance judges those builds: it links the modules
//! the pairs need into a private directory, checking each file against its
//! record, and comparator judges each pair on exports of them, building
//! nothing (see [`judge`]). It runs on a slot of the worker, in its cgroup,
//! and sandboxes the kernels itself.
//!
//! The export holds every module the run's spec snapshots and heads need,
//! as their code was stored, and the configurations, a named run's modules
//! named by the run. Its Lake configuration is the worker's project's, so
//! the modules build on what they were built on.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
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
use pisa_snapshots::RunPair;
use pisa_snapshots::review::review;
use serde_json::{Value, json};

use crate::catalog::{ModuleMeta, record_fits};
use crate::launcher::Ran;
use crate::pool::Pool;
use crate::snapshots::Snapshots;

mod judge;
mod palomar;

pub use judge::{JudgeOptions, Judgment, ModulesToJudge, PairToJudge, export_targets, judge};

/// How many of comparator's last lines of output a pair keeps.
const OUTPUT_LINES: usize = 40;

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
    let judged = judged_run(snapshots, &job.against, &job.allow).await?;
    let run = package(snapshots, project, judged).await?;
    match &job.palomar {
        Some(palomar) => palomar::package(&run, palomar, &job.against, &job.allow),
        None => Ok(run),
    }
}

/// A run as acceptance judges it: its audit, and each pair the audit
/// compared with comparator's configuration for it, which names the
/// modules by their hash, as pisa built them.
struct JudgedRun {
    audit: AuditSnapshotsResponse,
    /// One per comparison, in the same order.
    pairs: Vec<JudgedPair>,
    /// The same, as entries and modules.
    run: Vec<RunPair>,
    /// The records of the modules the pairs need, by id.
    records: HashMap<String, ModuleMeta>,
}

struct JudgedPair {
    spec: String,
    head: String,
    /// Its configuration, as JSON.
    configuration: String,
    /// Its configuration's path in the export: `comparator/<n>.json`, `n`
    /// counting from 1.
    config: String,
    theorems: u64,
    left_out: Vec<LeftOut>,
}

/// The run that spec snapshots `against` start, with `allow` naming what its
/// heads may change besides the spec's holes, as acceptance judges it.
async fn judged_run(
    snapshots: &Snapshots,
    against: &[String],
    allow: &[String],
) -> Result<JudgedRun, Error> {
    let run = snapshots.reader().audit_run(against, allow).await?;
    let catalog = snapshots.catalog();
    let mut records: HashMap<String, ModuleMeta> = HashMap::new();
    let mut record = async |m: &str| -> Result<ModuleMeta, Error> {
        if let Some(meta) = records.get(m) {
            return Ok(meta.clone());
        }
        let meta = catalog.module(m).await?;
        records.insert(m.to_string(), meta.clone());
        Ok(meta)
    };
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
        let (challenge, solution) = (
            snapshot::module(&pair.spec_module),
            snapshot::module(&pair.head_module),
        );
        let named = config(
            &pair.spec_module,
            (&challenge, &solution),
            &spec_meta.result.declarations,
            &holes,
            &assumptions,
            (&spec_defs, &head_defs),
            &head_axioms,
        );
        pairs.push(JudgedPair {
            spec: compared.spec.clone(),
            head: compared.head.clone(),
            configuration: named.text,
            config: format!("comparator/{}.json", n + 1),
            theorems: named.theorems,
            left_out: named.left_out,
        });
    }
    Ok(JudgedRun {
        audit: run.audit,
        pairs,
        run: run.pairs,
        records,
    })
}

/// `judged` as a Lake project, for `lake comparator` outside pisa: the
/// pairs of a named run named by the run, and the modules they import by
/// those names.
async fn package(
    snapshots: &Snapshots,
    project: Option<&Path>,
    judged: JudgedRun,
) -> Result<ExportedRun, Error> {
    let mut needed = BTreeSet::new();
    for pair in &judged.run {
        for m in [&pair.spec_module, &pair.head_module] {
            let meta = &judged.records[m.as_str()];
            needed.extend(meta.modules.iter().map(|m| m.id.clone()));
        }
    }
    // The pairs of a named run are named by the run, the others by their
    // hash.
    let modules: Vec<String> = needed.iter().cloned().collect();
    let names = snapshots.reader().readable_names(&judged.run).await?;
    let named = |m: &str| names.get(m).cloned().unwrap_or_else(|| snapshot::module(m));
    let path = |m: &str| format!("{}.lean", named(m).replace('.', "/"));
    let mut files = Vec::new();
    let mut pairs = Vec::new();
    for (pair, modules) in judged.pairs.into_iter().zip(&judged.run) {
        let (challenge, solution) = (named(&modules.spec_module), named(&modules.head_module));
        let mut config: Value = serde_json::from_str(&pair.configuration).map_err(|e| {
            Error::new(ErrorKind::Internal, format!("reading a configuration: {e}"))
        })?;
        config["challenge_module"] = challenge.clone().into();
        config["solution_module"] = solution.clone().into();
        files.push(file(&pair.config, pretty(&config)));
        pairs.push(ExportedPair {
            spec: pair.spec,
            head: pair.head,
            spec_module: snapshot::module(&modules.spec_module),
            head_module: snapshot::module(&modules.head_module),
            challenge_module: challenge,
            solution_module: solution,
            config: pair.config,
            theorems: pair.theorems,
            left_out: pair.left_out,
        });
    }
    for m in &needed {
        let code = &judged.records[m.as_str()].code;
        files.push(file(&path(m), snapshot::rename_imports(code, &names)));
    }
    // The snapshots on each module, for the README's table.
    let mut on: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (entry, entry_meta) in snapshots.catalog().entries_on(&modules, &[]).await? {
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
    Ok(ExportedRun {
        files,
        pairs,
        forks: judged.audit.forks,
    })
}

/// A judgment's private directory, removed when the judgment is done.
struct Judging(PathBuf);

impl Drop for Judging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Judges the run `job` names: audits it, and judges the pairs the audit
/// compared with `lake comparator`, each in turn, on exports of the modules
/// pisa built. Stores the result and returns it.
pub async fn accept(pool: &Arc<Pool>, job: SnapshotAcceptJob) -> Result<AcceptResponse, Error> {
    let project = pool.launcher().project();
    let snapshots = pool.snapshots();
    let run = judged_run(snapshots, &job.against, &job.allow).await?;
    let mut audit = run.audit;
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
    let reader = snapshots.reader();
    (audit.review, audit.review_left_out) =
        review(reader, &audit.comparisons, &job.allow, DEFAULT_REVIEW_LIMIT).await?;
    // The modules the pairs need, linked before the judgment waits for a
    // slot: a snapshot deleted after takes nothing from it.
    let judging =
        Judging((pool.launcher().judging_dir()).join(uuid::Uuid::new_v4().simple().to_string()));
    let mut ids: Vec<String> = Vec::new();
    for pair in &run.run {
        for m in [&pair.spec_module, &pair.head_module] {
            if !ids.contains(m) {
                ids.push(m.clone());
            }
        }
    }
    let closures = snapshots.link_verified(&ids, &judging.0).await?;
    let modules = ModulesToJudge {
        root: judging.0.clone(),
        closures: (closures.into_iter())
            .map(|(id, closure)| (snapshot::module(&id), closure))
            .collect(),
    };
    let to_judge: Vec<PairToJudge> = (run.pairs.iter().zip(&run.run))
        .map(|(pair, modules)| PairToJudge {
            spec_module: snapshot::module(&modules.spec_module),
            head_module: snapshot::module(&modules.head_module),
            configuration: pair.configuration.clone(),
            theorems: pair.theorems,
        })
        .collect();
    let options = JudgeOptions {
        paranoid: job.paranoid,
        timeout: Duration::from_secs(job.timeout_secs),
    };
    let mut slot = pool.acquire("").await;
    // The exporter and the kernels load the modules, so free the slot's
    // memory first.
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
    drop(judging);
    let pairs: Vec<AcceptPair> = (run.pairs.into_iter().zip(to_judge).zip(judgments))
        .map(|((pair, judged_as), judged)| AcceptPair {
            spec: pair.spec,
            head: pair.head,
            challenge_module: judged_as.spec_module.clone(),
            solution_module: judged_as.head_module.clone(),
            spec_module: judged_as.spec_module,
            head_module: judged_as.head_module,
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The kernels a paranoid pair runs, as Lake names them and runs them, sorted
/// as it runs them: their executables in the toolchain's `bin`, and their
/// arguments. A configuration's `external_kernels` lists them, as
/// `--paranoid`, which names the same five, can't bound their workers.
const KERNELS: [(&str, &str, &[&str]); 5] = [
    (
        "Lean paranoid",
        "leanchecker-paranoid",
        &["--silent", "--from-export"],
    ),
    ("con-leche", "con-leche", &[]),
    ("con-ron", "con-ron", &[]),
    ("lean4lean", "lean4lean", &["--import"]),
    ("nanoda", "nanoda_bin", &[]),
];

/// The bundled kernels that don't support non-standard axioms, which a run
/// of a spec that permits one leaves out.
const DECLINING: [&str; 2] = ["con-leche", "con-ron"];

/// What a parallel worker of a kernel takes of memory, in MiB, as measured
/// on the largest package: con-ron's and con-leche's, which default to a
/// worker for each core.
const WORKER_MB: [(&str, u64); 2] = [("con-leche", 3790), ("con-ron", 6250)];

/// What a kernel run takes beside its workers, in MiB.
const KERNEL_BASE_MB: u64 = 3072;

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

/// Whether a paranoid pair leaves out the kernels that decline a spec's
/// non-standard axiom: `--paranoid` would run them and fail when they
/// decline, refusing a proof no kernel rejected.
fn leaves_out_declining(config: &str, paranoid: bool) -> bool {
    paranoid && permits_spec_axiom(config)
}

/// How many workers of a kernel fit in a slot of `cap_mb`, at `per_worker_mb`
/// each, after the base: at least one.
fn kernel_jobs(cap_mb: u64, per_worker_mb: u64) -> u64 {
    (cap_mb.saturating_sub(KERNEL_BASE_MB) / per_worker_mb).max(1)
}

/// `config` with the kernels as its `external_kernels`, from the toolchain's
/// `bin`, without those that decline a spec's axiom if it permits one. With
/// the slot's memory `cap_mb`, con-leche and con-ron get as many workers as
/// fit in it: as a single `--jobs=N`, as `--jobs N` exits 3.
fn external_config(config: &str, bin: &Path, cap_mb: Option<u64>) -> String {
    let mut config: Value = serde_json::from_str(config).unwrap_or_default();
    let declining = permits_spec_axiom(&config.to_string());
    let kernels: serde_json::Map<String, Value> = KERNELS
        .iter()
        .filter(|(name, ..)| !(declining && DECLINING.contains(name)))
        .map(|(name, exe, args)| {
            let jobs = WORKER_MB.iter().find(|(n, _)| n == name).zip(cap_mb);
            let command: Vec<String> = std::iter::once(bin.join(exe).display().to_string())
                .chain(args.iter().map(|a| a.to_string()))
                .chain(jobs.map(|((_, mb), cap)| format!("--jobs={}", kernel_jobs(cap, *mb))))
                .collect();
            (name.to_string(), json!(command))
        })
        .collect();
    config["external_kernels"] = Value::Object(kernels);
    pretty(&config)
}

/// The arguments of `lake` for a pair's `config`, judged on `exports` of
/// its challenge and solution. Never `--paranoid`: a paranoid pair's config
/// names its kernels.
fn comparator_args<'a>(config: &'a str, exports: (&'a str, &'a str)) -> Vec<&'a str> {
    vec![
        "comparator",
        "--challenge-from-export",
        exports.0,
        "--solution-from-export",
        exports.1,
        "--config",
        config,
    ]
}

fn kernel(name: &str, verdict: KernelOutcome, reason: Option<&str>) -> KernelVerdict {
    KernelVerdict {
        name: name.to_string(),
        verdict,
        reason: reason.map(String::from),
    }
}

/// The kernel con-leche or con-ron, which a run of a spec axiom doesn't run.
fn skipped(name: &str) -> KernelVerdict {
    let why = "doesn't support non-standard axioms, which the spec permits, and wasn't run";
    kernel(name, KernelOutcome::Unsupported, Some(why))
}

/// The kernels of a pair comparator accepted, which it did when it exited 0:
/// all that it was set to run, in Lake's order, from what pisa configured
/// (`paranoid`, and a spec axiom that leaves out `declining` kernels), not
/// from its output.
fn accepted_kernels(paranoid: bool, left_out: bool) -> Vec<KernelVerdict> {
    let mut kernels = Vec::new();
    if paranoid {
        kernels.extend(KERNELS.iter().map(|(name, ..)| match left_out {
            true if DECLINING.contains(name) => skipped(name),
            _ => kernel(name, KernelOutcome::Accepted, None),
        }));
    }
    kernels.push(kernel(DEFAULT_KERNEL, KernelOutcome::Accepted, None));
    kernels
}

/// The kernels of a pair comparator didn't accept, as it printed them: a
/// kernel's `accepts`, `rejected` or `Error while interacting` line, in
/// order, which the solution's own build or a declaration's name could
/// imitate. They are for the reader and never settle a pair. The kernels a
/// spec axiom left out follow its own.
fn failed_kernels(output: &str, left_out: bool) -> Vec<KernelVerdict> {
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
    if left_out && !kernels.is_empty() {
        let last = kernels.iter().position(|k| k.name == DEFAULT_KERNEL);
        let at = last.unwrap_or(kernels.len());
        kernels.splice(at..at, DECLINING.iter().map(|name| skipped(name)));
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
/// anything, as a kernel prints some of it and a declaration's name is in
/// its errors. Comparator exits 0 for a pair it accepts, 2 when it can't
/// start, and 1 for any error: its last error line tells them apart. That
/// can only label a failure, never excuse one.
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
            let kind = match error {
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
fn project_files(project: Option<&Path>, roots: &[&str]) -> Result<Vec<ExportedFile>, Error> {
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

    /// A paranoid pair names all five kernels in `external_kernels` and never
    /// passes `--paranoid`, which can't bound their workers; a spec axiom
    /// leaves out the two that decline it.
    #[test]
    fn paranoid_always_names_its_kernels() {
        let bin = Path::new("/tc/bin");
        let args = comparator_args("comparator/1.json", ("c", "s"));
        let mut expected = vec!["comparator", "--challenge-from-export", "c"];
        expected.extend(["--solution-from-export", "s"]);
        expected.extend(["--config", "comparator/1.json"]);
        assert_eq!(args, expected);
        let kernels = |config: &str| -> Value {
            let text = external_config(config, bin, None);
            serde_json::from_str::<Value>(&text).unwrap()["external_kernels"].clone()
        };
        assert_eq!(
            kernels(CONFIG),
            json!({
                "Lean paranoid": ["/tc/bin/leanchecker-paranoid", "--silent", "--from-export"],
                "con-leche": ["/tc/bin/con-leche"],
                "con-ron": ["/tc/bin/con-ron"],
                "lean4lean": ["/tc/bin/lean4lean", "--import"],
                "nanoda": ["/tc/bin/nanoda_bin"],
            })
        );
        assert_eq!(
            kernels(CONFIG_WITH_AXIOM),
            json!({
                "Lean paranoid": ["/tc/bin/leanchecker-paranoid", "--silent", "--from-export"],
                "lean4lean": ["/tc/bin/lean4lean", "--import"],
                "nanoda": ["/tc/bin/nanoda_bin"],
            })
        );
        let text = external_config(CONFIG_WITH_AXIOM, bin, None);
        let config: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(config["enable_nanoda"], Value::Null);
        assert_eq!(config["permitted_axioms"][3], "lit");
        let names: Vec<&str> = KERNELS.iter().map(|k| k.0).collect();
        assert!(names.is_sorted(), "Lake runs them sorted: {names:?}");
        assert!(names.iter().all(|n| !n.contains("noda") || *n == "nanoda"));
    }

    /// con-ron's and con-leche's workers are bounded by the slot's memory:
    /// what is left after the base, over a worker's, at least one.
    #[test]
    fn jobs_follow_the_slots_memory_cap() {
        assert_eq!(kernel_jobs(16000, 6250), 2);
        assert_eq!(kernel_jobs(16000, 3790), 3);
        assert_eq!(kernel_jobs(4000, 6250), 1);
        assert_eq!(kernel_jobs(4000, 3790), 1);
        assert_eq!(kernel_jobs(100, 3790), 1);
        let jobs = |cap| -> Value {
            let text = external_config(CONFIG, Path::new("/b"), cap);
            serde_json::from_str::<Value>(&text).unwrap()["external_kernels"].clone()
        };
        let capped = jobs(Some(16000));
        assert_eq!(capped["con-ron"], json!(["/b/con-ron", "--jobs=2"]));
        assert_eq!(capped["con-leche"], json!(["/b/con-leche", "--jobs=3"]));
        assert_eq!(capped["nanoda"], json!(["/b/nanoda_bin"]));
        assert_eq!(
            jobs(Some(4000))["con-ron"],
            json!(["/b/con-ron", "--jobs=1"])
        );
        assert_eq!(jobs(None)["con-ron"], json!(["/b/con-ron"]));
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
                ("con-leche".to_string(), a),
                ("con-ron".to_string(), b),
                ("lean4lean".to_string(), Accepted),
                ("nanoda".to_string(), Accepted),
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
        let reason = accepted_kernels(true, true)[1].reason.clone().unwrap();
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
