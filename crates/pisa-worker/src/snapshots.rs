//! Environment snapshots: code compiled to a Lean module that later code
//! imports, its files kept in object storage so that any worker of the
//! version can import it. Entries keep modules alive: the same code is one
//! module, however many entries point at it, and a module goes once no entry
//! or kept module needs it. The records, with each module's check result, are
//! in the [`Catalog`], and so are the idempotency keys that make a create safe
//! to retry.
//!
//! Object storage, per pool version:
//! - `snapshots/<version>/<id>.<build>.<ext>.zst`: the module's files,
//!   zstd-compressed: its `.olean`, and for module-system code also
//!   `.olean.server`, `.olean.private`, `.ir` and `.ir.sig`. `build` is random
//!   per build and named in the module's record. The files are uploaded
//!   before the record is written, so a module exists once its record does,
//!   and a delete removes only the files of the record it removed: the same
//!   code built again meanwhile has files of its own.
//!
//! Workers keep the modules they import in a local cache on every Lean
//! process's search path. A cached snapshot is imported only while a lookup
//! in the store vouches for it (`PISA_SNAPSHOT_CONFIRM_SECS`): a module that
//! is still being built can't be imported, and a deleted one stops working
//! everywhere within that time, without a store request on every check.
//!
//! The kernel re-check is separate from building: it runs at creation for code
//! that uses macros or asks for it, and later on request. A module's record
//! is rewritten once it passes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pisa_protocol::api::{
    Allow, AuditAssumption, AuditFork, AuditLink, AuditSnapshotsResponse, AuditTarget,
    AuditUnproved, CheckResult, CreateSnapshotResponse, Declaration, DeleteSnapshotResponse, Error,
    ErrorKind, Export, ListDependentsResponse, ListSnapshotsResponse, RebuildItem, RebuildReplaced,
    RebuildSnapshotsResponse, Replacement, Replaces, Snapshot, SnapshotDetail, SnapshotId,
    VerifyFailure, VerifySnapshotResponse,
};
use pisa_protocol::header::{Split, header_key, split_header};
use pisa_protocol::labels::{
    MAX_LABELS, MAX_LIMIT, check_key, check_labels, is_cursor, snapshot_id_error,
};
use pisa_protocol::repl::{CommandResponse, ReplCommand, ReplResponse, Severity};
use pisa_protocol::search::{Description, Informal};
use pisa_protocol::snapshot;
use pisa_protocol::verify::{
    Change, ProofStatus, ReportedDeclaration, Verification, normalised, statement_changes,
};
use pisa_protocol::wire::{CheckOutcome, SnapshotCreateJob, SnapshotRebuildJob};
use sha2::{Digest, Sha256};

use crate::catalog::{
    Catalog, EntryMeta, Module, ModuleFile, ModuleMeta, NewEntry, SnapshotDecl, SnapshotDecls,
    record_fits, unknown_module,
};
use crate::check::{header_error, policy_error, run, unexpected};
use crate::db::Db;
use crate::policy::{
    Policy, module_export, module_report, parse_declarations, parse_export, parse_report,
    uses_macros,
};
use crate::pool::{Lease, Pool};

/// Time allowed for the report on a snapshot's declarations.
const REPORT_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Snapshots {
    store: Arc<dyn ObjectStore>,
    catalog: Catalog,
    version: String,
    /// Holds `PisaSnap/s<id>.<ext>`.
    root: PathBuf,
    confirm_every: Duration,
    /// The build of each module in the local cache.
    cached: Mutex<HashMap<String, Module>>,
    /// When the store last confirmed that each snapshot exists.
    confirmed: Mutex<HashMap<String, Instant>>,
    /// One task at a time loads or builds a given snapshot.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// The pool's policy, for the axioms an audit lists.
    policy: Policy,
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, e.to_string())
}

/// The error for a create whose idempotency key is on an entry of other code.
fn key_in_use() -> Error {
    Error::new(
        ErrorKind::BadRequest,
        "idempotency key already used for different code",
    )
}

/// The entry a create replaces, as it was found before the build.
struct Replacing {
    /// The replaced snapshot, as the request names it.
    replaced: Replaces,
    /// The replaced entry, which the new entry's link names.
    entry: String,
    /// The declarations of the replaced entry's module.
    declarations: Vec<Declaration>,
    /// The snapshot modules it needs, other than its own.
    needs: Vec<String>,
}

impl Replacing {
    /// The link the new entry records: the replaced entry, and what the
    /// replacement may change.
    fn link(&self) -> Replaces {
        Replaces {
            snapshot_id: self.entry.clone(),
            allow: self.replaced.allow.clone(),
        }
    }
}

/// The snapshot modules a replacement swaps: those the replaced module builds
/// on and the replacement doesn't (`lost`), and those the replacement builds
/// on and the replaced module doesn't (`gained`), each with its declarations.
#[derive(Default)]
struct Swapped {
    lost: Vec<(String, Vec<Declaration>)>,
    gained: Vec<(String, Vec<Declaration>)>,
}

/// Compares a replacement's declarations, `now`, with those of the module it
/// replaces: what it filled and changed, or why it can't replace it.
///
/// The declarations are compared by their hashes, and a hash names the
/// constants a type or value uses without saying what they are. When the
/// replacement builds on every snapshot the replaced module does, each of
/// those constants is the same, as Lean refuses an import that declares a
/// name again and Mathlib's are fixed by the version. Otherwise the
/// comparison follows what the replaced declarations use into the snapshots
/// it lost (see [`walk`]).
fn compare(
    replacing: &Replacing,
    now: &[Declaration],
    swapped: &Swapped,
) -> Result<Replacement, Vec<String>> {
    let allowed = |name: &str| allows(replacing.replaced.allow.as_ref(), name);
    let (values, changed): (Vec<_>, Vec<_>) = statement_changes(&replacing.declarations, now)
        .into_iter()
        .partition(|c| c.change == Change::Value && allowed(&c.name));
    let mut refused: Vec<String> = changed.iter().map(ToString::to_string).collect();
    let mut changed_definitions: Vec<String> = values.into_iter().map(|c| c.name).collect();
    for reached in walk(replacing, now, swapped)? {
        if reached.change == Change::Value && allowed(&reached.name) {
            changed_definitions.push(reached.name);
        } else {
            refused.push(reached.to_string());
        }
    }
    refused.extend(added_assumptions(replacing, now, swapped));
    if !refused.is_empty() {
        return Err(refused);
    }
    let affected = if changed_definitions.is_empty() {
        Vec::new()
    } else {
        let gained = swapped.gained.iter().flat_map(|(_, d)| d);
        let names: HashSet<&str> = now.iter().map(|d| d.name.as_str()).collect();
        let used = changed_definitions.iter().cloned().collect();
        let (_, theorems) = uses(&used, now.iter().chain(gained));
        theorems
            .into_iter()
            .filter(|t| names.contains(t.as_str()))
            .collect()
    };
    let uses_sorry = |d: &Declaration| d.axioms.iter().any(|a| a == "sorryAx");
    let filled = replacing
        .declarations
        .iter()
        .filter(|d| {
            let name = normalised(&d.name);
            uses_sorry(d)
                && now
                    .iter()
                    .any(|n| normalised(&n.name) == name && !uses_sorry(n))
        })
        .map(|d| d.name.clone())
        .collect();
    Ok(Replacement {
        filled,
        changed_definitions,
        affected,
    })
}

/// Whether `allow` lets the definition `name` change its value: it names it,
/// both as [`normalised`].
fn allows(allow: Option<&Allow>, name: &str) -> bool {
    let name = normalised(name);
    allow.is_some_and(|a| a.definitions.iter().any(|n| normalised(n) == name))
}

/// The definitions, declarations with a value, that a replacement's `allow`
/// may name: the replaced module's, and those of the snapshots it no longer
/// builds on.
fn allowable<'a>(
    replacing: &'a Replacing,
    swapped: &'a Swapped,
) -> impl Iterator<Item = &'a Declaration> {
    let lost = swapped.lost.iter().flat_map(|(_, d)| d);
    replacing
        .declarations
        .iter()
        .chain(lost)
        .filter(|d| d.value_hash.is_some())
}

/// The error for a replacement whose `allow` names something other than a
/// definition it may change (see [`allowable`]), by [`normalised`] name.
fn allow_error(replacing: &Replacing, swapped: &Swapped) -> Option<Error> {
    let allow = replacing.replaced.allow.as_ref()?;
    let names: HashSet<_> = allowable(replacing, swapped)
        .map(|d| normalised(&d.name))
        .collect();
    let name = allow
        .definitions
        .iter()
        .find(|n| !names.contains(&normalised(n)))?;
    let private = format!("_private.{}", normalised(name));
    let hint = if names.contains(private.as_str()) {
        format!(" (for a private definition, write {private})")
    } else {
        String::new()
    };
    Some(Error::new(
        ErrorKind::BadRequest,
        format!(
            "allow names {name}, which isn't a definition of the replaced snapshot or of a \
             snapshot the code no longer builds on{hint}"
        ),
    ))
}

/// Whether Lean made `name` rather than the code, such as the axiom
/// `native_decide` declares: an internal name.
fn lean_made(name: &str) -> bool {
    let name = normalised(name);
    let user = name.strip_prefix("_private.").unwrap_or(&name);
    user.split('.').any(|part| part.starts_with('_'))
}

/// The assumptions `decls` declare: their `axiom`s other than Lean's, by
/// name across modules, with their type hash.
fn declared_assumptions<'a>(
    decls: impl IntoIterator<Item = &'a Declaration>,
) -> HashMap<String, &'a str> {
    decls
        .into_iter()
        .filter(|d| d.kind == "axiom" && !lean_made(&d.name))
        .map(|d| (normalised(&d.name).into_owned(), d.type_hash.as_str()))
        .collect()
}

/// A replacement keeps the replaced module's assumptions but adds none: each
/// assumption that it, or a snapshot it builds on and the replaced module
/// doesn't, declares must be declared by the replaced module or a snapshot
/// it no longer builds on, with the same statement. The snapshots both build
/// on are the same. Otherwise a `sorry` could be "filled" by assuming it.
fn added_assumptions(replacing: &Replacing, now: &[Declaration], swapped: &Swapped) -> Vec<String> {
    let lost = swapped.lost.iter().flat_map(|(_, d)| d);
    let before = declared_assumptions(replacing.declarations.iter().chain(lost));
    // The replaced module's own are compared as its statements.
    let own = declared_assumptions(&replacing.declarations);
    let gained = swapped.gained.iter().flat_map(|(_, d)| d);
    let mut reasons: Vec<String> = declared_assumptions(now.iter().chain(gained))
        .into_iter()
        .filter_map(|(name, statement)| match before.get(&name) {
            Some(was) if *was == statement => None,
            Some(_) if own.contains_key(&name) => None,
            Some(_) => Some(format!(
                "statement: the assumption {name} has a different statement"
            )),
            None => Some(format!("statement: the code adds the assumption {name}")),
        })
        .collect();
    reasons.sort();
    reasons
}

/// A constant of a snapshot that a replacement no longer builds on, reached
/// from what the replaced module's declarations use, that the replacement
/// doesn't keep as it was.
#[derive(Debug, PartialEq, Eq)]
struct Reached {
    name: String,
    module: String,
    change: Change,
}

impl std::fmt::Display for Reached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let module = snapshot::module(&self.module);
        let what = self.change.says();
        write!(f, "statement: {}, from {module}, {what}", self.name)
    }
}

/// What `d` uses: its type's refs, and its value's for a definition or an
/// inductive.
fn refs_of(d: &Declaration) -> Vec<&String> {
    let mut refs: Vec<&String> = d.type_refs.iter().collect();
    if d.value_hash.is_some() || d.kind == "inductive" {
        refs.extend(&d.value_refs);
    }
    refs
}

/// Follows what the replaced module's declarations use, through types and
/// values, into the snapshots the replacement no longer builds on, and
/// compares each constant reached there with the one of the same name (see
/// [`normalised`]) that the replacement declares or builds on now. What it
/// can't compare, because a private name is ambiguous, is an error: the
/// replacement is refused.
fn walk(
    replacing: &Replacing,
    now: &[Declaration],
    swapped: &Swapped,
) -> Result<Vec<Reached>, Vec<String>> {
    if swapped.lost.is_empty() {
        return Ok(Vec::new());
    }
    // Refs name constants as they are, so the old side is found exactly.
    let old: HashMap<&str, (&str, &Declaration)> = swapped
        .lost
        .iter()
        .flat_map(|(m, decls)| {
            decls
                .iter()
                .map(move |d| (d.name.as_str(), (m.as_str(), d)))
        })
        .collect();
    let mut new: HashMap<std::borrow::Cow<str>, Option<&Declaration>> = HashMap::new();
    for d in now.iter().chain(swapped.gained.iter().flat_map(|(_, d)| d)) {
        new.entry(normalised(&d.name))
            .and_modify(|found| *found = None)
            .or_insert(Some(d));
    }
    let mut todo: Vec<&String> = replacing.declarations.iter().flat_map(refs_of).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let (mut reached, mut ambiguous) = (Vec::new(), Vec::new());
    while let Some(r) = todo.pop() {
        if !seen.insert(r) {
            continue;
        }
        // Neither from a lost snapshot: the replaced module's own, which are
        // compared already, or from a snapshot both build on.
        let Some(&(module, c)) = old.get(r.as_str()) else {
            continue;
        };
        let change = match new.get(&normalised(r)) {
            None => Some(Change::Missing),
            Some(None) => {
                ambiguous.push(format!(
                    "statement: {r}, from {}, matches more than one declaration of the code \
                     and what it builds on",
                    snapshot::module(module)
                ));
                None
            }
            Some(Some(n)) if n.kind != c.kind => Some(Change::Kind),
            Some(Some(n)) if n.type_hash != c.type_hash => Some(Change::Type),
            Some(Some(n)) if n.value_hash != c.value_hash => Some(Change::Value),
            Some(Some(_)) => None,
        };
        if let Some(change) = change {
            reached.push(Reached {
                name: c.name.clone(),
                module: module.to_string(),
                change,
            });
        }
        todo.extend(refs_of(c));
    }
    if ambiguous.is_empty() {
        Ok(reached)
    } else {
        ambiguous.dedup();
        Err(ambiguous)
    }
}

/// What [`Snapshots::build`] kept: the module's record, the new entry, and
/// what the replacement changed.
type Built = Option<(ModuleMeta, (String, EntryMeta), Option<Replacement>)>;

