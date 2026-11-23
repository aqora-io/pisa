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
    AcceptPair, AcceptPhase, AcceptResponse, AuditSnapshotsResponse, DEFAULT_REVIEW_LIMIT, Error,
    ErrorKind, KernelOutcome, KernelVerdict, LeftOut, LeftOutReason, PairError, PairErrorKind,
};
use pisa_protocol::header::{import_parts, split_header};
use pisa_protocol::snapshot;
use pisa_protocol::verify::Declaration;
use pisa_protocol::wire::{
    ExportedFile, ExportedPair, ExportedRun, SnapshotAcceptJob, SnapshotExportJob,
};
use pisa_snapshots::RunPair;
use pisa_snapshots::review::review;
use serde_json::{Value, json};

use crate::catalog::{Acceptance, ModuleMeta, record_fits};
use crate::launcher::{Launcher, Ran};
use crate::pool::{Lease, Pool};
use crate::snapshots::Snapshots;

mod judge;
mod palomar;
mod progress;

pub use judge::{JudgeOptions, Judgment, ModulesToJudge, PairToJudge, export_targets, judge};
pub use progress::{Progress, ProgressSender};

/// How many of comparator's last lines of output a pair keeps.
const OUTPUT_LINES: usize = 40;

/// The project's Lake package.
const NAME: &str = "pisa_run";

/// The axioms `#print axioms` counts a proof clean with.
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Quot.sound", "Classical.choice"];

/// Where the worker's project's own code comes from: its git repository and
/// the revision it is at (`PISA_PROJECT_GIT`, `PISA_PROJECT_REV`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSource {
    pub git: String,
    pub rev: String,
}

/// The run that spec snapshots `job.against` start, with `job.allow` naming
/// what its heads may change besides the spec's holes, as a Lake project, or
/// one of its pairs as a Palomar package. `project` is the worker's, whose
/// toolchain and packages it takes, and `source` where its own code comes
/// from, which an export whose modules import the project's own libraries
/// needs.
pub async fn export(
    snapshots: &Snapshots,
    project: Option<&Path>,
    source: Option<&ProjectSource>,
    job: &SnapshotExportJob,
) -> Result<ExportedRun, Error> {
    let judged = judged_run(snapshots, &job.against, &job.allow).await?;
    let run = package(snapshots, project, source, judged).await?;
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
    source: Option<&ProjectSource>,
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
    // The project's own libraries the modules import, which only the
    // project itself provides.
    let codes = needed
        .iter()
        .map(|m| judged.records[m.as_str()].code.as_str());
    let own = project.map(own_libraries).unwrap_or_default();
    let imported: BTreeSet<String> = imported_roots(codes).intersection(&own).cloned().collect();
    let project_files = project_files(project, &roots, &imported, source)?;
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

/// The slot a judgment runs on. Dropped before the judgment is
/// [`done`](Self::done), as when its job's claim moves to another attempt,
/// it kills what is left in the slot's cgroup and waits for it to go before
/// the slot goes back to the pool, so that the next job finds it empty: the
/// exporter and Lake are killed as their handles drop, and the kernels die
/// with Lake, but not at once.
struct JudgingSlot {
    pool: Arc<Pool>,
    lease: Option<Lease>,
}

impl JudgingSlot {
    /// The judgment is done: the slot goes back as it is.
    fn done(mut self) {
        self.lease.take();
    }
}

impl std::ops::Deref for JudgingSlot {
    type Target = Lease;
    fn deref(&self) -> &Lease {
        self.lease.as_ref().unwrap()
    }
}

impl std::ops::DerefMut for JudgingSlot {
    fn deref_mut(&mut self) -> &mut Lease {
        self.lease.as_mut().unwrap()
    }
}

impl Drop for JudgingSlot {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let pool = self.pool.clone();
        runtime.spawn(async move {
            pool.launcher().kill(lease.id).await;
            drop(lease);
        });
    }
}

/// Judges the run `job` names: audits it, and judges the pairs the audit
/// compared with `lake comparator`, each in turn, on exports of the modules
/// pisa built, saying what it is doing to `progress`. Returns the result,
/// which [`acceptance`] makes the record of to store.
pub async fn accept(
    pool: &Arc<Pool>,
    job: SnapshotAcceptJob,
    progress: &ProgressSender,
) -> Result<AcceptResponse, Error> {
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
    let mut progress = Progress::new(pool.launcher(), progress);
    progress.begin(AcceptPhase::Linking);
    let closures = (progress)
        .during(snapshots.link_verified(&ids, &judging.0))
        .await?;
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
    progress.begin(AcceptPhase::Waiting);
    let slot = (progress)
        .during(async {
            let mut slot = JudgingSlot {
                pool: pool.clone(),
                lease: Some(pool.acquire("").await),
            };
            // The exporter and the kernels load the modules, so free the
            // slot's memory first.
            slot.retire_and_wait("evict").await;
            slot
        })
        .await;
    let judgments = judge(
        pool.launcher(),
        slot.id,
        &modules,
        project,
        &to_judge,
        &options,
        &mut progress,
    )
    .await?;
    slot.done();
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
            phases: judged.phases,
        })
        .collect();
    let clean = audit.clean == Some(true);
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
    Ok(accepted)
}

