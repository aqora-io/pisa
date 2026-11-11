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
    AcceptPair, AcceptResponse, AuditSnapshotsResponse, Error, ErrorKind, LeftOut, LeftOutReason,
    PairError, PairErrorKind,
};
use pisa_protocol::snapshot;
use pisa_protocol::verify::Declaration;
use pisa_protocol::wire::{ExportedFile, ExportedPair, ExportedRun, SnapshotAcceptJob};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::catalog::ModuleMeta;
use crate::launcher::Ran;
use crate::pool::Pool;
use crate::snapshots::Snapshots;

/// How many of comparator's last lines of output a pair keeps.
const OUTPUT_LINES: usize = 40;

/// How many judged runs a worker keeps built.
const KEPT_RUNS: usize = 4;

/// The project's Lake package.
const NAME: &str = "pisa_run";

/// The axioms `#print axioms` counts a proof clean with.
const STANDARD_AXIOMS: [&str; 3] = ["propext", "Quot.sound", "Classical.choice"];

/// The run that spec snapshots `against` start, with `allow` naming what its
/// heads may change besides the spec's holes, as a Lake project. `project`
/// is the worker's, whose toolchain and packages it takes.
pub async fn export(
    snapshots: &Snapshots,
    project: Option<&Path>,
    against: &[String],
    allow: &[String],
) -> Result<ExportedRun, Error> {
    Ok(audited_export(snapshots, project, against, allow).await?.1)
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
    let mut files = Vec::new();
    let mut pairs = Vec::new();
    for (n, (compared, pair)) in run.audit.comparisons.iter().zip(&run.pairs).enumerate() {
        let spec_meta = record(&pair.spec_module).await?;
        let head_meta = record(&pair.head_module).await?;
        needed.extend(spec_meta.modules.iter().map(|m| m.id.clone()));
        needed.extend(head_meta.modules.iter().map(|m| m.id.clone()));
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
        for m in &head_meta.modules {
            let meta = record(&m.id).await?;
            let defs = meta.result.declarations.iter().filter(|d| d.kind == "def");
            head_defs.extend(defs.map(|d| d.name.clone()));
        }
        let holes: Vec<String> = pair.holes.iter().chain(allow).cloned().collect();
        let named = config(
            &pair.spec_module,
            &pair.head_module,
            &spec_meta.result.declarations,
            &holes,
            &assumptions,
            (&spec_defs, &head_defs),
        );
        let path = format!("comparator/{}.json", n + 1);
        files.push(file(&path, named.text));
        pairs.push(ExportedPair {
            spec: compared.spec.clone(),
            head: compared.head.clone(),
            config: path,
            theorems: named.theorems,
            left_out: named.left_out,
        });
    }
    for m in &needed {
        let code = record(m).await?.code;
        files.push(file(&format!("{}/s{m}.lean", snapshot::ROOT), code));
    }
    // The snapshots on each module, for the README's table.
    let modules: Vec<String> = needed.iter().cloned().collect();
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
    let project_files = project_files(project)?;
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
        .map(|m| {
            let path = format!("{}/s{m}.lean", snapshot::ROOT);
            (path, on.remove(m).unwrap_or_default())
        })
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
    let (audit, run) = audited_export(pool.snapshots(), project, &job.against, &job.allow).await?;
    let key = key(&run);
    // One judgment at a time of a run, in its directory.
    let lock = pool.snapshots().lock(&format!("run:{key}"));
    let _judging = lock.lock().await;
    let runs = pool.launcher().runs_dir();
    let dir = runs.join(&key);
    write(&dir, &run, project).map_err(|e| {
        Error::new(
            ErrorKind::Internal,
            format!("writing the run's project: {e}"),
        )
    })?;
    evict(pool, &runs, &key);
    let mut slot = pool.acquire("").await;
    // Comparator builds the modules, so free the slot's memory first.
    slot.retire_and_wait("evict").await;
    let timeout = Duration::from_secs(job.timeout_secs);
    let mut pairs = Vec::new();
    for pair in &run.pairs {
        // Nothing for comparator to judge, which it would refuse to start.
        let ran = match pair.theorems {
            0 => None,
            _ => {
                let mut args = vec!["comparator", "--config", &pair.config];
                if job.paranoid {
                    args.push("--paranoid");
                }
                let launcher = pool.launcher();
                let oom_before = launcher.oom_kills(slot.id);
                let ran = launcher
                    .lake(slot.id, &args, &dir, timeout)
                    .await
                    .map_err(|e| {
                        Error::new(ErrorKind::Internal, format!("running lake comparator: {e}"))
                    })?;
                let oom = launcher.oom_kills(slot.id) > oom_before;
                Some((ran, oom))
            }
        };
        let (error, exit_code, output) = match &ran {
            None => (
                Some(PairError {
                    kind: PairErrorKind::NothingToJudge,
                    message: "the spec names no theorem comparator can judge".into(),
                }),
                None,
                String::new(),
            ),
            Some((ran, oom)) => (
                classify(ran, *oom),
                ran.code,
                tail(&ran.output, OUTPUT_LINES),
            ),
        };
        pairs.push(AcceptPair {
            spec: pair.spec.clone(),
            head: pair.head.clone(),
            config: pair.config.clone(),
            accepted: error.is_none(),
            error,
            exit_code,
            output,
            theorems: pair.theorems,
            left_out: pair.left_out.clone(),
        });
    }
    drop(slot);
    let clean = audit.clean == Some(true);
    let snapshots = pool.snapshots();
    let hex = uuid::Uuid::new_v4().simple().to_string();
    let accepted = AcceptResponse {
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
    };
    snapshots.catalog().put_acceptance(&hex, &accepted).await?;
    Ok(accepted)
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

/// Writes `run` into `dir`, with the project's packages, which are built
/// already, as its own. A new run is written beside it and moved into place;
/// a run already there gets only its configurations and README again, so
/// that Lake finds what it built.
fn write(dir: &Path, run: &ExportedRun, project: Option<&Path>) -> std::io::Result<()> {
    let put = |root: &Path, f: &ExportedFile| -> std::io::Result<()> {
        let path = root.join(&f.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &f.text)
    };
    if dir.is_dir() {
        let _ = std::fs::remove_dir_all(dir.join("comparator"));
        for f in run.files.iter().filter(|f| judging_only(&f.path)) {
            put(dir, f)?;
        }
    } else {
        let new = dir.with_extension("tmp");
        let _ = std::fs::remove_dir_all(&new);
        for f in &run.files {
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

/// Why comparator didn't accept a pair, from how its run ended (`ran`, and
/// `oom` if the worker's memory limit killed something meanwhile); `None`
/// if it accepted it. Comparator exits 0 for a pair it accepts, 2 when it
/// can't start, and 1 for any error, a failed build in its sandbox included:
/// its last error line, and the step it was on, tell them apart.
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
fn project_files(project: Option<&Path>) -> Result<Vec<ExportedFile>, Error> {
    let Some(project) = project else {
        // As in tests, which build nothing.
        let manifest = json!({"version": "1.3.0", "packagesDir": ".lake/packages",
                              "packages": [], "name": NAME, "lakeDir": ".lake",
                              "fixedToolchain": false});
        return Ok(vec![
            file("lake-manifest.json", pretty(&manifest)),
            file("lakefile.toml", lakefile(&manifest)),
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
        file("lakefile.toml", lakefile(&manifest)),
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
/// revisions it names, with the snapshot modules as a library built with
/// `autoImplicit` off, as the worker builds them.
fn lakefile(manifest: &Value) -> String {
    let mut out = format!(
        "# A pisa run, for `lake comparator`: see README.md.\nname = {}\n\
         defaultTargets = [{}]\n\n[leanOptions]\nautoImplicit = false\n",
        toml(NAME),
        toml(snapshot::ROOT)
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
    out.push_str(&format!(
        "\n[[lean_lib]]\nname = {}\nglobs = [{}]\n",
        toml(snapshot::ROOT),
        toml(&format!("{}.+", snapshot::ROOT))
    ));
    out
}

/// A pair's comparator configuration, with what it names and leaves out.
struct Named {
    text: String,
    /// How many of the spec's theorems it names.
    theorems: u64,
    left_out: Vec<LeftOut>,
}

/// Comparator's configuration for spec module `spec` (the challenge) and
/// head module `head` (the solution): the spec's theorems among
/// `declarations`, `holes` as its definition holes, and the standard axioms
/// and `assumptions` as permitted. Left out, and listed with why: private
/// names, which differ between the two modules, and holes that aren't plain
/// definitions on both sides, the names in `defs` (spec's, head's), which
/// comparator would refuse as definition holes.
fn config(
    spec: &str,
    head: &str,
    declarations: &[Declaration],
    holes: &[String],
    assumptions: &[String],
    defs: (&HashSet<String>, &HashSet<String>),
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
    let theorems = name(
        &mut declarations
            .iter()
            .filter(|d| d.kind == "theorem" && !d.generated && d.parent.is_none())
            .map(|d| &d.name),
        false,
    );
    let holes = name(&mut holes.iter(), true);
    let standard: Vec<String> = STANDARD_AXIOMS.iter().map(|a| a.to_string()).collect();
    let axioms = name(&mut standard.iter().chain(assumptions), false);
    let config = json!({
        "challenge_module": snapshot::module(spec),
        "solution_module": snapshot::module(head),
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
        "\n`lake build` builds every module, with `autoImplicit` off as pisa built them. A \
         module's file is named by its code's hash, `PisaSnap/s<hash>.lean`, and imports the \
         others by those names.\n\n\
         ## Judging it\n\n\
         Judge each pair with `lake comparator --paranoid --config comparator/<n>.json`, which \
         needs `bwrap`. It accepts a pair when each theorem of the spec is proved in the head \
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
            lakefile(&manifest),
            "# A pisa run, for `lake comparator`: see README.md.\nname = \"pisa_run\"\n\
             defaultTargets = [\"PisaSnap\"]\n\n[leanOptions]\nautoImplicit = false\n\n\
             [[require]]\nname = \"Physlib\"\ngit = \"https://example.com/physlib.git\"\n\
             rev = \"lean-v4.36.0-rc2\"\n\n\
             [[require]]\nname = \"local\"\npath = \"../local\"\n\n\
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
        let named = config(
            "a",
            "b",
            &declarations,
            &holes,
            &assumptions,
            (&spec_defs, &head_defs),
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
        let files = project_files(None).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["lake-manifest.json", "lakefile.toml"]);
        assert!(!files[1].text.contains("[[require]]"), "{}", files[1].text);
    }
}