/// Puts what a create's comparison with `replacing` found into its
/// response, if the entry the response has records that comparison: a create
/// that lost a race on its key gets the other create's entry.
fn replaced(
    done: &mut CreateSnapshotResponse,
    replacing: Option<&Replacing>,
    replacement: Option<Replacement>,
) {
    if let (Some(replacing), Some(r)) = (replacing, replacement)
        && done
            .snapshot
            .as_ref()
            .is_some_and(|s| s.replaces.as_ref() == Some(&replacing.replaced))
    {
        done.replacement = Some(r);
    }
}

/// What among `items` uses a name in `used`, directly or through each other,
/// by the refs they record: the declarations other than theorems whose type
/// or value does, which then count as used too, and the theorems whose type
/// does. Names are matched as [`normalised`].
fn uses<'a, I>(used: &HashSet<String>, items: I) -> (Vec<String>, Vec<String>)
where
    I: IntoIterator<Item = &'a Declaration>,
    I::IntoIter: Clone,
{
    let items = items.into_iter();
    let theorem = |d: &Declaration| d.kind == "theorem";
    let mut names: HashSet<String> = used.iter().map(|u| normalised(u).into_owned()).collect();
    let mut users: Vec<String> = Vec::new();
    let is_used = |r: &String, names: &HashSet<String>| names.contains(normalised(r).as_ref());
    loop {
        let before = users.len();
        for d in items.clone().filter(|d| !theorem(d)) {
            let refs = d.type_refs.iter().chain(&d.value_refs);
            if !names.contains(normalised(&d.name).as_ref())
                && refs.clone().any(|r| is_used(r, &names))
            {
                names.insert(normalised(&d.name).into_owned());
                users.push(d.name.clone());
            }
        }
        if users.len() == before {
            break;
        }
    }
    let theorems = items
        .filter(|d| theorem(d) && d.type_refs.iter().any(|r| is_used(r, &names)))
        .map(|d| d.name.clone())
        .collect();
    (users, theorems)
}

/// The modules of `above` (see [`Catalog::dependents`]) to rebuild for
/// `matching`: those, and the ones among `above` they import, directly or
/// through others.
fn needed(
    above: &BTreeMap<String, Vec<String>>,
    matching: impl IntoIterator<Item = String>,
) -> BTreeSet<String> {
    let mut needed = BTreeSet::new();
    let mut todo: Vec<String> = matching.into_iter().collect();
    while let Some(m) = todo.pop() {
        if let Some(imports) = above.get(&m)
            && needed.insert(m)
        {
            todo.extend(imports.iter().cloned());
        }
    }
    needed
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The files `lean -o` wrote for module `s<id>` in `dir`, by extension, the
/// `.olean` last.
fn outputs(dir: &Path, id: &str) -> std::io::Result<Vec<(String, PathBuf)>> {
    let prefix = format!("s{id}.");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(ext) = name.strip_prefix(&prefix)
            && ext != "lean"
        {
            files.push((ext.to_string(), path.clone()));
        }
    }
    files.sort_by_key(|(ext, _)| (ext == "olean", ext.clone()));
    Ok(files)
}

/// The snapshots among the `.olean` files `lean --deps` printed, by id.
fn snapshot_imports(paths: &[PathBuf]) -> Result<BTreeSet<String>, Error> {
    let mut ids = BTreeSet::new();
    for path in paths {
        let parent = path.parent().and_then(Path::file_name);
        if parent != Some(std::ffi::OsStr::new(snapshot::ROOT)) {
            continue;
        }
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        match snapshot::parse_module(&format!("{}.{stem}", snapshot::ROOT)) {
            Ok(Some(id)) => {
                ids.insert(id);
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::BadRequest,
                    format!(
                        "Lean reads an import that isn't a snapshot: {}",
                        path.display()
                    ),
                ));
            }
        }
    }
    Ok(ids)
}