/// The record of `accepted`, to store with its job's result: under the hex
/// of its id, of the spec entries it was judged against.
pub fn acceptance(snapshots: &Snapshots, accepted: &AcceptResponse) -> Result<Acceptance, Error> {
    let specs = (accepted.against.iter())
        .map(|a| snapshots.reader().parse(a))
        .collect::<Result<Vec<_>, _>>()?;
    let (_, hex) = accepted.id.rsplit_once('~').unwrap_or_default();
    snapshots.catalog().new_acceptance(hex, &specs, accepted)
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

/// The executable Lake runs as [`DEFAULT_KERNEL`], in the toolchain's `bin`
/// (`Check.lean:618`).
const DEFAULT_KERNEL_EXE: &str = "leanchecker";

/// The kernel a process running `exe` is, as Lake names it, by the name of
/// its executable: one of [`KERNELS`], or [`DEFAULT_KERNEL_EXE`]; `None` for
/// any other program.
fn kernel_of(exe: &Path) -> Option<&'static str> {
    let file = exe.file_name()?.to_str()?;
    if file == DEFAULT_KERNEL_EXE {
        return Some(DEFAULT_KERNEL);
    }
    let kernel = KERNELS
        .iter()
        .find(|(_, kernel_exe, _)| *kernel_exe == file);
    kernel.map(|(name, ..)| *name)
}

/// The kernel running in `slot`, as [`kernel_of`] knows it among the
/// processes in the slot's cgroup; `None` if none is, or the worker has no
/// cgroups. Lake runs one kernel at a time.
fn running_kernel(launcher: &Launcher, slot: usize) -> Option<&'static str> {
    let exes = launcher.slot_executables(slot);
    exes.iter().find_map(|exe| kernel_of(exe))
}

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

/// The kernels comparator runs for a pair, by name, in Lake's order: those
/// of a paranoid pair, without the ones a spec axiom leaves out, and Lean's
/// own last.
fn run_kernels(paranoid: bool, left_out: bool) -> Vec<&'static str> {
    let named = KERNELS.iter().map(|(name, ..)| *name);
    let mut names: Vec<&str> = match paranoid {
        true => named
            .filter(|name| !(left_out && DECLINING.contains(name)))
            .collect(),
        false => Vec::new(),
    };
    names.push(DEFAULT_KERNEL);
    names
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

/// The most of a kernel's own line that is kept as its reason, in characters.
const REASON_CHARS: usize = 200;

/// What Lake printed of a kernel's run.
#[derive(Clone, Copy, PartialEq)]
enum Said {
    Accepts,
    Rejected,
    Interacting,
}

/// The kernel and code of the last line of `output` that says
/// `error: <kernel> exited with <code>` for one of `kernels`, which are the
/// names pisa configured, exactly: a kernel's own output comes before it and
/// can say anything.
fn exit_failure<'a>(output: &'a str, kernels: &[&str]) -> Option<(&'a str, i32)> {
    output.lines().rev().find_map(|line| {
        let (name, code) = line.strip_prefix("error: ")?.split_once(" exited with ")?;
        kernels.contains(&name).then_some(())?;
        Some((name, code.parse().ok()?))
    })
}

/// What a kernel that exited with `code` was: a kill by the memory limit
/// (`oom`, for a code of 128 or more) is out of memory, and any other
/// failure but a rejection (1) and con-leche's and con-ron's decline (2) is
/// that it couldn't run. A 1 after bwrap said it couldn't make the kernel's
/// sandbox (`sandbox`, see [`sandbox_failed`]) is bwrap's, not the kernel's.
fn exit_outcome(name: &str, code: i32, oom: bool, sandbox: bool) -> (KernelOutcome, PairErrorKind) {
    match code {
        1 if !sandbox => (KernelOutcome::Rejected, PairErrorKind::Refused),
        2 if DECLINING.contains(&name) => (KernelOutcome::Unsupported, PairErrorKind::Unsupported),
        128.. if oom => (KernelOutcome::OutOfMemory, PairErrorKind::OutOfMemory),
        _ => (KernelOutcome::CannotRun, PairErrorKind::CannotRun),
    }
}

/// Whether the first line Lake printed of kernel `name` in its `sections`
/// is bwrap's own: bwrap exits 1 when it can't make the sandbox comparator
/// runs the kernel in, as a kernel that rejects the solution does. Like the
/// rest of the output, it can only label a failure.
fn sandbox_failed(sections: &HashMap<&str, (Said, Option<String>)>, name: &str) -> bool {
    let first = sections.get(name).and_then(|(_, first)| first.as_deref());
    first.is_some_and(|line| line.starts_with("bwrap: "))
}

/// What Lake printed of each kernel it ran: from its `Running` line to its
/// verdict, and the first line the kernel printed between them.
fn kernel_sections(output: &str) -> HashMap<&str, (Said, Option<String>)> {
    let mut sections = HashMap::new();
    let mut open: Option<(&str, Option<&str>)> = None;
    for line in output.lines() {
        if let Some(name) =
            (line.strip_prefix("Running ")).and_then(|l| l.strip_suffix(" kernel on solution"))
        {
            open = Some((name, None));
            continue;
        }
        let Some((name, first)) = open else { continue };
        let said = if line == format!("{name} kernel accepts the solution") {
            Said::Accepts
        } else if line == format!("{name} kernel rejected the solution") {
            Said::Rejected
        } else if line == format!("Error while interacting with {name} kernel") {
            Said::Interacting
        } else {
            if first.is_none() && !line.trim().is_empty() {
                open = Some((name, Some(line.trim())));
            }
            continue;
        };
        let reason = first.map(|l| match l.char_indices().nth(REASON_CHARS) {
            Some((end, _)) => format!("{}\u{2026}", &l[..end]),
            None => l.to_string(),
        });
        sections.insert(name, (said, reason));
        open = None;
    }
    sections
}