/// Removes every file of module `s<id>` from `dir`.
fn remove_module(dir: &Path, id: &str) {
    let prefix = format!("s{id}.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What re-checking one module of a chain came to.
#[derive(Debug, PartialEq)]
enum Recheck {
    Passed,
    AlreadyVerified,
    /// The module was deleted and built again since the chain was read, so
    /// this run doesn't verify it.
    Changed,
    Failed(String),
}

/// Every module a snapshot needs: its imports' modules, then its own.
fn modules(parents: &[ModuleMeta], own: Module) -> Vec<Module> {
    let mut modules: Vec<Module> = Vec::new();
    for m in parents.iter().flat_map(|p| &p.modules) {
        if !modules.contains(m) {
            modules.push(m.clone());
        }
    }
    modules.push(own);
    modules
}

/// The level of each module in `above` (see [`Catalog::dependents`]) over
/// `root`, which is 0: one more than the highest of its imports among them.
fn levels(above: &BTreeMap<String, Vec<String>>, root: &str) -> HashMap<String, usize> {
    let mut levels = HashMap::from([(root.to_string(), 0)]);
    while levels.len() <= above.len() {
        let before = levels.len();
        for (m, imports) in above {
            if levels.contains_key(m) {
                continue;
            }
            let known: Option<Vec<usize>> =
                imports.iter().map(|i| levels.get(i).copied()).collect();
            if let Some(known) = known {
                levels.insert(m.clone(), 1 + known.into_iter().max().unwrap_or(0));
            }
        }
        // Imports always lead down to `root`, so each pass places some.
        if levels.len() == before {
            break;
        }
    }
    levels
}

/// A rebuild item before its level: when its snapshot was made (or its
/// module, for an item without one), and its old module.
struct Planned {
    created: u64,
    module: String,
    item: RebuildItem,
}

/// The level of each item of a rebuild plan (see [`RebuildItem::level`]):
/// one more than the highest among the items on the modules it imports,
/// `imports[i]`.
fn waves(imports: &[Vec<usize>]) -> Vec<usize> {
    let mut waves: Vec<Option<usize>> = vec![None; imports.len()];
    // Imports always lead down to `from`, so each pass places some.
    while waves.contains(&None) {
        for i in 0..waves.len() {
            if waves[i].is_some() {
                continue;
            }
            let below = imports[i].iter().map(|&j| waves[j]);
            if let Some(known) = below.collect::<Option<Vec<usize>>>() {
                waves[i] = Some(1 + known.into_iter().max().unwrap_or(0));
            }
        }
    }
    waves.into_iter().flatten().collect()
}

/// The worst status among a module's declarations, `decls`.
fn worst_status(decls: &[Declaration]) -> ProofStatus {
    let statuses = decls.iter().map(|d| d.status);
    statuses.max().unwrap_or(ProofStatus::Proved)
}

/// The ids of `modules`.
fn ids(modules: &[Module]) -> impl Iterator<Item = &String> {
    modules.iter().map(|m| &m.id)
}

/// The error for a create that asked to verify an import or existing snapshot
/// and couldn't.
fn failure_error(f: &VerifyFailure) -> Error {
    Error::new(
        ErrorKind::Lean,
        format!("{} failed verification: {}", f.module, f.reason),
    )
}

/// Re-checks the module through the kernel in a clean process, and returns its
/// declarations with their axioms from there, so that they don't depend on
/// whatever ran during the build. The error is why the re-check failed.
async fn recheck_module(slot: &Lease, id: &str) -> Result<Vec<ReportedDeclaration>, String> {
    let verified = slot.verify(&snapshot::module(id), REPORT_TIMEOUT).await;
    if !verified.ok {
        return Err(verified
            .reason
            .unwrap_or_else(|| "snapshot rejected".into()));
    }
    parse_declarations(verified.declarations)
        .ok_or_else(|| "unreadable verifier declarations".to_string())
}

/// Runs one of the worker's own reports in environment `env` and parses it;
/// `what` names it in the error.
async fn run_report<T>(
    slot: &mut Lease,
    text: String,
    env: u64,
    what: &str,
    parse: impl Fn(&CommandResponse) -> Option<T>,
) -> Result<T, Error> {
    let cmd = ReplCommand::cmd(text, Some(env));
    let report = match run(slot, &cmd, REPORT_TIMEOUT).await? {
        ReplResponse::Command(r) => r,
        ReplResponse::Error(e) => return Err(Error::new(ErrorKind::Lean, e.message)),
        other => return Err(unexpected(other)),
    };
    parse(&report).ok_or_else(|| {
        let why = report
            .messages
            .iter()
            .find(|m| m.severity == Severity::Error)
            .map_or("no output".to_string(), |m| m.data.clone());
        internal(format!("the {what} failed: {why}"))
    })
}

/// The declarations of a module in a process that has it imported, with their
/// axioms as that process reports them.
async fn report_module(
    slot: &mut Lease,
    id: &str,
    env: u64,
) -> Result<Vec<ReportedDeclaration>, Error> {
    let text = module_report(&snapshot::module(id));
    run_report(slot, text, env, "axiom report", parse_report).await
}

/// What the search service indexes of a module's declarations, from a
/// process that has it imported.
async fn export_module(slot: &mut Lease, id: &str, env: u64) -> Result<Vec<SnapshotDecl>, Error> {
    let text = module_export(&snapshot::module(id));
    run_report(slot, text, env, "declaration export", parse_export).await
}

impl Snapshots {
    /// `root` is the search-path root of snapshot modules; it must exist and
    /// contain an empty `PisaSnap/`. A lookup in the store vouches for a
    /// cached snapshot for `confirm_every`.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        db: Db,
        version: String,
        root: PathBuf,
        confirm_every: Duration,
        policy: Policy,
    ) -> Self {
        Snapshots {
            store,
            catalog: Catalog::new(db, version.clone()),
            version,
            root,
            confirm_every,
            cached: Mutex::default(),
            confirmed: Mutex::default(),
            locks: Mutex::default(),
            policy,
        }
    }

    /// Where the store keeps file `ext` of a module build.
    fn blob(&self, module: &Module, ext: &str) -> ObjectPath {
        ObjectPath::from(format!(
            "snapshots/{}/{}.{}.{ext}.zst",
            self.version, module.id, module.build
        ))
    }

    fn dir(&self) -> PathBuf {
        self.root.join(snapshot::ROOT)
    }

    fn file(&self, id: &str, ext: &str) -> PathBuf {
        self.dir().join(format!("s{id}.{ext}"))
    }

    fn lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    /// Makes every snapshot `header` imports importable by this worker's Lean
    /// processes, except `pending`, which the caller is building.
    pub async fn ensure(&self, header: &str, pending: Option<&str>) -> Result<(), Error> {
        let ids = snapshot::imports(header).map_err(|e| Error::new(ErrorKind::BadRequest, e))?;
        for id in ids.iter().filter(|id| Some(id.as_str()) != pending) {
            self.ensure_one(id).await?;
        }
        Ok(())
    }

    async fn ensure_one(&self, id: &str) -> Result<(), Error> {
        let lock = self.lock(id);
        let _loading = lock.lock().await;
        let fresh = self
            .confirmed
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|t| t.elapsed() < self.confirm_every);
        if fresh {
            return Ok(());
        }
        let meta = match self.catalog.module(id).await {
            Ok(meta) => meta,
            Err(e) => {
                if e.kind == ErrorKind::NotFound {
                    self.discard(id);
                }
                return Err(e);
            }
        };
        for module in &meta.modules {
            self.fetch(module).await?;
        }
        self.confirmed
            .lock()
            .unwrap()
            .insert(id.to_string(), Instant::now());
        Ok(())
    }

    /// Downloads a module build into the local cache unless it is already
    /// there.
    async fn fetch(&self, module: &Module) -> Result<(), Error> {
        let cached = self.cached.lock().unwrap().get(&module.id).cloned();
        if cached.as_ref() == Some(module) {
            return Ok(());
        }
        for file in &module.files {
            let path = self.blob(module, &file.ext);
            let compressed = match self.store.get(&path).await {
                Ok(r) => r.bytes().await.map_err(internal)?,
                Err(object_store::Error::NotFound { .. }) => {
                    return Err(unknown_module(&module.id));
                }
                Err(e) => return Err(internal(e)),
            };
            let bytes = zstd::decode_all(&compressed[..]).map_err(internal)?;
            if sha256(&bytes) != file.sha256 {
                return Err(internal(format!(
                    "snapshot {} is corrupt in storage",
                    snapshot::module(&module.id)
                )));
            }
            // Lean processes may have the old file mapped: replace it, don't
            // overwrite it.
            let dest = self.file(&module.id, &file.ext);
            let tmp = self.file(
                &module.id,
                &format!("tmp.{}", uuid::Uuid::new_v4().simple()),
            );
            tokio::fs::write(&tmp, &bytes).await.map_err(internal)?;
            tokio::fs::rename(&tmp, &dest).await.map_err(internal)?;
        }
        self.cached
            .lock()
            .unwrap()
            .insert(module.id.clone(), module.clone());
        Ok(())
    }

    /// Forgets a snapshot on this worker. Processes that imported it keep it.
    fn discard(&self, id: &str) {
        remove_module(&self.dir(), id);
        self.cached.lock().unwrap().remove(id);
        self.confirmed.lock().unwrap().remove(id);
    }

    /// Forgets a module that a delete removed, unless the same code has been
    /// stored again since, maybe by a build on this worker. Then the local
    /// copy may be the removed build, so the next import looks the module up
    /// again.
    async fn forget(&self, id: &str) {
        let lock = self.lock(id);
        let _held = lock.lock().await;
        if self.catalog.module(id).await.is_err() {
            self.discard(id);
        } else {
            self.confirmed.lock().unwrap().remove(id);
        }
    }

    /// Deletes a module build's files from the store, logging any it can't.
    async fn delete_blobs(&self, module: &Module) {
        for file in &module.files {
            let path = self.blob(module, &file.ext);
            match self.store.delete(&path).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => tracing::warn!(%path, "deleting a snapshot file failed: {e}"),
            }
        }
    }

    /// The snapshot id of entry `entry`, as the API answers it.
    fn id(&self, entry: &str) -> String {
        let id = SnapshotId {
            version: self.version.clone(),
            entry: entry.to_string(),
        };
        id.to_string()
    }

    /// The entry snapshot id `id` names, if it is of this worker's version.
    fn parse(&self, id: &str) -> Result<String, Error> {
        let parsed = SnapshotId::parse(id)
            .ok_or_else(|| Error::new(ErrorKind::BadRequest, snapshot_id_error(id)))?;
        if parsed.version != self.version {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("snapshot {id} belongs to version {}", parsed.version),
            ));
        }
        Ok(parsed.entry)
    }

    /// Entry `entry`, on the module `meta` records, as the API answers it.
    fn snapshot(&self, entry: String, entry_meta: EntryMeta, meta: &ModuleMeta) -> Snapshot {
        Snapshot {
            id: self.id(&entry),
            version: self.version.clone(),
            module: snapshot::module(&entry_meta.module),
            header: meta.header.clone(),
            imports: meta.imports.iter().map(|i| snapshot::module(i)).collect(),
            labels: entry_meta.labels,
            replaces: entry_meta.replaces.map(|r| Replaces {
                snapshot_id: self.id(&r.snapshot_id),
                ..r
            }),
            status: meta.status,
            verified: meta.verified,
            created: entry_meta.created_micros / 1_000_000,
        }
    }

    /// The response to a create that added `entry` to the module `meta`
    /// records, or replayed it. What a replacement changed is for the caller
    /// to add: a key replay doesn't compare again.
    fn created(
        &self,
        meta: ModuleMeta,
        (entry, entry_meta): (String, EntryMeta),
    ) -> CreateSnapshotResponse {
        CreateSnapshotResponse {
            snapshot: Some(self.snapshot(entry, entry_meta, &meta)),
            check: meta.result,
            replacement: None,
            heads: Vec::new(),
        }
    }

    /// The response to a create of kept code, the module `meta` records,
    /// that added no entry: the module's check, failed, for the caller to
    /// say why.
    fn refused(&self, meta: ModuleMeta) -> CreateSnapshotResponse {
        let mut check = meta.result;
        check.ok = false;
        CreateSnapshotResponse {
            snapshot: None,
            check,
            replacement: None,
            heads: Vec::new(),
        }
    }

    /// Checks `job.code` by compiling it to a module and keeps the module
    /// unless Lean reported errors, with a new entry on it. Code that is
    /// already kept gets the new entry without being built again. Failures
    /// are reported in `check`. So is a replaced snapshot that another
    /// replaces already, which is refused as a `conflict` before anything is
    /// built, with the run's `heads`, unless the job forks.
    pub async fn create(&self, pool: &Arc<Pool>, job: SnapshotCreateJob) -> CreateSnapshotResponse {
        let mut response = CreateSnapshotResponse {
            snapshot: None,
            check: CheckResult::default(),
            replacement: None,
            heads: Vec::new(),
        };
        let valid =
            check_labels(&job.labels).and_then(|()| job.key.as_deref().map_or(Ok(()), check_key));
        if let Err(e) = valid {
            response.check.error = Some(Error::new(ErrorKind::BadRequest, e));
            return response;
        }
        let split = split_header(&job.code);
        if let Err(e) = snapshot::imports(&split.header) {
            response.check.error = Some(Error::new(ErrorKind::BadRequest, e));
            return response;
        }
        // Macros are allowed: `build` re-checks them through the kernel unless
        // the request opts out, which marks the snapshot as not verified.
        let rejected = pool.policy().scan(&split.body, true);
        if !rejected.is_empty() {
            response.check.error = Some(policy_error(&rejected));
            response.check.reasons = rejected;
            return response;
        }
        if let Some(key) = &job.key {
            match self.replay(key, &snapshot::id(&job.code)).await {
                Ok(Some(done)) => return done,
                Ok(None) => {}
                Err(e) => {
                    response.check.error = Some(e);
                    return response;
                }
            }
        }
        let replacing = match &job.replaces {
            Some(r) => match self.replacing(r).await {
                Ok(r) => Some(r),
                Err(e) => {
                    response.check.error = Some(e);
                    return response;
                }
            },
            None => None,
        };
        if let Some(r) = replacing.as_ref().filter(|_| !job.fork) {
            match self.replaced_by(&r.entry).await {
                Ok(Some((error, heads))) => {
                    response.check.error = Some(error);
                    response.heads = heads;
                    return response;
                }
                Ok(None) => {}
                Err(e) => {
                    response.check.error = Some(e);
                    return response;
                }
            }
        }
        // A kept module removed before its new entry was added is built
        // again, once.
        for _ in 0..2 {
            let done = self
                .create_once(pool, &job, &split, replacing.as_ref(), response.clone())
                .await;
            if let Some(mut done) = done {
                // Another create replaced it while this one was building.
                if let Some(r) = &replacing
                    && done
                        .check
                        .error
                        .as_ref()
                        .is_some_and(|e| e.kind == ErrorKind::Conflict)
                    && let Ok(Some((error, heads))) = self.replaced_by(&r.entry).await
                {
                    done.check.error = Some(error);
                    done.heads = heads;
                }
                return done;
            }
        }
        response.check.error = Some(internal(
            "the snapshot was deleted twice while it was being created",
        ));
        response
    }

    /// The entry `r` names, with its module's declarations.
    async fn replacing(&self, r: &Replaces) -> Result<Replacing, Error> {
        let entry = self.parse(&r.snapshot_id)?;
        let entry_meta = self.catalog.entry(&entry).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        Ok(Replacing {
            replaced: r.clone(),
            entry,
            declarations: meta.result.declarations,
            needs: ids(&meta.modules)
                .filter(|m| **m != entry_meta.module)
                .cloned()
                .collect(),
        })
    }

    /// The refusal of a create that replaces `entry` when another entry
    /// replaces it already: the error, which names the replacers and the
    /// run's heads from there, and the heads. `None` if `entry` is a head.
    async fn replaced_by(&self, entry: &str) -> Result<Option<(Error, Vec<String>)>, Error> {
        let replacers = self.catalog.replacers(entry).await?;
        if replacers.is_empty() {
            return Ok(None);
        }
        let heads = self.heads_from(entry).await?;
        let replacers: Vec<String> = replacers.iter().map(|(r, _)| self.id(r)).collect();
        let message = format!(
            "{} is replaced by {}; the run's heads from there are {}. Replace a head, or set fork",
            self.id(entry),
            replacers.join(", "),
            heads.join(", ")
        );
        Ok(Some((Error::new(ErrorKind::Conflict, message), heads)))
    }

    /// The snapshot ids of the heads of the run from `entry`: the entries
    /// that nothing replaces, reached from `entry` through what replaces it,
    /// oldest first.
    async fn heads_from(&self, entry: &str) -> Result<Vec<String>, Error> {
        let created = self.catalog.entry(entry).await?.created_micros;
        let mut todo = vec![(entry.to_string(), created)];
        let mut heads = Vec::new();
        while let Some((e, created)) = todo.pop() {
            let replacers = self.catalog.replacers(&e).await?;
            if replacers.is_empty() {
                heads.push((created, e));
            }
            todo.extend(replacers.into_iter().map(|(r, m)| (r, m.created_micros)));
        }
        heads.sort();
        Ok(heads.into_iter().map(|(_, e)| self.id(&e)).collect())
    }

    /// The snapshot modules a replacement of `replacing` that builds on
    /// `needs` (its own module, `own`, among them) swaps; see [`Swapped`].
    async fn swapped(
        &self,
        replacing: &Replacing,
        needs: &[String],
        own: &str,
    ) -> Result<Swapped, Error> {
        self.predicted_swap(replacing, needs, own, &HashMap::new(), &mut HashMap::new())
            .await
    }

    /// [`swapped`](Self::swapped) for a replacement that builds on modules
    /// not made yet: each module `originals` maps is taken to declare what
    /// the module it maps to does. The records it reads are kept in `metas`.
    async fn predicted_swap(
        &self,
        replacing: &Replacing,
        needs: &[String],
        own: &str,
        originals: &HashMap<String, String>,
        metas: &mut HashMap<String, ModuleMeta>,
    ) -> Result<Swapped, Error> {
        let lost: Vec<&String> = replacing
            .needs
            .iter()
            .filter(|m| !needs.contains(m))
            .collect();
        let gained = needs
            .iter()
            .filter(|m| *m != own && !replacing.needs.contains(m));
        let mut swapped = Swapped::default();
        for (side, modules) in [
            (&mut swapped.lost, lost),
            (&mut swapped.gained, gained.collect()),
        ] {
            for m in modules {
                let read = originals.get(m).unwrap_or(m);
                let decls = self
                    .module_cached(read, metas)
                    .await?
                    .result
                    .declarations
                    .clone();
                side.push((m.clone(), decls));
            }
        }
        Ok(swapped)
    }

    /// The response to a create whose idempotency key an entry already has:
    /// that entry, unchanged, if it is on module `id`, else an error. `None` if
    /// no entry has the key.
    async fn replay(&self, key: &str, id: &str) -> Result<Option<CreateSnapshotResponse>, Error> {
        let Some((entry, entry_meta)) = self.catalog.keyed(key).await? else {
            return Ok(None);
        };
        if entry_meta.module != id {
            return Err(key_in_use());
        }
        let meta = self.catalog.module(id).await?;
        Ok(Some(self.created(meta, (entry, entry_meta))))
    }

    /// [`create`](Self::create) after validation. `None` if the module was
    /// kept but removed before the entry could be added, or during the
    /// re-check the request asked for.
    async fn create_once(
        &self,
        pool: &Arc<Pool>,
        job: &SnapshotCreateJob,
        split: &Split,
        replacing: Option<&Replacing>,
        mut response: CreateSnapshotResponse,
    ) -> Option<CreateSnapshotResponse> {
        let id = snapshot::id(&job.code);
        let lock = self.lock(&id);
        let building = lock.lock().await;
        match self.catalog.module(&id).await {
            Ok(meta) => {
                drop(building);
                return self
                    .add_to_kept(pool, &id, meta, job, replacing, response)
                    .await;
            }
            Err(e) if e.kind == ErrorKind::NotFound => {}
            Err(e) => {
                response.check.error = Some(e);
                return Some(response);
            }
        }
        if job.verify == Some(true) {
            for import in snapshot::imports(&split.header).unwrap_or_default() {
                let error = match self.verify_chain(pool, &import).await {
                    Ok((_, Some(failure))) => failure_error(&failure),
                    Ok((_, None)) => continue,
                    Err(e) => e,
                };
                response.check.error = Some(error);
                return Some(response);
            }
        }
        let header = snapshot::next_header(&split.header, &id);
        let mut slot = pool.acquire(&header_key(&header), None).await;
        let built = self
            .build(&mut slot, job, split, replacing, &id, &header)
            .await;
        slot.finish_job();
        Some(match built {
            Ok(Some((meta, entry, replacement))) => {
                let mut done = self.created(meta, entry);
                replaced(&mut done, replacing, replacement);
                done
            }
            Ok(None) => response,
            Err(outcome) => {
                response.check = (*outcome).into();
                response
            }
        })
    }

    /// A create for code that is already kept: it verifies the module's chain
    /// first if the request asks for that and the module isn't verified, then
    /// adds the entry. Its check is what is stored. If the re-check fails,
    /// the check has the error and no entry is added, as for new code that
    /// fails it. `None` if the module is gone by the time it is needed, or
    /// was built again during the re-check.
    async fn add_to_kept(
        &self,
        pool: &Arc<Pool>,
        id: &str,
        meta: ModuleMeta,
        job: &SnapshotCreateJob,
        replacing: Option<&Replacing>,
        mut response: CreateSnapshotResponse,
    ) -> Option<CreateSnapshotResponse> {
        let meta = if job.verify == Some(true) && !meta.verified {
            let verified = async {
                let (_, failure) = self.verify_chain(pool, id).await?;
                Ok::<_, Error>((self.catalog.module(id).await?, failure))
            };
            match verified.await {
                // Nothing failed, so the chain stopped at a module built again.
                Ok((meta, None)) if !meta.verified => return None,
                Ok((meta, None)) => meta,
                Ok((meta, Some(failure))) => {
                    let mut refused = self.refused(meta);
                    refused.check.error = Some(failure_error(&failure));
                    return Some(refused);
                }
                Err(e) => {
                    let gone = e.kind == ErrorKind::NotFound
                        && matches!(self.catalog.module(id).await, Err(e) if e.kind == ErrorKind::NotFound);
                    if gone {
                        return None;
                    }
                    response.check.error = Some(e);
                    return Some(response);
                }
            }
        } else {
            meta
        };
        let replacement = match replacing {
            Some(r) => {
                let needs: Vec<String> = ids(&meta.modules).cloned().collect();
                let swapped = match self.swapped(r, &needs, id).await {
                    Ok(swapped) => swapped,
                    Err(e) => {
                        response.check.error = Some(e);
                        return Some(response);
                    }
                };
                if let Some(e) = allow_error(r, &swapped) {
                    response.check.error = Some(e);
                    return Some(response);
                }
                match compare(r, &meta.result.declarations, &swapped) {
                    Ok(replacement) => Some(replacement),
                    Err(reasons) => {
                        let mut refused = self.refused(meta);
                        refused.check.reasons.extend(reasons);
                        return Some(refused);
                    }
                }
            }
            None => None,
        };
        let entry = NewEntry {
            labels: job.labels.clone(),
            key: job.key.clone(),
            replaces: replacing.map(Replacing::link),
            replacement: replacement.clone(),
            fork: job.fork,
        };
        let added = match self.catalog.add_entry(id, entry).await {
            Ok(Some(added)) => added,
            Ok(None) => return None,
            Err(e) => {
                response.check.error = Some(e);
                return Some(response);
            }
        };
        // Another create took the key for other code since `create` looked.
        if added.1.module != id {
            response.check.error = Some(key_in_use());
            return Some(response);
        }
        let mut done = self.created(meta, added);
        replaced(&mut done, replacing, replacement);
        Some(done)
    }

    /// Compiles, judges and stores the snapshot with a new entry, once it
    /// compares well with the entry it replaces, if any. Returns its record,
    /// the entry and what the replacement changed, `None` if nothing could be
    /// checked (the cause is in the result), or the outcome of a check that
    /// failed.
    async fn build(
        &self,
        slot: &mut Lease,
        job: &SnapshotCreateJob,
        split: &Split,
        replacing: Option<&Replacing>,
        id: &str,
        header: &str,
    ) -> Result<Built, Box<CheckOutcome>> {
        let mut outcome = CheckOutcome {
            time: 0.0,
            error: None,
            response: None,
            verification: None,
        };
        let fail = |mut outcome: CheckOutcome, error: Error| {
            outcome.error = Some(error);
            Err(Box::new(outcome))
        };
        if let Err(e) = self.ensure(&split.header, None).await {
            return fail(outcome, e);
        }
        let parents = match self.parents(&split.header).await {
            Ok(parents) => parents,
            Err(e) => return fail(outcome, e),
        };
        let macros = uses_macros(&split.body) || parents.iter().any(|p| p.macros);
        let recheck = job.verify.unwrap_or(macros);
        // The snapshots a replacement swaps follow from its imports, so a
        // misnamed `allow` costs no build.
        let swapped = match replacing {
            Some(r) => {
                let mut needs: Vec<String> = Vec::new();
                for m in parents.iter().flat_map(|p| ids(&p.modules)) {
                    if !needs.contains(m) {
                        needs.push(m.clone());
                    }
                }
                needs.push(id.to_string());
                let swapped = match self.swapped(r, &needs, id).await {
                    Ok(swapped) => swapped,
                    Err(e) => return fail(outcome, e),
                };
                if let Some(e) = allow_error(r, &swapped) {
                    return fail(outcome, e);
                }
                Some(swapped)
            }
            None => None,
        };
        // `lean -o` imports everything again, so free the slot's memory first.
        slot.retire_and_wait("evict").await;

        let root = slot.scratch_dir().join("build");
        let dir = root.join(snapshot::ROOT);
        let file = dir.join(format!("s{id}.lean"));
        let olean = file.with_extension("olean");
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            return fail(outcome, internal(e));
        }
        if let Err(e) = tokio::fs::write(&file, &job.code).await {
            return fail(outcome, internal(e));
        }
        let limit = slot.import_timeout() + Duration::from_secs(job.timeout_secs);
        let start = Instant::now();
        let compiled = slot.compile(&root, &file, &olean, limit).await;
        outcome.time = (start.elapsed().as_secs_f64() * 1e6).round() / 1e6;
        let compiled = match compiled {
            Ok(c) => c,
            Err(e) => {
                remove_module(&dir, id);
                return fail(outcome, internal(format!("failed to start Lean: {e}")));
            }
        };
        // The file is the submitted code, so positions need no shifting.
        let response = CommandResponse {
            env: 0,
            messages: compiled.messages,
            sorries: Vec::new(),
            tactics: Vec::new(),
            at: Vec::new(),
            infotree: None,
        };
        let errors = response.has_errors();
        outcome.response = Some(response);
        let built = match compiled.status {
            None => Err(Error::new(
                ErrorKind::Timeout,
                format!("building took more than {}s", limit.as_secs()),
            )),
            Some(_) if compiled.oom_kills > 0 => Err(Error::new(
                ErrorKind::OutOfMemory,
                "Lean exceeded its memory limit",
            )),
            Some(status) if status.success() || errors => Ok(()),
            Some(status) => Err(Error::new(
                ErrorKind::Crash,
                format!("Lean failed ({status}): {}", compiled.stderr.trim()),
            )),
        };
        if let Err(e) = built {
            remove_module(&dir, id);
            return fail(outcome, e);
        }
        if errors {
            remove_module(&dir, id);
            let judged = slot
                .policy()
                .judge(outcome.response.as_ref().unwrap(), None);
            outcome.verification = Some(judged);
            return Err(Box::new(outcome));
        }
        // What the module imports is what Lean's own header parser read,
        // which can differ from the line-by-line split the record holds.
        let read = match slot.deps(&file, limit).await {
            Ok(paths) => snapshot_imports(&paths),
            Err(e) => Err(internal(format!("reading the imports failed: {e}"))),
        };
        let listed = snapshot::imports(&split.header).unwrap_or_default();
        if let Err(e) = read.and_then(|read| {
            let listed: BTreeSet<String> = listed.into_iter().collect();
            (read == listed).then_some(()).ok_or_else(|| {
                Error::new(
                    ErrorKind::BadRequest,
                    "Lean reads other snapshot imports than the header's lines say: \
                     put each import on its own line",
                )
            })
        }) {
            remove_module(&dir, id);
            return fail(outcome, e);
        }

        // Judge it in the slot that will serve code on top of it.
        let build = uuid::Uuid::new_v4().simple().to_string();
        let installed = self.install(id, build, &dir).await;
        remove_module(&dir, id);
        let (own, contents) = match installed {
            Ok(installed) => installed,
            Err(e) => {
                self.discard(id);
                return fail(outcome, e);
            }
        };
        let rechecked = if recheck {
            match recheck_module(slot, id).await {
                Ok(decls) => Some(decls),
                Err(reason) => {
                    self.unbuild(slot, id);
                    return fail(outcome, Error::new(ErrorKind::Lean, reason));
                }
            }
        } else {
            None
        };

        // Keep the builder warm on the snapshot so the next checks are hits.
        let env = match slot
            .warm_up_with(header, &header_key(header), Some(id))
            .await
        {
            Ok((env, _)) => env,
            Err(f) => {
                self.unbuild(slot, id);
                return fail(outcome, header_error(f, slot.import_timeout()));
            }
        };
        // The verifier's declarations, which don't depend on what ran in the
        // build, or else the build's report.
        let decls = match rechecked {
            Some(decls) => decls,
            None => match report_module(slot, id, env).await {
                Ok(decls) => decls,
                Err(e) => {
                    self.unbuild(slot, id);
                    return fail(outcome, e);
                }
            },
        };
        let mut verification = slot.policy().judge_decls(false, Some(decls));
        // For search only, so a failure just leaves the module out of it.
        let exported = export_module(slot, id, env)
            .await
            .inspect_err(|e| tracing::warn!(module = %id, "exporting declarations failed: {e}"))
            .ok();
        let status = worst_status(&verification.declarations);
        let replacement = match replacing.zip(swapped.as_ref()) {
            Some((r, swapped)) => match compare(r, &verification.declarations, swapped) {
                Ok(replacement) => Some(replacement),
                Err(reasons) => {
                    self.unbuild(slot, id);
                    verification.ok = false;
                    verification.reasons.extend(reasons);
                    outcome.verification = Some(verification);
                    return Err(Box::new(outcome));
                }
            },
            None => None,
        };
        outcome.verification = Some(verification);

        let meta = ModuleMeta {
            imports: snapshot::imports(&split.header).unwrap_or_default(),
            modules: modules(&parents, own.clone()),
            header: header.to_string(),
            code: job.code.clone(),
            status,
            verified: recheck && parents.iter().all(|p| p.verified),
            macros,
            result: outcome.clone().into(),
            created: now(),
        };
        let search = exported
            .map(|decls| SnapshotDecls::new(&meta, decls))
            .filter(|d| {
                record_fits(d) || {
                    tracing::warn!(
                        module = %id,
                        "declarations too large, or with a NUL character, to index for search"
                    );
                    false
                }
            });
        let entry = NewEntry {
            labels: job.labels.clone(),
            key: job.key.clone(),
            replaces: replacing.map(Replacing::link),
            replacement: replacement.clone(),
            fork: job.fork,
        };
        match self
            .commit(&own, &contents, &meta, search.as_ref(), entry)
            .await
        {
            Ok((meta, entry)) => Ok(Some((meta, entry, replacement))),
            Err(e) => {
                self.unbuild(slot, id);
                fail(outcome, e)
            }
        }
    }

    /// Moves a freshly built module's files from `dir` into the local cache,
    /// not yet confirmed, and returns the build with the files' contents.
    async fn install(
        &self,
        id: &str,
        build: String,
        dir: &Path,
    ) -> Result<(Module, Vec<Vec<u8>>), Error> {
        let outputs = outputs(dir, id).map_err(internal)?;
        if !outputs.iter().any(|(ext, _)| ext == "olean") {
            return Err(internal("Lean wrote no .olean"));
        }
        let mut files = Vec::new();
        let mut contents = Vec::new();
        for (ext, path) in outputs {
            let bytes = tokio::fs::read(&path).await.map_err(internal)?;
            tokio::fs::rename(&path, self.file(id, &ext))
                .await
                .map_err(internal)?;
            files.push(ModuleFile {
                ext,
                sha256: sha256(&bytes),
            });
            contents.push(bytes);
        }
        let module = Module {
            id: id.to_string(),
            build,
            files,
        };
        self.cached
            .lock()
            .unwrap()
            .insert(id.to_string(), module.clone());
        Ok((module, contents))
    }

    /// Undoes [`install`](Self::install) after a failure, including the slot
    /// that imported the module.
    fn unbuild(&self, slot: &mut Lease, id: &str) {
        slot.retire("snapshot_failed");
        self.discard(id);
    }

    /// The records of the modules `header` imports.
    async fn parents(&self, header: &str) -> Result<Vec<ModuleMeta>, Error> {
        let mut parents = Vec::new();
        for import in snapshot::imports(header).map_err(internal)? {
            parents.push(self.catalog.module(&import).await?);
        }
        Ok(parents)
    }

    /// Re-checks an entry's module and everything it imports through the
    /// kernel, and records each module that passes.
    pub async fn verify(
        &self,
        pool: &Arc<Pool>,
        entry: &str,
    ) -> Result<VerifySnapshotResponse, Error> {
        let entry = self.parse(entry)?;
        let id = self.catalog.entry(&entry).await?.module;
        let (checked, failure) = self.verify_chain(pool, &id).await?;
        // As the entry and its module are now.
        let entry_meta = self.catalog.entry(&entry).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        Ok(VerifySnapshotResponse {
            snapshot: self.snapshot(entry, entry_meta, &meta),
            checked,
            failure,
        })
    }

    /// Walks `meta.modules`, which lists imports before what imports them, so
    /// a module is only marked verified once its imports are. Stops at the
    /// first module that fails, or that was deleted and built again since:
    /// that one isn't verified this run, which isn't an error. Returns the
    /// modules that passed, in order, and the one that failed.
    async fn verify_chain(
        &self,
        pool: &Arc<Pool>,
        id: &str,
    ) -> Result<(Vec<String>, Option<VerifyFailure>), Error> {
        self.ensure_one(id).await?;
        let modules = self.catalog.module(id).await?.modules;
        let mut checked = Vec::new();
        let mut failure = None;
        for module in &modules {
            match self.verify_module(pool, module).await? {
                Recheck::Passed => checked.push(snapshot::module(&module.id)),
                Recheck::AlreadyVerified => {}
                // Whatever imports it was built again too, or is gone.
                Recheck::Changed => break,
                Recheck::Failed(reason) => {
                    failure = Some(VerifyFailure {
                        module: snapshot::module(&module.id),
                        reason,
                    });
                    break;
                }
            }
        }
        Ok((checked, failure))
    }

    /// Re-checks one module build, whose imports must be verified and in the
    /// local cache, unless it is already verified. Its axioms then come from
    /// the re-check, replacing what the build's report said.
    ///
    /// This doesn't take the snapshot's lock, which every check on it needs:
    /// they shouldn't wait for the kernel, and two re-checks of a module agree.
    async fn verify_module(&self, pool: &Arc<Pool>, module: &Module) -> Result<Recheck, Error> {
        if let Some(done) = self.before_recheck(module).await? {
            return Ok(done);
        }
        // No header to prefer: this takes a slot with nothing warm if it can.
        let mut slot = pool.acquire("", None).await;
        // The verifier imports everything again, so free the slot's memory first.
        slot.retire_and_wait("evict").await;
        let verification = match recheck_module(&slot, &module.id).await {
            Ok(decls) => slot.policy().judge_decls(false, Some(decls)),
            Err(reason) => return Ok(Recheck::Failed(reason)),
        };
        drop(slot);
        self.record_recheck(module, &verification).await
    }

    /// Gets a re-check of a module build ready: `Changed` if the module's
    /// record names another build now, `AlreadyVerified` if this build is,
    /// else `None` once the local files, which the verifier reads, are this
    /// build's. They are fetched for a verified build too, as the next
    /// modules of the chain import it.
    async fn before_recheck(&self, module: &Module) -> Result<Option<Recheck>, Error> {
        let meta = self.catalog.module(&module.id).await?;
        if meta.modules.last().map(|own| &own.build) != Some(&module.build) {
            return Ok(Some(Recheck::Changed));
        }
        self.fetch(module).await?;
        Ok(meta.verified.then_some(Recheck::AlreadyVerified))
    }

    /// Records that a module build passed the re-check, with what it found.
    /// A module removed meanwhile stays removed, and one built again keeps
    /// its record and is `Changed`.
    async fn record_recheck(
        &self,
        module: &Module,
        verification: &Verification,
    ) -> Result<Recheck, Error> {
        let updated = self
            .catalog
            .update_module(&module.id, &module.build, |meta| {
                meta.verified = true;
                meta.status = worst_status(&verification.declarations);
                meta.result.ok = verification.ok;
                meta.result.reasons = verification.reasons.clone();
                meta.result.declarations = verification.declarations.clone();
            })
            .await?;
        Ok(match updated {
            Some(_) => Recheck::Passed,
            None => Recheck::Changed,
        })
    }

    /// Uploads a freshly built module's files (`own`, whose files have
    /// `contents`), then stores its record, and `search` with it, with the new
    /// entry. Returns the
    /// stored record and the entry. If another worker stored the module
    /// first, its record and build are kept, and this build's files go. So do
    /// they if the entry's key is on an entry of other code, which is
    /// `bad_request`, with nothing stored, and if the catalog refuses the
    /// record. Only after an `unavailable` store, which may have stored it,
    /// do they stay.
    async fn commit(
        &self,
        own: &Module,
        contents: &[Vec<u8>],
        meta: &ModuleMeta,
        search: Option<&SnapshotDecls>,
        entry: NewEntry,
    ) -> Result<(ModuleMeta, (String, EntryMeta)), Error> {
        for (file, bytes) in own.files.iter().zip(contents) {
            let compressed = zstd::encode_all(&bytes[..], 3).map_err(internal)?;
            self.store
                .put(&self.blob(own, &file.ext), PutPayload::from(compressed))
                .await
                .map_err(internal)?;
        }
        let (entry, entry_meta, stored) =
            match self.catalog.commit(&own.id, meta, search, entry).await {
                Ok(committed) => committed,
                Err(e) => {
                    if e.kind != ErrorKind::Unavailable {
                        self.delete_blobs(own).await;
                    }
                    return Err(e);
                }
            };
        if entry_meta.module != own.id {
            self.delete_blobs(own).await;
            return Err(key_in_use());
        }
        if stored.modules.last() != Some(own) {
            self.delete_blobs(own).await;
            if let Some(kept) = stored.modules.last()
                && let Err(e) = self.fetch(kept).await
            {
                // Not confirmed, so the next import fetches it.
                tracing::warn!(module = %kept.id, "fetching the stored build failed: {e}");
                self.discard(&own.id);
                return Ok((stored, (entry, entry_meta)));
            }
        }
        self.confirmed
            .lock()
            .unwrap()
            .insert(own.id.clone(), Instant::now());
        Ok((stored, (entry, entry_meta)))
    }

    pub async fn get(&self, entry: &str) -> Result<SnapshotDetail, Error> {
        let entry = self.parse(entry)?;
        let entry_meta = self.catalog.entry(&entry).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        let exports = self.exports(&entry_meta.module).await?;
        let snapshot = self.snapshot(entry, entry_meta, &meta);
        Ok(SnapshotDetail {
            snapshot,
            code: meta.code,
            check: meta.result,
            exports,
        })
    }

    /// Module `m`'s exports, each with the description search's leader has
    /// recorded for it, if any.
    async fn exports(&self, m: &str) -> Result<Vec<Export>, Error> {
        let Some(decls) = self.catalog.decls(m).await? else {
            return Ok(Vec::new());
        };
        let mut described: HashMap<String, serde_json::Value> =
            self.catalog.described(m).await?.into_iter().collect();
        let exports = decls.decls.into_iter().map(|d| Export {
            description: described
                .remove(&d.name)
                .and_then(|r| serde_json::from_value::<Informal>(r.get("informal")?.clone()).ok())
                .map(Description::from),
            name: d.name,
            kind: d.kind,
            signature: d.signature,
            docstring: d.docstring,
        });
        Ok(exports.collect())
    }

    /// The entries, newest first, that have every label in `labels`, at most
    /// `limit`, after the cursor `after`, and only heads if `heads`; see
    /// [`Catalog::list`].
    pub async fn list(
        &self,
        labels: &[(String, String)],
        limit: usize,
        after: Option<&str>,
        heads: bool,
    ) -> Result<ListSnapshotsResponse, Error> {
        if !(1..=MAX_LIMIT).contains(&limit) || after.is_some_and(|a| !is_cursor(a)) {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("a listing takes a limit from 1 to {MAX_LIMIT} and a cursor from `next`"),
            ));
        }
        if labels.len() > MAX_LABELS {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!(
                    "{} label filters; a listing takes at most {MAX_LABELS}",
                    labels.len()
                ),
            ));
        }
        let (listed, next) = self.catalog.list(labels, limit, after, heads).await?;
        let mut modules: HashMap<String, Option<ModuleMeta>> = HashMap::new();
        let mut snapshots = Vec::new();
        for (entry, entry_meta) in listed {
            let id = entry_meta.module.clone();
            if !modules.contains_key(&id) {
                let meta = match self.catalog.module(&id).await {
                    Ok(meta) => Some(meta),
                    // Its entries were deleted since they were listed.
                    Err(e) if e.kind == ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                };
                modules.insert(id.clone(), meta);
            }
            if let Some(meta) = &modules[&id] {
                snapshots.push(self.snapshot(entry, entry_meta, meta));
            }
        }
        Ok(ListSnapshotsResponse { snapshots, next })
    }

    /// The entries above `entry`'s module, parents first; see
    /// [`ListDependentsResponse`].
    pub async fn dependents(
        &self,
        entry: &str,
        labels: &[(String, String)],
    ) -> Result<ListDependentsResponse, Error> {
        let entry = self.parse(entry)?;
        if labels.len() > MAX_LABELS {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("{} label filters; at most {MAX_LABELS}", labels.len()),
            ));
        }
        let root = self.catalog.entry(&entry).await?.module;
        let above = self.catalog.dependents(&root).await?;
        let levels = levels(&above, &root);
        let modules: Vec<String> = above.keys().cloned().collect();
        let mut found = self.catalog.entries_on(&modules, labels).await?;
        // Stable, so each level stays oldest first.
        found.sort_by_key(|(_, e)| levels.get(&e.module).copied().unwrap_or(usize::MAX));
        let mut metas: HashMap<String, Option<ModuleMeta>> = HashMap::new();
        let mut snapshots = Vec::new();
        for (entry, entry_meta) in found {
            let id = entry_meta.module.clone();
            if !metas.contains_key(&id) {
                let meta = match self.catalog.module(&id).await {
                    Ok(meta) => Some(meta),
                    // Deleted since it was found.
                    Err(e) if e.kind == ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                };
                metas.insert(id.clone(), meta);
            }
            if let Some(meta) = &metas[&id] {
                snapshots.push(self.snapshot(entry, entry_meta, meta));
            }
        }
        Ok(ListDependentsResponse { snapshots })
    }

    /// Plans the creates that rebuild `job.from`'s dependents on `job.to`;
    /// see [`RebuildSnapshotsResponse`].
    pub async fn rebuild_plan(
        &self,
        job: &SnapshotRebuildJob,
    ) -> Result<RebuildSnapshotsResponse, Error> {
        let bad = |m: &str| Error::new(ErrorKind::BadRequest, m);
        let (from, to) = (self.parse(&job.from)?, self.parse(&job.to)?);
        if job.labels.is_empty() {
            return Err(bad("a rebuild takes labels: which dependents to rebuild"));
        }
        check_labels(&job.labels).map_err(|e| bad(&e))?;
        let to_entry = self.catalog.entry(&to).await?;
        let (old, new) = (self.catalog.entry(&from).await?.module, to_entry.module);
        if old == new {
            return Err(bad(
                "from and to are on the same module: there is nothing to rebuild",
            ));
        }
        let (old_meta, new_meta) = (
            self.catalog.module(&old).await?,
            self.catalog.module(&new).await?,
        );
        if ids(&new_meta.modules).any(|m| *m == old) {
            return Err(bad(
                "to builds on from's module, so it can't take its place",
            ));
        }

        let now = &new_meta.result.declarations;
        // `to` compared as a replacement of `from` would be.
        let replacing = |allow| Replacing {
            replaced: Replaces {
                snapshot_id: job.from.clone(),
                allow,
            },
            entry: from.clone(),
            declarations: old_meta.result.declarations.clone(),
            needs: ids(&old_meta.modules)
                .filter(|m| **m != old)
                .cloned()
                .collect(),
        };
        let strict = replacing(None);
        let needs: Vec<String> = ids(&new_meta.modules).cloned().collect();
        let swapped = self.swapped(&strict, &needs, &new).await?;
        // A replacement's create compared them already.
        let link = to_entry.replaces.filter(|r| r.snapshot_id == from);
        let changes = match link {
            Some(_) => Vec::new(),
            None => compare(&strict, now, &swapped).err().unwrap_or_default(),
        };
        // Under its own `allow` if `to` replaces `from`, else any definition.
        let allow = match &link {
            Some(link) => link.allow.clone(),
            None => Some(Allow {
                definitions: allowable(&strict, &swapped)
                    .map(|d| d.name.clone())
                    .collect(),
            }),
        };
        let compared = compare(&replacing(allow), now, &swapped);
        let changed_definitions = match compared {
            Ok(r) => r.changed_definitions,
            // What else changed is in `changes`.
            Err(_) => statement_changes(&old_meta.result.declarations, now)
                .into_iter()
                .filter(|c| c.change == Change::Value)
                .map(|c| c.name)
                .collect(),
        };
        if !changes.is_empty() && !job.allow_changes {
            return Ok(RebuildSnapshotsResponse {
                ok: false,
                changes,
                changed_definitions,
                ..Default::default()
            });
        }

        let above = self.catalog.dependents(&old).await?;
        let levels = levels(&above, &old);
        let filter: Vec<(String, String)> = job.labels.clone().into_iter().collect();
        let modules: Vec<String> = above.keys().cloned().collect();
        let has_labels = |e: &EntryMeta| job.labels.iter().all(|(k, v)| e.labels.get(k) == Some(v));
        let mut matching: HashMap<String, Vec<(String, EntryMeta)>> = HashMap::new();
        let mut replaced: Vec<RebuildReplaced> = Vec::new();
        let on = self.catalog.entries_on(&modules, &filter).await?;
        let on_ids: Vec<String> = on.iter().map(|(e, _)| e.clone()).collect();
        let mut replacers_of = self.catalog.replacers_among(&on_ids).await?;
        for (e, entry_meta) in on {
            // Only heads are copied, and entries that only their copies from
            // this same rebuild replace, so that it replays them when sent
            // again.
            let key = format!("rebuild:{old}:{new}:{e}");
            let replacers = replacers_of.remove(&e).unwrap_or_default();
            if replacers.iter().all(|(_, r)| r.key.as_ref() == Some(&key)) {
                matching
                    .entry(entry_meta.module.clone())
                    .or_default()
                    .push((e, entry_meta));
                continue;
            }
            let outside: Vec<String> = replacers
                .iter()
                .filter(|(_, r)| !has_labels(r))
                .map(|(r, _)| self.id(r))
                .collect();
            if !outside.is_empty() {
                replaced.push(RebuildReplaced {
                    snapshot_id: self.id(&e),
                    replaced_by: outside,
                });
            }
        }
        let mut order: Vec<String> = needed(&above, matching.keys().cloned())
            .into_iter()
            .collect();
        order.sort_by_key(|m| levels.get(m).copied().unwrap_or(usize::MAX));
        let items: usize = order
            .iter()
            .map(|m| matching.get(m).map_or(1, Vec::len))
            .sum();
        if items > MAX_LIMIT {
            return Err(bad(&format!(
                "the rebuild would create {items} snapshots, more than {MAX_LIMIT}: narrow its labels"
            )));
        }

        // Which theorems use a changed definition, read from what the
        // modules' declarations use, level by level: what uses one below is
        // used here.
        let mut used: HashSet<String> = changed_definitions.iter().cloned().collect();
        if !changed_definitions.is_empty() {
            let (users, _) = uses(&used, now);
            used.extend(users);
        }

        let mut renamed = BTreeMap::from([(old.clone(), new.clone())]);
        // Each new module's original, whose declarations it is taken to have.
        let mut originals: HashMap<String, String> = HashMap::new();
        // What a copy builds on through each module it imports, as its create
        // will find it, the import itself last: through `from`, `to` and what
        // it builds on; through a module the plan rebuilds, that module's copy
        // and what the copy builds on; through any other, the module and what
        // it builds on.
        let mut through: HashMap<String, Vec<String>> =
            HashMap::from([(old.clone(), needs.clone())]);
        let mut planned: Vec<Planned> = Vec::new();
        // The records the plan reads, each once.
        let mut metas: HashMap<String, ModuleMeta> = HashMap::new();
        for m in order {
            let meta = self.module_cached(&m, &mut metas).await?.clone();
            let code = snapshot::rewrite_imports(&meta.code, &renamed);
            let id = snapshot::id(&code);
            let header = snapshot::next_header(&split_header(&code).header, &id);
            renamed.insert(m.clone(), id.clone());
            originals.insert(id.clone(), m.clone());
            // What the copy builds on, from its imports, its own module last.
            // The plan rebuilds what is below first, so an import not seen yet
            // is one it leaves.
            let mut needs_now: Vec<String> = Vec::new();
            for i in &meta.imports {
                if !through.contains_key(i) {
                    let kept = self.module_cached(i, &mut metas).await?;
                    through.insert(i.clone(), ids(&kept.modules).cloned().collect());
                }
                for n in &through[i] {
                    if !needs_now.contains(n) {
                        needs_now.push(n.clone());
                    }
                }
            }
            needs_now.push(id.clone());
            through.insert(m.clone(), needs_now.clone());
            let affected = if changed_definitions.is_empty() {
                Vec::new()
            } else {
                let (users, theorems) = uses(&used, &meta.result.declarations);
                used.extend(users);
                theorems
            };
            let item = |entry: Option<String>, labels, replaces| RebuildItem {
                level: 0,
                key: format!("rebuild:{old}:{new}:{}", entry.as_deref().unwrap_or(&m)),
                replaces,
                snapshot_id: entry.as_deref().map(|e| self.id(e)),
                module: snapshot::module(&m),
                new_module: snapshot::module(&id),
                new_header: header.clone(),
                code: code.clone(),
                labels,
                affected: affected.clone(),
            };
            let Some(entries) = matching.remove(&m) else {
                // Nothing to copy: it supports the copies above it, with no
                // labels and no link.
                planned.push(Planned {
                    created: meta.created * 1_000_000,
                    module: m.clone(),
                    item: item(None, BTreeMap::new(), None),
                });
                continue;
            };
            let needs_before: Vec<String> =
                ids(&meta.modules).filter(|n| **n != m).cloned().collect();
            for (e, entry_meta) in entries {
                // The comparison the copy's create will run, the copy taken to
                // declare what its original does, under an `allow` of every
                // definition: the copy replaces its original if it passes.
                let mut copied = Replacing {
                    replaced: Replaces {
                        snapshot_id: self.id(&e),
                        allow: None,
                    },
                    entry: e.clone(),
                    declarations: meta.result.declarations.clone(),
                    needs: needs_before.clone(),
                };
                let swapped = self
                    .predicted_swap(&copied, &needs_now, &id, &originals, &mut metas)
                    .await?;
                let definitions: Vec<String> = allowable(&copied, &swapped)
                    .map(|d| d.name.clone())
                    .collect();
                copied.replaced.allow = (!definitions.is_empty()).then_some(Allow { definitions });
                let replaces = compare(&copied, &copied.declarations, &swapped)
                    .ok()
                    .map(|r| Replaces {
                        snapshot_id: copied.replaced.snapshot_id.clone(),
                        allow: (!r.changed_definitions.is_empty()).then_some(Allow {
                            definitions: r.changed_definitions,
                        }),
                    });
                planned.push(Planned {
                    created: entry_meta.created_micros,
                    module: m.clone(),
                    item: item(Some(e), entry_meta.labels, replaces),
                });
            }
        }

        let mut on: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, p) in planned.iter().enumerate() {
            on.entry(&p.module).or_default().push(i);
        }
        let imports: Vec<Vec<usize>> = planned
            .iter()
            .map(|p| {
                let modules = above[&p.module].iter();
                modules
                    .filter_map(|m| on.get(m.as_str()))
                    .flatten()
                    .copied()
                    .collect()
            })
            .collect();
        let wave = waves(&imports);
        for (p, wave) in planned.iter_mut().zip(wave) {
            p.item.level = wave;
        }
        planned.sort_by_key(|p| (p.item.level, p.created));
        Ok(RebuildSnapshotsResponse {
            ok: true,
            changes,
            changed_definitions,
            plan: planned.into_iter().map(|p| p.item).collect(),
            replaced,
            ..Default::default()
        })
    }

    /// What the entries with every label in `labels` rest on; see
    /// [`AuditSnapshotsResponse`]. It reads records only.
    pub async fn audit(
        &self,
        labels: &[(String, String)],
    ) -> Result<AuditSnapshotsResponse, Error> {
        if labels.len() > MAX_LABELS {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("{} label filters; at most {MAX_LABELS}", labels.len()),
            ));
        }
        // An entry on each module the labels reach.
        let mut entries: BTreeMap<String, String> = BTreeMap::new();
        let mut after = None;
        loop {
            let (page, next) = self
                .catalog
                .list(labels, MAX_LIMIT, after.as_deref(), false)
                .await?;
            for (e, entry_meta) in page {
                entries.entry(entry_meta.module).or_insert(e);
            }
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        self.count(&entries, &mut HashMap::new()).await
    }

    /// The audit of a run against its spec, the entries `against` names:
    /// the walk from them to the run's heads, and the counts over the
    /// heads; see [`AuditSnapshotsResponse`]. It reads records only.
    pub async fn audit_against(&self, against: &[String]) -> Result<AuditSnapshotsResponse, Error> {
        let mut specs: Vec<(String, EntryMeta)> = Vec::new();
        for id in against {
            let entry = self.parse(id)?;
            if !specs.iter().any(|(e, _)| *e == entry) {
                let entry_meta = self.catalog.entry(&entry).await?;
                specs.push((entry, entry_meta));
            }
        }
        let mut links = Vec::new();
        let mut linked = HashSet::new();
        let mut add_link = |e: &str, entry_meta: &EntryMeta| {
            if let Some(link) = self.audit_link(e, entry_meta)
                && linked.insert(e.to_string())
            {
                links.push(link);
            }
        };
        let mut forks = Vec::new();
        // Each head once, as `(entry, module)`, and those each spec reaches.
        let mut heads: Vec<(String, String)> = Vec::new();
        let mut reached: Vec<Vec<String>> = Vec::new();
        // Whether a spec's chain back reaches a snapshot that is gone.
        let mut cut = false;
        for (spec, spec_meta) in &specs {
            // Back through what it replaced, as far as the entries exist.
            let mut back = Some((spec.clone(), spec_meta.clone()));
            let mut seen = HashSet::new();
            while let Some((e, entry_meta)) = back.take() {
                let Some(replaces) = &entry_meta.replaces else {
                    break;
                };
                if !seen.insert(e.clone()) {
                    break;
                }
                add_link(&e, &entry_meta);
                let older = self.catalog.find_entry(&replaces.snapshot_id).await?;
                cut |= older.is_none();
                back = older.map(|m| (replaces.snapshot_id.clone(), m));
            }
            // Forward through what replaces it, to the heads.
            let mut ends = Vec::new();
            let mut stack = vec![(spec.clone(), spec_meta.clone())];
            let mut seen = HashSet::new();
            while let Some((e, entry_meta)) = stack.pop() {
                if !seen.insert(e.clone()) {
                    continue;
                }
                if e != *spec {
                    add_link(&e, &entry_meta);
                }
                let replacers = self.catalog.replacers(&e).await?;
                if replacers.is_empty() {
                    if !heads.iter().any(|(h, _)| *h == e) {
                        heads.push((e.clone(), entry_meta.module));
                    }
                    ends.push(e);
                    continue;
                }
                if replacers.len() > 1
                    && !forks
                        .iter()
                        .any(|f: &AuditFork| f.snapshot_id == self.id(&e))
                {
                    forks.push(AuditFork {
                        snapshot_id: self.id(&e),
                        replaced_by: replacers.iter().map(|(r, _)| self.id(r)).collect(),
                    });
                }
                // Oldest first.
                stack.extend(replacers.into_iter().rev());
            }
            reached.push(ends);
        }

        let mut metas: HashMap<String, ModuleMeta> = HashMap::new();
        let modules = specs.iter().map(|(_, m)| &m.module);
        for m in modules.chain(heads.iter().map(|(_, m)| m)) {
            self.cached(m, &mut metas).await?;
        }
        // What the spec permits: the axioms its modules, and what they build
        // on, declare.
        let mut permitted: HashSet<String> = HashSet::new();
        for (_, spec_meta) in &specs {
            let Some(meta) = metas.get(&spec_meta.module) else {
                continue;
            };
            let closure: Vec<String> = ids(&meta.modules).cloned().collect();
            for m in &closure {
                let Some(meta) = self.cached(m, &mut metas).await? else {
                    continue;
                };
                let axioms = meta
                    .result
                    .declarations
                    .iter()
                    .filter(|d| d.kind == "axiom");
                permitted.extend(axioms.map(|d| normalised(&d.name).into_owned()));
            }
        }
        let (targets, accepted) = self.targets(&specs, &reached, &heads, &metas, &permitted);
        // A spec copied from a snapshot that is gone, as a retiring rebuild
        // leaves it, can't be shown to state what the original did.
        let clean = forks.is_empty() && accepted && !cut;

        // Each head stands for its module.
        let mut entries: BTreeMap<String, String> = BTreeMap::new();
        for (e, m) in &heads {
            entries.entry(m.clone()).or_insert_with(|| e.clone());
        }
        let mut audit = self.count(&entries, &mut metas).await?;
        for a in &mut audit.assumptions {
            a.in_spec = Some(permitted.contains(&*normalised(&a.name)));
        }
        audit.clean = Some(clean);
        audit.heads = heads.iter().map(|(e, _)| self.id(e)).collect();
        audit.forks = forks;
        audit.targets = targets;
        audit.links = links;
        Ok(audit)
    }

    /// The targets of an audit against `specs`: each declaration of a spec
    /// snapshot's module, other than its assumptions and what Lean
    /// generated, on each head that spec reaches (`reached`), with whether
    /// every one is accepted there under the assumptions `permitted` (see
    /// [`AuditSnapshotsResponse::clean`]). `heads` holds each head's module,
    /// and `metas` the module records.
    fn targets(
        &self,
        specs: &[(String, EntryMeta)],
        reached: &[Vec<String>],
        heads: &[(String, String)],
        metas: &HashMap<String, ModuleMeta>,
        permitted: &HashSet<String>,
    ) -> (Vec<AuditTarget>, bool) {
        let module_of: HashMap<&String, &String> = heads.iter().map(|(e, m)| (e, m)).collect();
        let mut targets = Vec::new();
        let mut all_accepted = true;
        for ((spec, spec_meta), ends) in specs.iter().zip(reached) {
            // A record deleted during the audit can't be judged, so isn't
            // accepted; nor below.
            let Some(meta) = metas.get(&spec_meta.module) else {
                all_accepted = false;
                continue;
            };
            let declared = meta.result.declarations.iter();
            for d in declared.filter(|d| !d.generated && d.kind != "axiom") {
                let name = normalised(&d.name);
                for head in ends {
                    let Some(head_meta) = metas.get(module_of[head]) else {
                        all_accepted = false;
                        continue;
                    };
                    let mut on_head = head_meta.result.declarations.iter();
                    // A replacement keeps every declaration, so this is a
                    // record that was edited: the run isn't done.
                    let Some(h) = on_head.find(|h| normalised(&h.name) == name) else {
                        all_accepted = false;
                        continue;
                    };
                    // Both records come from the kernel re-check, not from a
                    // build's report, and the head keeps the statement:
                    // its link alone doesn't say so.
                    let accepted = meta.verified
                        && head_meta.verified
                        && h.kind == d.kind
                        && h.type_hash == d.type_hash
                        && match h.status {
                            ProofStatus::Proved => true,
                            ProofStatus::Conditional => h
                                .assumes
                                .iter()
                                .all(|a| permitted.contains(&*normalised(a))),
                            ProofStatus::Sorry | ProofStatus::Rejected => false,
                        };
                    all_accepted &= accepted;
                    targets.push(AuditTarget {
                        name: d.name.clone(),
                        spec: self.id(spec),
                        head: self.id(head),
                        status: h.status,
                        verified: head_meta.verified,
                    });
                }
            }
        }
        (targets, all_accepted)
    }

    /// Entry `e`'s link, as an audit lists it, if it allowed changes.
    fn audit_link(&self, e: &str, entry_meta: &EntryMeta) -> Option<AuditLink> {
        let replaces = entry_meta.replaces.as_ref().filter(|r| r.allow.is_some())?;
        Some(AuditLink {
            snapshot_id: self.id(e),
            replaces: Replaces {
                snapshot_id: self.id(&replaces.snapshot_id),
                allow: replaces.allow.clone(),
            },
            replacement: entry_meta.replacement.clone().unwrap_or_default(),
        })
    }

    /// An audit's counts over the modules of `entries`, each with the entry
    /// that stands for it: their declarations by status, the assumptions
    /// they use, and what isn't proved. `metas` holds the module records
    /// read so far, and gains those read here.
    async fn count(
        &self,
        entries: &BTreeMap<String, String>,
        metas: &mut HashMap<String, ModuleMeta>,
    ) -> Result<AuditSnapshotsResponse, Error> {
        let mut audit = AuditSnapshotsResponse::default();
        let mut used_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        // Each assumption is declared among the entries' modules or what
        // they build on.
        let mut closure: BTreeSet<String> = BTreeSet::new();
        for (m, entry) in entries {
            // Its entries were deleted since they were found.
            let Some(meta) = self.cached(m, metas).await? else {
                continue;
            };
            audit.summary.unverified_modules += u64::from(!meta.verified);
            for d in meta.result.declarations.iter().filter(|d| !d.generated) {
                // An assumption itself.
                if d.kind == "axiom" && d.assumes.contains(&d.name) {
                    continue;
                }
                let status = d.status;
                let summary = &mut audit.summary;
                *match status {
                    ProofStatus::Proved => &mut summary.proved,
                    ProofStatus::Conditional => &mut summary.conditional,
                    ProofStatus::Sorry => &mut summary.sorry,
                    ProofStatus::Rejected => &mut summary.rejected,
                } += 1;
                for a in &d.assumes {
                    used_by.entry(a.clone()).or_default().push(d.name.clone());
                }
                if status >= ProofStatus::Sorry {
                    let allowed = &self.policy.allowed_axioms;
                    audit.not_proved.push(AuditUnproved {
                        name: d.name.clone(),
                        snapshot_id: self.id(entry),
                        status,
                        axioms: d
                            .axioms
                            .iter()
                            .filter(|a| !allowed.contains(a))
                            .cloned()
                            .collect(),
                    });
                }
            }
            closure.extend(ids(&meta.modules).cloned());
        }
        for (name, mut users) in used_by {
            users.sort();
            users.dedup();
            let mut assumption = AuditAssumption {
                name,
                signature: None,
                snapshot_id: None,
                verified: false,
                used_by: users,
                in_spec: None,
            };
            if let Some(m) = self.declaring(&assumption.name, &closure, metas).await? {
                assumption.verified = metas[&m].verified;
                let exported = self
                    .catalog
                    .decls(&m)
                    .await?
                    .map(|d| d.decls)
                    .unwrap_or_default();
                assumption.signature = exported
                    .into_iter()
                    .find(|d| d.name == assumption.name)
                    .map(|d| d.signature);
                assumption.snapshot_id = match entries.get(&m) {
                    Some(e) => Some(self.id(e)),
                    None => {
                        let on = self
                            .catalog
                            .entries_on(std::slice::from_ref(&m), &[])
                            .await?;
                        on.into_iter().next().map(|(e, _)| self.id(&e))
                    }
                };
            }
            audit.assumptions.push(assumption);
        }
        Ok(audit)
    }

    /// Module `m`'s record, from `metas` or read into it; `None` if it is
    /// gone.
    async fn cached<'a>(
        &self,
        m: &str,
        metas: &'a mut HashMap<String, ModuleMeta>,
    ) -> Result<Option<&'a ModuleMeta>, Error> {
        if !metas.contains_key(m) {
            match self.catalog.module(m).await {
                Ok(meta) => metas.insert(m.to_string(), meta),
                Err(e) if e.kind == ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
        }
        Ok(metas.get(m))
    }

    /// [`cached`](Self::cached) for a module that must be kept: `not_found`
    /// if it is gone, as [`Catalog::module`] has it.
    async fn module_cached<'a>(
        &self,
        m: &str,
        metas: &'a mut HashMap<String, ModuleMeta>,
    ) -> Result<&'a ModuleMeta, Error> {
        self.cached(m, metas)
            .await?
            .ok_or_else(|| unknown_module(m))
    }

    /// The module among `modules` that declares the axiom `name`, loading
    /// the records `metas` doesn't hold yet.
    async fn declaring(
        &self,
        name: &str,
        modules: &BTreeSet<String>,
        metas: &mut HashMap<String, ModuleMeta>,
    ) -> Result<Option<String>, Error> {
        for m in modules {
            let Some(meta) = self.cached(m, metas).await? else {
                continue;
            };
            let declares = |d: &Declaration| d.kind == "axiom" && d.name == name;
            if meta.result.declarations.iter().any(declares) {
                return Ok(Some(m.clone()));
            }
        }
        Ok(None)
    }

    /// Edits an entry's labels with a merge patch; see
    /// [`Catalog::set_labels`].
    pub async fn set_labels(
        &self,
        entry: &str,
        labels: &BTreeMap<String, Option<String>>,
    ) -> Result<Snapshot, Error> {
        let entry = self.parse(entry)?;
        let entry_meta = self.catalog.set_labels(&entry, labels).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        Ok(self.snapshot(entry, entry_meta, &meta))
    }

    /// Deletes an entry, or with `with_replacers` it and every entry that
    /// replaces it, directly or through others, then each module that
    /// nothing needs any more: the entries' own, then up the chain of
    /// imports. Once the entries are gone, the rest is best effort: what
    /// fails is logged, and a module whose record went is listed even if its
    /// files couldn't be deleted.
    pub async fn delete(
        &self,
        entry: &str,
        with_replacers: bool,
    ) -> Result<DeleteSnapshotResponse, Error> {
        let entry = self.parse(entry)?;
        let (deleted, cleaned) = if with_replacers {
            self.catalog.remove_branch(&entry).await?
        } else {
            let (entry_meta, cleaned) = self.catalog.remove_entry(&entry).await?;
            let cleaned = cleaned.map(|m| (entry_meta.module, m));
            (vec![entry], cleaned.into_iter().collect())
        };
        let mut queue = VecDeque::from(cleaned);
        let mut removed = Vec::new();
        while let Some((id, meta)) = queue.pop_front() {
            // The record is gone, so the files of its build can go.
            if let Some(own) = meta.modules.last() {
                self.delete_blobs(own).await;
            }
            self.forget(&id).await;
            removed.push(snapshot::module(&id));
            for parent in meta.imports {
                match self.catalog.cleanup(&parent).await {
                    Ok(Some(parent_meta)) => queue.push_back((parent, parent_meta)),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(module = %parent, "cleaning up a snapshot failed: {e}")
                    }
                }
            }
        }
        let deleted = deleted.iter().map(|entry| self.id(entry)).collect();
        Ok(DeleteSnapshotResponse { removed, deleted })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    fn decl(name: &str, ty: &str, value: Option<&str>, axioms: &[&str]) -> Declaration {
        let reported = ReportedDeclaration {
            name: name.into(),
            kind: if value.is_some() { "def" } else { "theorem" }.into(),
            axioms: axioms.iter().map(|a| a.to_string()).collect(),
            type_hash: ty.into(),
            value_hash: value.map(Into::into),
            ..Default::default()
        };
        reported.with_status(ProofStatus::Proved)
    }

    /// A `kind` declaration whose type uses `type_refs` and value
    /// `value_refs`.
    fn declared(name: &str, kind: &str, type_refs: &[&str], value_refs: &[&str]) -> Declaration {
        let d = Declaration {
            kind: kind.into(),
            ..decl(name, "", None, &[])
        };
        using(d, type_refs, value_refs)
    }

    /// An `allow` that names `names`.
    fn allow(names: &[&str]) -> Option<Allow> {
        Some(Allow {
            definitions: names.iter().map(|n| n.to_string()).collect(),
        })
    }

    fn replacing(old: Vec<Declaration>, allow: Option<Allow>) -> Replacing {
        Replacing {
            replaced: Replaces {
                snapshot_id: "v~e".into(),
                allow,
            },
            entry: "e".into(),
            declarations: old,
            needs: vec!["a".into()],
        }
    }

    #[test]
    fn a_replacement_keeps_statements_and_says_what_it_filled() {
        let old = vec![
            decl("t", "1", None, &["sorryAx"]),
            decl("u", "2", None, &["sorryAx"]),
            decl("f", "3", Some("4"), &[]),
        ];
        let now = [
            decl("t", "1", None, &["propext"]),
            decl("u", "2", None, &["sorryAx"]),
            decl("f", "3", Some("4"), &[]),
            decl("helper", "9", None, &[]),
        ];
        // It may build on more snapshots than the replaced module does.
        let needs = Swapped::default();
        let r = compare(&replacing(old.clone(), None), &now, &needs).unwrap();
        assert_eq!(r.filled, ["t"]);
        assert!(r.changed_definitions.is_empty() && r.affected.is_empty());

        let retyped = [decl("t", "X", None, &[]), now[1].clone(), now[2].clone()];
        assert_eq!(
            compare(&replacing(old.clone(), None), &retyped, &needs),
            Err(vec!["statement: t has a different type".to_string()])
        );
        // A definition's value changes only when that is allowed, and never
        // its type.
        let redefined = [
            now[0].clone(),
            now[1].clone(),
            decl("f", "3", Some("5"), &[]),
        ];
        assert_eq!(
            compare(&replacing(old.clone(), None), &redefined, &needs),
            Err(vec!["statement: f has a different value".to_string()])
        );
        let allowed = replacing(old.clone(), allow(&["f"]));
        let r = compare(&allowed, &redefined, &needs).unwrap();
        assert_eq!(r.changed_definitions, ["f"]);
        assert_eq!(compare(&allowed, &retyped, &needs).unwrap_err().len(), 1);
    }

    fn using(d: Declaration, type_refs: &[&str], value_refs: &[&str]) -> Declaration {
        let refs = |r: &[&str]| r.iter().map(|r| r.to_string()).collect();
        Declaration {
            type_refs: refs(type_refs),
            value_refs: refs(value_refs),
            ..d
        }
    }

    /// A module on snapshot `a`, whose `thm3` states something about `a`'s
    /// `foo`, which uses `bar`; and a replacement on `a2` instead, which
    /// declares `foo` and `bar` again as `foo2` and `bar2` say.
    fn swap(foo2: Option<Declaration>, bar2: Declaration) -> (Vec<Declaration>, Swapped) {
        let old = vec![using(decl("thm3", "9", None, &[]), &["foo"], &[])];
        let foo = using(decl("foo", "1", Some("2"), &[]), &[], &["bar"]);
        let bar = using(decl("bar", "3", Some("4"), &[]), &[], &[]);
        let lemma = using(decl("lemma", "7", None, &[]), &[], &[]);
        let gained = foo2.into_iter().chain([bar2, lemma]).collect();
        let swapped = Swapped {
            lost: vec![("a".into(), vec![foo, bar])],
            gained: vec![("a2".into(), gained)],
        };
        (old, swapped)
    }

    #[test]
    fn only_the_named_definitions_may_change_their_value() {
        let old = vec![
            decl("f", "1", Some("2"), &[]),
            decl("g", "1", Some("2"), &[]),
        ];
        let now = [
            decl("f", "1", Some("3"), &[]),
            decl("g", "1", Some("3"), &[]),
        ];
        let none = Swapped::default();
        assert_eq!(
            compare(&replacing(old.clone(), allow(&["f"])), &now, &none),
            Err(vec!["statement: g has a different value".to_string()])
        );
        let r = compare(&replacing(old, allow(&["f", "g"])), &now, &none).unwrap();
        assert_eq!(r.changed_definitions, ["f", "g"]);
    }

    #[test]
    fn allow_names_definitions_of_the_replaced_module_or_a_lost_snapshot() {
        let old = vec![decl("f", "1", Some("2"), &[]), decl("t", "3", None, &[])];
        let swapped = Swapped {
            lost: vec![(
                "a".into(),
                vec![
                    decl("foo", "1", Some("2"), &[]),
                    decl("_private.PisaSnap.sa.0.h", "1", Some("2"), &[]),
                ],
            )],
            gained: vec![("b".into(), vec![decl("bar", "1", Some("2"), &[])])],
        };
        assert!(allow_error(&replacing(old.clone(), None), &swapped).is_none());
        for names in [&["f"][..], &["foo"], &["f", "foo"], &["_private.h"]] {
            let allowed = replacing(old.clone(), allow(names));
            assert!(allow_error(&allowed, &swapped).is_none(), "{names:?}");
        }
        // A theorem, a definition of a snapshot the code builds on now, and
        // a private name as written, which the error says how to name.
        for (name, hint) in [
            ("t", ""),
            ("bar", ""),
            ("nope", ""),
            ("h", " (for a private definition, write _private.h)"),
        ] {
            let allowed = replacing(old.clone(), allow(&["f", name]));
            let error = allow_error(&allowed, &swapped).unwrap();
            assert_eq!(error.kind, ErrorKind::BadRequest);
            let message = format!(
                "allow names {name}, which isn't a definition of the replaced snapshot or of a \
                 snapshot the code no longer builds on{hint}"
            );
            assert_eq!(error.message, message);
        }
    }

    #[test]
    fn a_private_definition_is_allowed_by_its_name_without_its_module() {
        let h = |m: &str, value: &str| {
            decl(
                &format!("_private.PisaSnap.s{m}.0.h"),
                "1",
                Some(value),
                &[],
            )
        };
        let none = Swapped::default();
        // The replaced module's own: as the comparison names it, or as a
        // refusal does.
        let old = vec![h("a", "2")];
        let now = [h("b", "3")];
        for name in ["_private.h", "_private.PisaSnap.sa.0.h"] {
            let allowed = replacing(old.clone(), allow(&[name]));
            assert!(allow_error(&allowed, &none).is_none(), "{name}");
            let r = compare(&allowed, &now, &none).unwrap();
            assert_eq!(r.changed_definitions, ["_private.PisaSnap.sa.0.h"]);
        }
        let bare = replacing(old.clone(), allow(&["h"]));
        assert!(allow_error(&bare, &none).is_some());
        assert_eq!(
            compare(&bare, &now, &none),
            Err(vec![
                "statement: _private.PisaSnap.sa.0.h has a different value".to_string()
            ])
        );

        // One the comparison reaches in a snapshot the code no longer builds
        // on.
        let thm = vec![using(
            decl("thm3", "9", None, &[]),
            &["_private.PisaSnap.sa.0.h"],
            &[],
        )];
        let swapped = Swapped {
            lost: vec![("a".into(), vec![h("a", "2")])],
            gained: vec![("b".into(), vec![h("b", "3")])],
        };
        let allowed = replacing(thm.clone(), allow(&["_private.h"]));
        assert!(allow_error(&allowed, &swapped).is_none());
        let r = compare(&allowed, &thm, &swapped).unwrap();
        assert_eq!(r.changed_definitions, ["_private.PisaSnap.sa.0.h"]);
        assert_eq!(r.affected, ["thm3"]);
    }

    #[test]
    fn a_swapped_snapshot_passes_when_nothing_the_statements_use_changed() {
        let foo = || using(decl("foo", "1", Some("2"), &[]), &[], &["bar"]);
        let bar = |value: &str| using(decl("bar", "3", Some(value), &[]), &[], &[]);
        let (old, swapped) = swap(Some(foo()), bar("4"));
        assert!(compare(&replacing(old.clone(), None), &old, &swapped).is_ok());

        // `bar` changed under `foo`, which `thm3` names.
        let (old, swapped) = swap(Some(foo()), bar("5"));
        let reason = format!(
            "statement: bar, from {}, has a different value",
            snapshot::module("a")
        );
        assert_eq!(
            compare(&replacing(old.clone(), None), &old, &swapped),
            Err(vec![reason])
        );
        let allowed = replacing(old.clone(), allow(&["bar"]));
        let r = compare(&allowed, &old, &swapped).unwrap();
        assert_eq!(
            (r.changed_definitions, r.affected),
            (vec!["bar".into()], vec!["thm3".into()])
        );

        let (old, swapped) = swap(None, bar("4"));
        let reason = format!("statement: foo, from {}, is missing", snapshot::module("a"));
        assert_eq!(
            compare(&replacing(old.clone(), None), &old, &swapped),
            Err(vec![reason])
        );
    }

    #[test]
    fn a_replacement_cant_assume_what_it_should_prove() {
        let axiom = |name: &str, ty: &str| Declaration {
            kind: "axiom".into(),
            ..using(decl(name, ty, None, &[]), &[], &[])
        };
        let old = vec![using(decl("lem2", "1", None, &["sorryAx"]), &[], &[])];
        let skeleton = || replacing(old.clone(), None);
        let none = Swapped::default();
        // The theorem itself, assumed.
        let err = compare(&skeleton(), &[axiom("lem2", "1")], &none).unwrap_err();
        assert_eq!(
            err,
            [
                "statement: lem2 is a different kind of declaration",
                "statement: the code adds the assumption lem2"
            ]
        );
        // An assumption of its own, or of a snapshot it now builds on.
        let proof = using(decl("lem2", "1", None, &["helper"]), &[], &[]);
        let err = compare(&skeleton(), &[axiom("helper", "1"), proof.clone()], &none);
        assert_eq!(
            err.unwrap_err(),
            ["statement: the code adds the assumption helper"]
        );
        let gained = Swapped {
            lost: Vec::new(),
            gained: vec![("x".into(), vec![axiom("helper", "1")])],
        };
        let err = compare(&skeleton(), std::slice::from_ref(&proof), &gained).unwrap_err();
        assert_eq!(err, ["statement: the code adds the assumption helper"]);

        // Assumptions it already had are kept, with their statements.
        let assumed = vec![axiom("kunneth", "5"), old[0].clone()];
        let with = replacing(assumed.clone(), None);
        let kept = [
            axiom("kunneth", "5"),
            using(decl("lem2", "1", None, &["kunneth"]), &[], &[]),
        ];
        assert!(compare(&with, &kept, &none).is_ok());
        let moved = Swapped {
            lost: vec![("a".into(), vec![axiom("other", "6")])],
            gained: vec![("b".into(), vec![axiom("other", "7")])],
        };
        let err = compare(&with, &kept, &moved).unwrap_err();
        assert!(
            err.contains(&"statement: the assumption other has a different statement".to_string()),
            "{err:?}"
        );
        // What Lean declares, such as `native_decide`'s axiom, isn't one.
        let native = [
            axiom("lem2._native.native_decide.ax_1_1", "8"),
            old[0].clone(),
        ];
        assert!(compare(&skeleton(), &native, &none).is_ok());
    }

    #[test]
    fn a_swap_that_cant_be_followed_is_refused() {
        // A private name the new side has twice.
        let private = |m: &str| {
            using(
                decl(&format!("_private.PisaSnap.s{m}.0.h"), "5", None, &[]),
                &[],
                &[],
            )
        };
        let old = vec![using(
            decl("thm3", "9", None, &[]),
            &["_private.PisaSnap.sa.0.h"],
            &[],
        )];
        let swapped = Swapped {
            lost: vec![("a".into(), vec![private("a")])],
            gained: vec![
                ("b".into(), vec![private("b")]),
                ("c".into(), vec![private("c")]),
            ],
        };
        let err = compare(&replacing(old.clone(), None), &old, &swapped);
        assert!(err.unwrap_err()[0].contains("more than one"));
        // Once, it is found under its new module.
        let swapped = Swapped {
            gained: vec![("b".into(), vec![private("b")])],
            ..swapped
        };
        assert!(compare(&replacing(old.clone(), None), &old, &swapped).is_ok());
    }

    #[test]
    fn a_level_is_one_more_than_the_highest_import() {
        // b under c and d, e on c and d, and f on b and e.
        let above = BTreeMap::from(
            [
                ("c", vec!["b"]),
                ("d", vec!["b"]),
                ("e", vec!["c", "d"]),
                ("f", vec!["b", "e"]),
            ]
            .map(|(m, i)| (m.to_string(), i.into_iter().map(String::from).collect())),
        );
        let levels = levels(&above, "b");
        let level = |m: &str| levels[m];
        assert_eq!(
            [level("b"), level("c"), level("d"), level("e"), level("f")],
            [0, 1, 1, 2, 3]
        );
    }

    #[test]
    fn an_item_comes_after_what_it_imports() {
        // 0 and 1 on `from`'s module; 2 on 0's module, and 3 on 1's and 2's.
        let imports = [vec![], vec![], vec![0], vec![1, 2]];
        assert_eq!(waves(&imports), [1, 1, 2, 3]);
    }

    #[test]
    fn a_rebuild_needs_what_has_the_labels_and_what_links_it_to_the_root() {
        // b under c and d, e on c, f on d; x on b links y to it.
        let above = BTreeMap::from(
            [
                ("c", vec!["b"]),
                ("d", vec!["b"]),
                ("e", vec!["c"]),
                ("f", vec!["d"]),
                ("x", vec!["b"]),
                ("y", vec!["x"]),
            ]
            .map(|(m, i)| (m.to_string(), i.into_iter().map(String::from).collect())),
        );
        let needed = |matching: &[&str]| -> Vec<String> {
            needed(&above, matching.iter().map(|m| m.to_string()))
                .into_iter()
                .collect()
        };
        assert_eq!(needed(&["e"]), ["c", "e"]);
        assert_eq!(needed(&["e", "y"]), ["c", "e", "x", "y"]);
        assert!(
            needed(&["b", "nothing"]).is_empty(),
            "the root isn't rebuilt"
        );
    }

    #[test]
    fn uses_say_what_comes_to_use_a_name() {
        let declared = [
            declared("g", "def", &["Nat"], &["f"]),
            declared("h", "def", &["Nat"], &["g"]),
            declared("t", "theorem", &["h"], &["f"]),
            declared("u", "theorem", &["Nat"], &["f"]),
        ];
        let used = HashSet::from(["f".to_string()]);
        let (users, theorems) = uses(&used, &declared);
        assert_eq!(
            (users, theorems),
            (vec!["g".into(), "h".into()], vec!["t".into()])
        );
    }

    #[test]
    fn affected_theorems_use_a_changed_definition_directly_or_through_others() {
        let declared = [
            declared("f", "def", &["Nat"], &["Nat.add"]),
            declared("g", "def", &["Nat"], &["f"]),
            declared("h", "def", &["Nat"], &["Nat.mul"]),
            declared("direct", "theorem", &["f", "Eq"], &[]),
            declared("through", "theorem", &["g"], &[]),
            declared("unrelated", "theorem", &["h"], &["f"]),
        ];
        let affected = |changed: &str| uses(&HashSet::from([changed.to_string()]), &declared).1;
        assert_eq!(affected("f"), ["direct", "through"]);
        assert!(affected("nothing").is_empty());
    }

    /// A worker's snapshots over `db`, with its local cache in `dir`.
    fn snapshots(db: Db, dir: &Path) -> Arc<Snapshots> {
        std::fs::create_dir_all(dir.join(snapshot::ROOT)).unwrap();
        let store = Arc::new(InMemory::new());
        let policy = Policy {
            allowed_axioms: crate::policy::DEFAULT_AXIOMS
                .split(',')
                .map(String::from)
                .collect(),
            allowed_options: Vec::new(),
            allow_metaprogramming: false,
        };
        let s = Snapshots::new(
            store,
            db,
            "v".into(),
            dir.into(),
            Duration::from_secs(60),
            policy,
        );
        Arc::new(s)
    }

    /// A new build of module `id`: one `.olean` that holds `content`.
    fn built(id: &str, content: &[u8]) -> ModuleMeta {
        let own = Module {
            id: id.into(),
            build: uuid::Uuid::new_v4().simple().to_string(),
            files: vec![ModuleFile {
                ext: "olean".into(),
                sha256: sha256(content),
            }],
        };
        ModuleMeta {
            imports: Vec::new(),
            modules: vec![own],
            header: String::new(),
            code: String::new(),
            status: ProofStatus::Proved,
            verified: false,
            macros: false,
            result: CheckResult::default(),
            created: 0,
        }
    }

    fn no_labels() -> NewEntry {
        NewEntry {
            labels: Default::default(),
            key: None,
            replaces: None,
            replacement: None,
            fork: false,
        }
    }

    fn keyed(key: &str) -> NewEntry {
        NewEntry {
            key: Some(key.into()),
            ..no_labels()
        }
    }

    /// Every row of version `v`, except the change log's, which outlive what
    /// they describe.
    async fn rows(db: &Db) -> Vec<Vec<String>> {
        db.rows("v").await.unwrap()
    }

    async fn upload(s: &Snapshots, meta: &ModuleMeta, content: &[u8]) {
        let compressed = zstd::encode_all(content, 3).unwrap();
        let path = s.blob(&meta.modules[0], "olean");
        s.store.put(&path, compressed.into()).await.unwrap();
    }

    async fn has_files(s: &Snapshots, meta: &ModuleMeta) -> bool {
        s.store
            .get(&s.blob(&meta.modules[0], "olean"))
            .await
            .is_ok()
    }

    /// The race a delete has to survive: while it removes a module, a create
    /// of the same code on this worker builds the module again, uploads it and
    /// stores it. The delete removes only the files of the build it removed,
    /// and leaves the new build's local files alone.
    #[tokio::test]
    async fn a_delete_leaves_a_new_build_of_its_module_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let catalog = Catalog::new(db, "v".into());
        let id = snapshot::id("code");
        let old = built(&id, b"old");
        upload(&s, &old, b"old").await;
        let (entry, _, _) = catalog.commit(&id, &old, None, no_labels()).await.unwrap();

        // The new build is uploaded and installed here, but not stored yet.
        let new = built(&id, b"new");
        upload(&s, &new, b"new").await;
        std::fs::write(s.file(&id, "olean"), b"new").unwrap();
        s.cached
            .lock()
            .unwrap()
            .insert(id.clone(), new.modules[0].clone());
        // A lookup vouched for the old build before.
        s.confirmed
            .lock()
            .unwrap()
            .insert(id.clone(), Instant::now());

        // The build holds the module's lock until it has stored its record,
        // so the delete removes the old record and files, then waits there.
        let lock = s.lock(&id);
        let building = lock.lock().await;
        let deleting = tokio::spawn({
            let s = s.clone();
            async move { s.delete(&s.id(&entry), false).await }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while has_files(&s, &old).await {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        catalog.commit(&id, &new, None, no_labels()).await.unwrap();
        drop(building);

        let deleted = deleting.await.unwrap().unwrap();
        assert_eq!(deleted.removed, [snapshot::module(&id)]);
        assert!(has_files(&s, &new).await, "the new build's files stay");
        assert_eq!(std::fs::read(s.file(&id, "olean")).unwrap(), b"new");
        assert_eq!(s.cached.lock().unwrap().get(&id), Some(&new.modules[0]));
        assert_eq!(s.catalog.module(&id).await.unwrap().modules, new.modules);
        // The local copy might have been the old build: the next import
        // looks the module up again.
        assert!(!s.confirmed.lock().unwrap().contains_key(&id));
    }

    /// A re-check that passes on a module's old build. Meanwhile the module
    /// was deleted and built again: the new build stays unverified, and so
    /// does the old one's record, which no longer exists.
    #[tokio::test]
    async fn a_verify_of_an_old_build_leaves_the_new_one_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let catalog = Catalog::new(db, "v".into());
        let id = snapshot::id("code");
        let old = built(&id, b"old");
        let (entry, _, _) = catalog.commit(&id, &old, None, no_labels()).await.unwrap();
        catalog.remove_entry(&entry).await.unwrap();
        let new = built(&id, b"new");
        catalog.commit(&id, &new, None, no_labels()).await.unwrap();

        let passed = Verification {
            ok: true,
            reasons: Vec::new(),
            declarations: Vec::new(),
        };
        let old = &old.modules[0];
        assert_eq!(s.before_recheck(old).await.unwrap(), Some(Recheck::Changed));
        let recorded = s.record_recheck(old, &passed).await.unwrap();
        assert_eq!(recorded, Recheck::Changed);
        assert!(!catalog.module(&id).await.unwrap().verified);

        let new = &new.modules[0];
        let recorded = s.record_recheck(new, &passed).await.unwrap();
        assert_eq!(recorded, Recheck::Passed);
        assert!(catalog.module(&id).await.unwrap().verified);
    }

    /// The local copy is an old build that a lookup still vouches for, as on
    /// a worker that didn't see the module deleted and built again. The
    /// re-check of the stored build fetches that build first, since the
    /// verifier reads the local files.
    #[tokio::test]
    async fn a_verify_fetches_the_build_it_checks() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let id = snapshot::id("code");
        let old = built(&id, b"old");
        std::fs::write(s.file(&id, "olean"), b"old").unwrap();
        s.cached
            .lock()
            .unwrap()
            .insert(id.clone(), old.modules[0].clone());
        s.confirmed
            .lock()
            .unwrap()
            .insert(id.clone(), Instant::now());
        let new = built(&id, b"new");
        upload(&s, &new, b"new").await;
        let catalog = Catalog::new(db, "v".into());
        catalog.commit(&id, &new, None, no_labels()).await.unwrap();

        assert_eq!(s.before_recheck(&new.modules[0]).await.unwrap(), None);
        assert_eq!(std::fs::read(s.file(&id, "olean")).unwrap(), b"new");
        assert_eq!(s.cached.lock().unwrap().get(&id), Some(&new.modules[0]));
    }

    /// A record the store couldn't read back is refused before anything is
    /// written, and the files uploaded for it go.
    #[tokio::test]
    async fn a_record_too_large_is_refused_and_its_files_go() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let id = snapshot::id("code");
        let mut ours = built(&id, b"ours");
        ours.code = "x".repeat(7 * 1024 * 1024);
        let own = ours.modules[0].clone();
        let contents = [b"ours".to_vec()];
        let error = s
            .commit(&own, &contents, &ours, None, keyed("k"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest);
        assert!(
            error.message.contains("snapshot record too large"),
            "{error:?}"
        );
        assert!(rows(&db).await.is_empty());
        assert!(!has_files(&s, &ours).await, "this build's files go");
    }

    #[tokio::test]
    async fn a_listing_takes_at_most_64_filters() {
        let dir = tempfile::tempdir().unwrap();
        let s = snapshots(Db::memory().await, dir.path());
        let filters = |n: usize| -> Vec<(String, String)> {
            (0..n).map(|i| (i.to_string(), String::new())).collect()
        };
        assert!(s.list(&filters(64), 10, None, false).await.is_ok());
        let error = s.list(&filters(65), 10, None, false).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
    }

    /// A snapshot id of another version, which a router routes elsewhere, or
    /// a bare entry, is refused here too.
    #[tokio::test]
    async fn an_id_of_another_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let s = snapshots(Db::memory().await, dir.path());
        let entry = "0".repeat(32);
        let error = s.get(&format!("w~{entry}")).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
        assert!(error.message.contains("belongs to version w"), "{error:?}");
        let error = s.get(&entry).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest, "{error:?}");
        let error = s.get(&s.id(&entry)).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound, "{error:?}");
    }

    /// Two workers build the same code at once and the other one stores it
    /// first: this one adds its entry to their record, deletes the files it
    /// uploaded and installs their build instead.
    #[tokio::test]
    async fn a_commit_that_finds_the_module_stored_keeps_that_build() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let id = snapshot::id("code");
        let theirs = built(&id, b"theirs");
        upload(&s, &theirs, b"theirs").await;
        let catalog = Catalog::new(db, "v".into());
        catalog
            .commit(&id, &theirs, None, no_labels())
            .await
            .unwrap();

        // As `install` leaves this worker's build.
        let ours = built(&id, b"ours");
        std::fs::write(s.file(&id, "olean"), b"ours").unwrap();
        let own = ours.modules[0].clone();
        s.cached.lock().unwrap().insert(id.clone(), own.clone());

        let contents = [b"ours".to_vec()];
        let (stored, (entry, _)) = s
            .commit(&own, &contents, &ours, None, no_labels())
            .await
            .unwrap();
        assert_eq!(stored.modules, theirs.modules);
        assert_eq!(catalog.entry(&entry).await.unwrap().module, id);
        assert!(!has_files(&s, &ours).await, "this build's files go");
        assert!(has_files(&s, &theirs).await);
        assert_eq!(std::fs::read(s.file(&id, "olean")).unwrap(), b"theirs");
        assert_eq!(s.cached.lock().unwrap().get(&id), Some(&theirs.modules[0]));
    }

    /// Another create took the key for other code while this one was
    /// building: its transaction writes nothing, not even the module and its
    /// child link, and the blobs uploaded for it go. An entry added to a kept
    /// module under that key gets the other code's entry back too.
    #[tokio::test]
    async fn a_commit_whose_key_is_used_by_other_code_keeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let catalog = Catalog::new(db.clone(), "v".into());
        let (parent_id, other_id, id) = (
            snapshot::id("parent"),
            snapshot::id("other"),
            snapshot::id("code"),
        );
        let parent = built(&parent_id, b"parent");
        let other = built(&other_id, b"other");
        catalog
            .commit(&parent_id, &parent, None, no_labels())
            .await
            .unwrap();
        let (taken, _, _) = catalog
            .commit(&other_id, &other, None, keyed("k"))
            .await
            .unwrap();
        upload(&s, &other, b"other").await;
        let before = rows(&db).await;

        let ours = built(&id, b"ours");
        let own = ours.modules[0].clone();
        upload(&s, &ours, b"ours").await;
        let mut on_parent = ours.clone();
        on_parent.imports = vec![parent_id.clone()];
        on_parent.modules.insert(0, parent.modules[0].clone());
        let (entry, entry_meta, _) = catalog
            .commit(&id, &on_parent, None, keyed("k"))
            .await
            .unwrap();
        assert_eq!((entry, entry_meta.module), (taken.clone(), other_id));
        assert_eq!(rows(&db).await, before);

        let contents = [b"ours".to_vec()];
        let error = s
            .commit(&own, &contents, &on_parent, None, keyed("k"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest);
        assert!(error.message.contains("different code"), "{error:?}");
        assert_eq!(rows(&db).await, before);
        assert!(catalog.module(&id).await.is_err());
        assert!(!has_files(&s, &ours).await, "this build's files go");
        assert!(has_files(&s, &other).await);

        let added = catalog.add_entry(&parent_id, keyed("k")).await.unwrap();
        assert_eq!(added.map(|(e, _)| e), Some(taken));
        assert_eq!(rows(&db).await, before);
    }

    /// The key is already on an entry of this very module: that entry is
    /// returned unchanged with the stored record, and this build's files go.
    #[tokio::test]
    async fn a_commit_whose_key_is_on_the_same_module_returns_that_entry() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::memory().await;
        let s = snapshots(db.clone(), dir.path());
        let catalog = Catalog::new(db.clone(), "v".into());
        let id = snapshot::id("code");
        let theirs = built(&id, b"theirs");
        upload(&s, &theirs, b"theirs").await;
        let mut labelled = keyed("k");
        labelled.labels.insert("a".into(), "1".into());
        let (taken, _, _) = catalog.commit(&id, &theirs, None, labelled).await.unwrap();
        let before = rows(&db).await;

        let ours = built(&id, b"ours");
        let own = ours.modules[0].clone();
        upload(&s, &ours, b"ours").await;
        std::fs::write(s.file(&id, "olean"), b"ours").unwrap();
        let contents = [b"ours".to_vec()];
        let (stored, (entry, entry_meta)) = s
            .commit(&own, &contents, &ours, None, keyed("k"))
            .await
            .unwrap();
        assert_eq!(entry, taken);
        assert_eq!(entry_meta.labels["a"], "1");
        assert_eq!(stored.modules, theirs.modules);
        assert_eq!(rows(&db).await, before);
        assert!(!has_files(&s, &ours).await, "this build's files go");
        assert!(has_files(&s, &theirs).await);
    }

    /// A spec snapshot's module and its head's, as an audit judges them,
    /// with one record or the other gone, as when a snapshot is deleted
    /// during the audit: what can't be judged isn't accepted.
    #[tokio::test]
    async fn an_audit_target_without_its_records_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let s = snapshots(Db::memory().await, dir.path());
        let entry = |module: &str| EntryMeta {
            module: module.into(),
            labels: Default::default(),
            key: None,
            created_micros: 0,
            replaces: None,
            replacement: None,
        };
        // Each module declares `t`, proved and verified.
        let module = |m: &str| ModuleMeta {
            verified: true,
            result: CheckResult {
                declarations: vec![decl("t", "1", None, &[])],
                ..Default::default()
            },
            ..built(m, b"")
        };
        let specs = [("spec".to_string(), entry("s"))];
        let reached = [vec!["head".to_string()]];
        let heads = [("head".to_string(), "h".to_string())];
        let judged = |modules: &[&str]| {
            let metas = modules.iter().map(|m| (m.to_string(), module(m))).collect();
            s.targets(&specs, &reached, &heads, &metas, &HashSet::new())
        };
        let (targets, accepted) = judged(&["s", "h"]);
        assert_eq!((targets.len(), accepted), (1, true));
        for left in ["s", "h"] {
            assert_eq!(judged(&[left]), (Vec::new(), false), "only {left}");
        }
    }
}