/// The kernels of a pair comparator didn't accept, in the order of an
/// accepted pair's, with the kernels a spec axiom leaves out as they are
/// there. The code in comparator's final `error:` line, which is the first
/// failing kernel's, says what happened to that kernel (`oom`, if the
/// memory limit killed something), if comparator exited 1 (`code`), as
/// [`classify`] reads it: a kernel after it is `failed` if Lake
/// printed that it rejected the solution, since Lake doesn't report its code.
/// The rest is as Lake printed it, with the kernel's first line as its
/// `reason` if it didn't accept. It is all for the reader and never settles
/// a pair, as the solution's own output and a declaration's name could
/// imitate it.
fn failed_kernels(
    output: &str,
    code: Option<i32>,
    paranoid: bool,
    left_out: bool,
    oom: bool,
) -> Vec<KernelVerdict> {
    let failure =
        exit_failure(output, &run_kernels(paranoid, left_out)).filter(|_| code == Some(1));
    let sections = kernel_sections(output);
    if sections.is_empty() && failure.is_none() {
        return Vec::new();
    }
    let kernels = accepted_kernels(paranoid, left_out);
    kernels
        .into_iter()
        .filter_map(|k| {
            if k.verdict == KernelOutcome::Unsupported {
                return Some(k);
            }
            let section = sections.get(k.name.as_str());
            let verdict = match (failure, section) {
                (Some((name, code)), _) if name == k.name => {
                    exit_outcome(name, code, oom, sandbox_failed(&sections, name)).0
                }
                (_, Some((Said::Accepts, _))) => KernelOutcome::Accepted,
                (_, Some((Said::Rejected, _))) => KernelOutcome::Failed,
                (_, Some((Said::Interacting, _))) => KernelOutcome::CannotRun,
                (_, None) => return None,
            };
            let reason = match (verdict, section) {
                (KernelOutcome::Accepted, _) | (_, None) => None,
                (_, Some((_, reason))) => reason.as_deref(),
            };
            Some(kernel(&k.name, verdict, reason))
        })
        .collect()
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
/// start, and 1 for any error: its last error line tells them apart, and
/// when a kernel failed, the code it names (see [`exit_outcome`]), if the
/// kernel is one of `kernels`, those pisa configured for the pair. That can
/// only label a failure, never excuse one. A signal that killed it is a
/// timeout only if it was its time limit's.
fn classify(ran: &Ran, oom: bool, kernels: &[&str]) -> Option<PairError> {
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
    let failure = exit_failure(&ran.output, kernels).filter(|_| ran.code == Some(1));
    if let Some((name, code)) = failure {
        let sandbox = sandbox_failed(&kernel_sections(&ran.output), name);
        let kind = exit_outcome(name, code, oom, sandbox).1;
        let message = match sandbox {
            true => format!("{name} exited with {code}: bwrap couldn't make its sandbox"),
            false => format!("{name} exited with {code}"),
        };
        return Some(PairError { kind, message });
    }
    let (kind, message) = match ran.code {
        _ if oom => (
            PairErrorKind::OutOfMemory,
            "comparator exceeded the worker's memory limit".to_string(),
        ),
        None if ran.timed_out => (PairErrorKind::Timeout, ran.output.clone()),
        None => (
            PairErrorKind::CannotRun,
            "comparator was killed by a signal".to_string(),
        ),
        Some(1) => {
            let kind = match error {
                Some(e) if e.starts_with("Error while interacting with") => {
                    PairErrorKind::CannotRun
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

/// The first component of every module `codes` import.
fn imported_roots<'a>(codes: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    let mut roots = BTreeSet::new();
    for code in codes {
        for line in split_header(code).header.lines() {
            let modules = import_parts(line).map(|(_, modules)| modules);
            roots.extend(
                modules
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m.split('.').next())
                    .map(String::from),
            );
        }
    }
    roots
}

/// The libraries `project` builds itself, by the module roots of its build
/// outputs: those a wrapper project like `docker/lean-project` builds no
/// code imports, but a project that is itself a library's checkout builds
/// the library.
fn own_libraries(project: &Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(project.join(".lake/build/lib/lean")) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            match name.strip_suffix(".olean") {
                Some(module) => Some(module.to_string()),
                None => e.path().is_dir().then_some(name),
            }
        })
        .collect()
}

/// The Lake configuration: the project's toolchain and manifest, and a
/// lakefile with its requirements. When the modules import `imported`,
/// libraries of the project itself, they require the project, from
/// `source`, without which the export is refused.
fn project_files(
    project: Option<&Path>,
    roots: &[&str],
    imported: &BTreeSet<String>,
    source: Option<&ProjectSource>,
) -> Result<Vec<ExportedFile>, Error> {
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
    if !imported.is_empty() {
        let libraries: Vec<&str> = imported.iter().map(String::as_str).collect();
        let Some(source) = source else {
            let message = format!(
                "the run's modules import {}, which the worker's project builds itself, and the \
                 export can't say where that code comes from: set PISA_PROJECT_GIT and \
                 PISA_PROJECT_REV on the workers, or make the project a wrapper that requires \
                 the library (README, *A pool for another project*)",
                libraries.join(", ")
            );
            return Err(Error::new(ErrorKind::BadRequest, message));
        };
        let config = match project.join("lakefile.lean").exists() {
            true => "lakefile.lean",
            false => "lakefile.toml",
        };
        require_project(&mut manifest, source, config);
    }
    manifest["name"] = NAME.into();
    Ok(vec![
        file("lean-toolchain", read("lean-toolchain")?),
        file("lakefile.toml", lakefile(&manifest, roots)),
        file("lake-manifest.json", pretty(&manifest)),
    ])
}

/// Adds the project itself to `manifest`'s packages, from `source`, under
/// the project's package name, which it still has, configured by its file
/// `config`.
fn require_project(manifest: &mut Value, source: &ProjectSource, config: &str) {
    let name = manifest["name"].as_str().unwrap_or("project").to_string();
    let package = json!({
        "type": "git", "name": name, "url": source.git, "rev": source.rev,
        "inputRev": source.rev, "subDir": null, "scope": "", "inherited": false,
        "manifestFile": "lake-manifest.json", "configFile": config,
    });
    match manifest["packages"].as_array_mut() {
        Some(packages) => packages.insert(0, package),
        None => manifest["packages"] = json!([package]),
    }
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
            phases: Vec::new(),
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

    /// What comparator printed, as Lake 4.36.0's `CLI/Check.lean`,
    /// `Check/Compare.lean` and `Check/Axioms.lean` print it, and how it
    /// exited: with no `code`, its time limit killed it.
    fn ran(code: Option<i32>, output: &str) -> Ran {
        Ran {
            code,
            timed_out: code.is_none(),
            output: output.into(),
        }
    }

    const PARANOID: [&str; 6] = [
        "Lean paranoid",
        "con-leche",
        "con-ron",
        "lean4lean",
        "nanoda",
        "Lean default",
    ];

    fn kind(ran: &Ran, oom: bool) -> Option<PairErrorKind> {
        classify(ran, oom, &PARANOID).map(|e| e.kind)
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

    /// A kernel is known by its executable's name, wherever the toolchain
    /// is, as Lake runs it: Lean's own kernel is `leanchecker`, and Lean
    /// paranoid `leanchecker-paranoid`. Nothing else is a kernel.
    #[test]
    fn a_kernel_is_known_by_its_executable() {
        for bin in ["/tc/bin", "/elsewhere"] {
            let bin = Path::new(bin);
            for (name, exe, _) in KERNELS {
                assert_eq!(kernel_of(&bin.join(exe)), Some(name), "{exe}");
            }
            assert_eq!(kernel_of(&bin.join("leanchecker")), Some(DEFAULT_KERNEL));
        }
        assert_eq!(
            kernel_of(Path::new("/tc/bin/leanchecker-paranoid")),
            Some("Lean paranoid")
        );
        for other in [
            "/tc/bin/lake",
            "/usr/bin/bwrap",
            "/tc/bin/leanexport",
            "/tc/bin/lean",
            "/tc/bin/nanoda",
            "/tc/bin/con-ron.sh",
            "/tc/bin",
            "",
        ] {
            assert_eq!(kernel_of(Path::new(other)), None, "{other}");
        }
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
        use KernelOutcome::{Accepted, CannotRun, Failed, Unsupported};
        let said = |k: &[KernelVerdict]| -> Vec<(String, KernelOutcome)> {
            k.iter().map(|k| (k.name.clone(), k.verdict)).collect()
        };
        let named = |v: &[(&str, KernelOutcome)]| -> Vec<(String, KernelOutcome)> {
            v.iter().map(|(n, o)| (n.to_string(), *o)).collect()
        };
        let k = failed_kernels(DECLINED, Some(1), true, false, false);
        assert_eq!(
            said(&k),
            named(&[
                ("Lean paranoid", Accepted),
                ("con-leche", Unsupported),
                ("con-ron", Failed),
                ("Lean default", Accepted),
            ])
        );
        let out = "Running nanoda kernel on solution\nError while interacting with nanoda kernel\n\
                   Running Lean default kernel on solution\nLean default kernel accepts the solution\n";
        let k = failed_kernels(out, Some(1), true, true, false);
        assert_eq!(
            said(&k),
            named(&[
                ("con-leche", Unsupported),
                ("con-ron", Unsupported),
                ("nanoda", CannotRun),
                ("Lean default", Accepted),
            ])
        );
        assert_eq!(
            failed_kernels("error: Child exited with 1\n", Some(1), true, true, false),
            []
        );
    }

    /// A kernel's code is read only from a comparator that exited 1, as
    /// `classify` reads it: one that was killed, or exited otherwise, has no
    /// kernel's code, whatever its output says.
    #[test]
    fn failed_kernels_read_a_code_only_from_an_exit_of_1() {
        let out = con_ron_failed("", 1);
        let con_ron = |code| failed_kernels(&out, code, true, false, false)[2].verdict;
        assert_eq!(con_ron(Some(1)), KernelOutcome::Rejected);
        for code in [None, Some(2), Some(3)] {
            assert_eq!(con_ron(code), KernelOutcome::Failed, "{code:?}");
        }
    }

    /// What Lake prints of `kernels`, each as `(name, its own output, whether
    /// it exited non-zero)`, in the order given, and its final error line.
    fn printed(kernels: &[(&str, &str, bool)], error: &str) -> String {
        let mut out = String::from("Exporting #[t] from PisaRun.x\n");
        for (name, own, rejected) in kernels {
            out.push_str(&format!("Running {name} kernel on solution\n"));
            if !own.is_empty() {
                out.push_str(&format!("{own}\n"));
            }
            let verdict = if *rejected { "rejected" } else { "accepts" };
            out.push_str(&format!("{name} kernel {verdict} the solution\n"));
        }
        out.push_str(error);
        out
    }

    /// `con-ron` failing in a run of all the kernels: those after it accept.
    fn con_ron_failed(own: &str, code: i32) -> String {
        printed(
            &[
                ("Lean paranoid", "", false),
                ("con-leche", "", false),
                ("con-ron", own, true),
                ("lean4lean", "", false),
                ("nanoda", "", false),
                ("Lean default", "", false),
            ],
            &format!("error: con-ron exited with {code}\n"),
        )
    }

    /// Lake's final line has the first failing kernel's code, and the code
    /// says what happened to the kernel: a kill is never a rejection.
    #[test]
    fn the_first_failing_kernels_code_names_the_failure() {
        use KernelOutcome as K;
        use PairErrorKind as P;
        let judged = |output: &str, oom: bool, left_out: bool| {
            let ran = ran(Some(1), output);
            let names = run_kernels(true, left_out);
            let kernels = failed_kernels(output, Some(1), true, left_out, oom);
            (classify(&ran, oom, &names), kernels)
        };
        let con_ron = |kernels: &[KernelVerdict]| {
            kernels
                .iter()
                .find(|k| k.name == "con-ron")
                .unwrap()
                .verdict
        };
        for (code, oom, kernel, pair) in [
            (1, false, K::Rejected, P::Refused),
            (1, true, K::Rejected, P::Refused),
            (2, false, K::Unsupported, P::Unsupported),
            (3, false, K::CannotRun, P::CannotRun),
            (137, true, K::OutOfMemory, P::OutOfMemory),
            (134, true, K::OutOfMemory, P::OutOfMemory),
            (137, false, K::CannotRun, P::CannotRun),
            (42, false, K::CannotRun, P::CannotRun),
            (42, true, K::CannotRun, P::CannotRun),
        ] {
            let (error, kernels) = judged(&con_ron_failed("", code), oom, false);
            let error = error.unwrap();
            assert_eq!(
                (error.kind, con_ron(&kernels)),
                (pair, kernel),
                "con-ron exited with {code}, memory limit {oom}: {error:?}"
            );
        }
        // Only con-leche and con-ron decline: another kernel's 2 is a failure.
        let nanoda = printed(&[("nanoda", "", true)], "error: nanoda exited with 2\n");
        let (error, kernels) = judged(&nanoda, false, false);
        assert_eq!(error.unwrap().kind, P::CannotRun);
        assert_eq!(kernels[0].verdict, K::CannotRun);
        // The kernel's name is exactly one pisa configured: con-ron was left
        // out, and a name of its own isn't one.
        for name in ["con-ron", "bogus", "con-ron "] {
            let forged =
                format!("Running Lean default kernel on solution\nerror: {name} exited with 137\n");
            let (error, _) = judged(&forged, true, name == "con-ron");
            let error = error.unwrap();
            // Not a kernel it ran: the memory limit's kill is all it knows.
            assert_eq!(error.kind, P::OutOfMemory, "{name}");
            let (error, _) = judged(&forged, false, name == "con-ron");
            assert_eq!(error.unwrap().kind, P::Refused, "{name}");
        }
        // Only the last such line counts: a kernel's own output comes first,
        // and it can print what a declaration's name says.
        let own = "error: con-ron exited with 137";
        let first = printed(
            &[("nanoda", own, true), ("Lean default", "", false)],
            "error: nanoda exited with 1\n",
        );
        let (error, kernels) = judged(&first, false, false);
        assert_eq!(error.unwrap().kind, P::Refused);
        assert_eq!(kernels[0].verdict, K::Rejected);
        // Without such a line, as before: the exit says it was refused.
        let (error, _) = judged("error: Challenge and solution do not match\n", false, false);
        assert_eq!(error.unwrap().kind, P::Refused);
    }

    /// Lake reports only the first failure's code, so a kernel after it that
    /// printed `rejected` is `failed`, with no code, and one that printed
    /// `accepts` is accepted. The memory limit's kill is the first kernel's.
    #[test]
    fn kernels_after_the_first_failure_are_failed_with_no_code() {
        use KernelOutcome::{Accepted, Failed, OutOfMemory, Rejected};
        let out = printed(
            &[
                ("Lean paranoid", "", false),
                ("con-leche", "", false),
                ("con-ron", "", true),
                ("lean4lean", "", true),
                ("nanoda", "", false),
                ("Lean default", "", true),
            ],
            "error: con-ron exited with 137\n",
        );
        let outcomes = |oom| -> Vec<(String, KernelOutcome)> {
            let kernels = failed_kernels(&out, Some(1), true, false, oom);
            kernels.into_iter().map(|k| (k.name, k.verdict)).collect()
        };
        let named = |v: &[(&str, KernelOutcome)]| -> Vec<(String, KernelOutcome)> {
            v.iter().map(|(n, o)| (n.to_string(), *o)).collect()
        };
        assert_eq!(
            outcomes(true),
            named(&[
                ("Lean paranoid", Accepted),
                ("con-leche", Accepted),
                ("con-ron", OutOfMemory),
                ("lean4lean", Failed),
                ("nanoda", Accepted),
                ("Lean default", Failed),
            ])
        );
        let rejected = printed(
            &[("nanoda", "", true), ("Lean default", "", true)],
            "error: nanoda exited with 1\n",
        );
        let kernels = failed_kernels(&rejected, Some(1), true, false, false);
        let got: Vec<_> = kernels.iter().map(|k| k.verdict).collect();
        assert_eq!(got, [Rejected, Failed]);
    }

    /// A kernel's reason is its own first line, for the reader, and only for
    /// one that didn't accept.
    #[test]
    fn a_kernels_own_line_is_its_reason() {
        let own = "\ncon-ron: declined: non-standard axiom [at axiom lit]\nsecond line";
        let kernels = failed_kernels(&con_ron_failed(own, 2), Some(1), true, false, false);
        let reasons: Vec<(&str, Option<&str>)> = kernels
            .iter()
            .map(|k| (k.name.as_str(), k.reason.as_deref()))
            .collect();
        assert_eq!(
            reasons,
            [
                ("Lean paranoid", None),
                ("con-leche", None),
                (
                    "con-ron",
                    Some("con-ron: declined: non-standard axiom [at axiom lit]")
                ),
                ("lean4lean", None),
                ("nanoda", None),
                ("Lean default", None),
            ]
        );
        // A killed kernel said nothing.
        let kernels = failed_kernels(&con_ron_failed("", 137), Some(1), true, false, true);
        assert_eq!(kernels[2].reason, None);
        // A long line is cut, with a mark, at 200 characters.
        let long = format!("con-ron: {}", "x".repeat(500));
        let kernels = failed_kernels(&con_ron_failed(&long, 1), Some(1), true, false, false);
        let reason = kernels[2].reason.as_deref().unwrap();
        assert_eq!(reason.chars().count(), 201);
        assert!(reason.starts_with("con-ron: xxx") && reason.ends_with('\u{2026}'));
        // A kernel's section ends where the next kernel's starts.
        let two = printed(
            &[("nanoda", "nanoda: bad", true), ("Lean default", "", true)],
            "error: nanoda exited with 1\n",
        );
        let kernels = failed_kernels(&two, Some(1), true, false, false);
        assert_eq!(kernels[0].reason.as_deref(), Some("nanoda: bad"));
        assert_eq!(kernels[1].reason, None);
    }

    /// The kernels a failed pair lists are in the order an accepted one's
    /// are, with those a spec axiom leaves out in their place.
    #[test]
    fn a_failed_pair_lists_its_kernels_in_an_accepted_ones_order() {
        for left_out in [false, true] {
            let ran: Vec<(&str, &str, bool)> = run_kernels(true, left_out)
                .into_iter()
                .map(|n| (n, "", n == "nanoda"))
                .collect();
            let out = printed(&ran, "error: nanoda exited with 1\n");
            let failed = failed_kernels(&out, Some(1), true, left_out, false);
            let accepted = accepted_kernels(true, left_out);
            let names =
                |k: &[KernelVerdict]| -> Vec<String> { k.iter().map(|k| k.name.clone()).collect() };
            assert_eq!(names(&failed), names(&accepted), "left out: {left_out}");
            let unsupported = |k: &[KernelVerdict]| -> Vec<String> {
                let unsupported = k.iter().filter(|k| k.verdict == KernelOutcome::Unsupported);
                unsupported.map(|k| k.name.clone()).collect()
            };
            assert_eq!(unsupported(&failed), unsupported(&accepted));
        }
        assert_eq!(run_kernels(false, false), ["Lean default"]);
        assert_eq!(
            run_kernels(true, true),
            ["Lean paranoid", "lean4lean", "nanoda", "Lean default"]
        );
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
            assert!(classify(&ran(code, forged), false, &PARANOID).is_some());
        }
        let e = classify(&ran(Some(1), DECLINED), false, &PARANOID).unwrap();
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
        assert_eq!(classify(&ran(Some(0), DECLINED), false, &PARANOID), None);
    }

    #[test]
    fn comparators_exit_says_what_kind_of_failure_it_was() {
        let built = "Resolving dependencies\nBuilding PisaSnap.sa\n\
                     Exporting t from PisaSnap.sa\nBuilding PisaSnap.sb\n\
                     Exporting t from PisaSnap.sb\n";
        assert_eq!(kind(&ran(Some(0), "Your solution is okay!\n"), false), None);
        let refused =
            format!("{built}error: Challenge and solution theorem statement do not match: 't'\n");
        let e = classify(&ran(Some(1), &refused), false, &PARANOID).unwrap();
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
        let oom = classify(&ran(Some(1), ""), true, &PARANOID).unwrap();
        let named = with_cap(oom, Some(("PISA_MEMORY_MAX_MB", 16000)));
        assert!(
            named.message.ends_with(" (PISA_MEMORY_MAX_MB = 16000 MiB)"),
            "{}",
            named.message
        );
        let other = with_cap(
            classify(&ran(None, "slow"), false, &PARANOID).unwrap(),
            Some(("PISA_MEMORY_MAX_MB", 1)),
        );
        assert_eq!(other.message, "slow");
        // bwrap exits 1 when it can't make a kernel's sandbox, as a kernel
        // that rejects the solution does: these are what it printed.
        for line in [
            "bwrap: No permissions to create a new namespace, likely because the kernel does \
             not allow non-privileged user namespaces.",
            "bwrap: Can't mount proc on /proc: Operation not permitted",
        ] {
            let out = format!(
                "{built}Running Lean default kernel on solution\n{line}\n\
                 error: Lean default exited with 1\nLean default kernel rejected the solution\n"
            );
            let e = classify(&ran(Some(1), &out), false, &PARANOID).unwrap();
            assert_eq!(e.kind, PairErrorKind::CannotRun, "{line}");
            assert_eq!(
                e.message,
                "Lean default exited with 1: bwrap couldn't make its sandbox"
            );
            let k = failed_kernels(&out, Some(1), false, false, false);
            assert_eq!(k.len(), 1, "{k:?}");
            assert_eq!(k[0].verdict, KernelOutcome::CannotRun);
            assert!(
                k[0].reason.as_deref().unwrap().starts_with("bwrap: "),
                "{k:?}"
            );
        }
        // A build the memory limit killed exits like a failed one.
        let killed = "Resolving dependencies\nBuilding PisaSnap.sa\nerror: Child exited with 137\n";
        assert_eq!(
            kind(&ran(Some(1), killed), true),
            Some(PairErrorKind::OutOfMemory)
        );
    }

    /// A `lake` that a signal killed, which wasn't its time limit's, as when
    /// it crashes, couldn't run: it didn't time out. Or it ran out of memory,
    /// if the memory limit killed something meanwhile.
    #[test]
    fn a_signal_that_isnt_the_time_limit_is_no_timeout() {
        let killed = Ran {
            code: None,
            timed_out: false,
            output: "fake-lake comparator\nRunning Lean default kernel on solution\n".into(),
        };
        let e = classify(&killed, false, &PARANOID).unwrap();
        assert_eq!(e.kind, PairErrorKind::CannotRun);
        assert_eq!(e.message, "comparator was killed by a signal");
        assert_eq!(kind(&killed, true), Some(PairErrorKind::OutOfMemory));
        assert_eq!(
            kind(&ran(None, "slow"), false),
            Some(PairErrorKind::Timeout)
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
        let files = project_files(None, &["PisaSnap"], &BTreeSet::new(), None).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["lake-manifest.json", "lakefile.toml"]);
        assert!(!files[1].text.contains("[[require]]"), "{}", files[1].text);
    }

    /// A project named `name` that builds the libraries `own`, and requires
    /// cslib.
    fn project(name: &str, own: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        std::fs::write(
            path.join("lean-toolchain"),
            "leanprover/lean4:v4.36.0-rc2\n",
        )
        .unwrap();
        std::fs::write(path.join("lakefile.toml"), format!("name = \"{name}\"\n")).unwrap();
        let manifest = json!({"version": "1.3.0", "name": name, "packages": [
            {"type": "git", "name": "cslib", "url": "https://example.com/cslib.git",
             "rev": "98f9", "inputRev": "98f9", "inherited": false}]});
        std::fs::write(path.join("lake-manifest.json"), manifest.to_string()).unwrap();
        let lib = path.join(".lake/build/lib/lean");
        for root in own {
            std::fs::create_dir_all(lib.join(root)).unwrap();
            std::fs::write(lib.join(format!("{root}.olean")), "").unwrap();
        }
        dir
    }

    const ON_THE_LIBRARY: &str =
        "import Mathlib\nimport Lib.Classes.QMA\n\ntheorem t : True := trivial";

    #[test]
    fn an_export_requires_the_project_whose_own_library_its_modules_import() {
        let dir = project("lib-frontier", &["Lib", "LibResults"]);
        let own = own_libraries(dir.path());
        assert_eq!(
            own,
            BTreeSet::from(["Lib".to_string(), "LibResults".to_string()])
        );
        let imported: BTreeSet<String> = imported_roots([ON_THE_LIBRARY].into_iter())
            .intersection(&own)
            .cloned()
            .collect();
        assert_eq!(imported, BTreeSet::from(["Lib".to_string()]));
        let source = ProjectSource {
            git: "https://example.com/lib-frontier.git".into(),
            rev: "33c9".into(),
        };
        let files =
            project_files(Some(dir.path()), &["PisaRun"], &imported, Some(&source)).unwrap();
        let text = |path: &str| files.iter().find(|f| f.path == path).unwrap().text.clone();
        let lakefile = text("lakefile.toml");
        assert!(
            lakefile.contains(
                "[[require]]\nname = \"lib-frontier\"\ngit = \"https://example.com/lib-frontier.git\"\n\
                 rev = \"33c9\"\n\n[[require]]\nname = \"cslib\""
            ),
            "{lakefile}"
        );
        let manifest: Value = serde_json::from_str(&text("lake-manifest.json")).unwrap();
        assert_eq!(manifest["name"], NAME);
        assert_eq!(manifest["packages"][0]["name"], "lib-frontier");
        assert_eq!(manifest["packages"][0]["rev"], "33c9");

        // Without the project's source, the export says what is missing.
        let e = project_files(Some(dir.path()), &["PisaRun"], &imported, None).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadRequest);
        assert!(
            e.message.contains("import Lib,") && e.message.contains("PISA_PROJECT_GIT"),
            "{}",
            e.message
        );
    }

    #[test]
    fn a_wrapper_project_needs_no_source() {
        // `docker/lean-project` builds `PisaProject`, which no code imports.
        let dir = project("pisa_project", &["PisaProject"]);
        let imported: BTreeSet<String> = imported_roots([ON_THE_LIBRARY].into_iter())
            .intersection(&own_libraries(dir.path()))
            .cloned()
            .collect();
        assert!(imported.is_empty());
        let files = project_files(Some(dir.path()), &["PisaRun"], &imported, None).unwrap();
        let lakefile = &files
            .iter()
            .find(|f| f.path == "lakefile.toml")
            .unwrap()
            .text;
        assert!(!lakefile.contains("pisa_project\"\ngit"), "{lakefile}");
    }
}
