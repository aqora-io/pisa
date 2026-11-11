//! Snapshot metadata in the database: the modules, which import each other,
//! and the entries, which keep them.
//!
//! Tables (see `migrations/`), each keyed by pool version first:
//! - `modules`: module `m` ([`ModuleMeta`]): its build and files, whose
//!   files are in object storage under the build, uploaded before the row is
//!   written; its header, status, whether it is verified and when it was
//!   built, which listings read ([`ModuleSummary`]); its code and check; and
//!   its declarations for the search service ([`SnapshotDecls`]). What it
//!   needs is read through `module_imports`, not stored with it, so a record
//!   doesn't grow with the depth of its imports
//! - `module_imports`: `m` imports `parent`, at `position` in its header
//! - `entries`: entry `e` ([`EntryMeta`]), its labels, and the idempotency key
//!   it was made with
//! - `links`: what entry `e` replaces, with what its create allowed and
//!   found, and the entry its run starts at
//! - `described`: the search service's description and vector of a
//!   declaration of `m`, which only it reads
//! - `leases`: a lease held by one process, e.g. the search replica that
//!   indexes snapshots
//! - `changes`: a [`Change`]; one per transaction that changes an entry or a
//!   module, for the search service to follow, kept for [`LOG_RETENTION`]
//!
//! A module is kept while an entry points at it or a kept module imports it.
//! Transactions that write are serializable (see [`crate::db`]), so one that
//! removes a module conflicts with one that adds an entry on it, and two
//! creates with the same key conflict instead of both succeeding.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, SystemTime};

use pisa_protocol::api::{
    Allow, AmendedHoles, CheckResult, Error, ErrorKind, ProofStatus, Replacement, Replaces,
    RunImport, SnapshotId,
};
use pisa_protocol::snapshot;
use pisa_protocol::verify::{DeclarationChange, normalised};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::dispatch;
use crate::db::{Db, DbError, IsConflict};

/// A module's record. The catalog stores it as columns, `module_imports`, and
/// a `ModuleBody`: what it imports is read back in the header's order, and
/// what it needs, `modules`, is read through the imports of what it imports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleMeta {
    /// Ids of the modules it imports, in the header's order.
    pub imports: Vec<String>,
    /// Every module it needs, its own last, each after what it imports.
    pub modules: Vec<Module>,
    /// See [`Snapshot::header`](pisa_protocol::api::Snapshot::header).
    pub header: String,
    pub code: String,
    /// See [`Snapshot::status`](pisa_protocol::api::Snapshot::status): the
    /// worst status among its check result's declarations.
    pub status: ProofStatus,
    /// See [`Snapshot::verified`](pisa_protocol::api::Snapshot::verified).
    #[serde(default)]
    pub verified: bool,
    /// Its code or anything it imports uses macros, which can expand in code
    /// on top of it.
    #[serde(default)]
    pub macros: bool,
    pub result: CheckResult,
    pub created: u64,
}

/// What the catalog keeps of a module's record as JSON: its code, its check
/// result and whether it uses macros. The rest is columns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModuleBody {
    pub code: String,
    #[serde(default)]
    pub macros: bool,
    pub result: CheckResult,
}

impl ModuleBody {
    pub(crate) fn of(meta: &ModuleMeta) -> ModuleBody {
        ModuleBody {
            code: meta.code.clone(),
            macros: meta.macros,
            result: meta.result.clone(),
        }
    }
}

/// What a listing shows of a module: its record's columns and its imports,
/// without its code or check.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleSummary {
    pub imports: Vec<String>,
    pub header: String,
    pub status: ProofStatus,
    pub verified: bool,
    pub created: u64,
}

impl ModuleMeta {
    pub fn summary(&self) -> ModuleSummary {
        ModuleSummary {
            imports: self.imports.clone(),
            header: self.header.clone(),
            status: self.status,
            verified: self.verified,
            created: self.created,
        }
    }
}

/// A module's record without the builds of what it needs, as the reads of
/// the catalog take many at a time ([`Catalog::records`]): what it builds on
/// comes from [`Catalog::import_graph`], shared among them.
#[derive(Debug, Clone)]
pub struct ModuleRecord {
    /// Ids of the modules it imports, in the header's order.
    pub imports: Vec<String>,
    pub header: String,
    pub code: String,
    pub status: ProofStatus,
    pub verified: bool,
    pub macros: bool,
    pub result: CheckResult,
    pub created: u64,
}

/// A module's status as its column holds it.
pub(crate) fn status_text(status: ProofStatus) -> Result<String, Fail> {
    match serde_json::to_value(status)? {
        Value::String(text) => Ok(text),
        other => Err(Fail::Api(internal(format!("a status as {other}")))),
    }
}

pub(crate) fn status_of(text: &str) -> Result<ProofStatus, Fail> {
    Ok(serde_json::from_value(Value::String(text.to_string()))?)
}

/// The modules `root` needs, each after what it imports and `root` last, as
/// the header orders imports: `imports` has each module's imports in order,
/// as [`Catalog::import_graph`] reads them.
pub fn closure_order(root: &str, imports: &HashMap<String, Vec<String>>) -> Vec<String> {
    fn visit(
        m: &str,
        imports: &HashMap<String, Vec<String>>,
        seen: &mut HashSet<String>,
        out: &mut Vec<String>,
    ) {
        if !seen.insert(m.to_string()) {
            return;
        }
        for parent in imports.get(m).into_iter().flatten() {
            visit(parent, imports, seen, out);
        }
        out.push(m.to_string());
    }
    let mut out = Vec::new();
    visit(root, imports, &mut HashSet::new(), &mut out);
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub id: String,
    /// A random name for the build that made these files, so that the files
    /// of the same module built again never share a path with these.
    pub build: String,
    /// The `.olean` last: it is written last, because Lean imports a
    /// module-system `.olean` without its other files as an empty module.
    pub files: Vec<ModuleFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleFile {
    /// What follows `s<id>.` in the file's name, e.g. `olean.server`.
    pub ext: String,
    pub sha256: String,
}

/// A snapshot entry: a labelled reference to a module.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EntryMeta {
    /// The id of the module it points at.
    pub module: String,
    pub labels: BTreeMap<String, String>,
    /// The idempotency key it was made with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Microseconds since the Unix epoch.
    pub created_micros: u64,
    /// The entry its create replaced, once it checked the statements, or the
    /// one that entry replaced, if it was deleted. Its `snapshot_id` is
    /// stored as the replaced entry's bare hex; the worker turns it into a
    /// versioned id when it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<Replaces>,
    /// What that create's comparison found, or the union of two after a
    /// splice, which an audit lists later. Set exactly when `replaces` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<Replacement>,
    /// The definitions it leaves for the entries that replace it to change:
    /// a spec's own, or those of the entry it replaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<String>,
    /// The code as its create sent it, where it imported runs by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// What those imports by run name resolved to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunImport>,
}

impl EntryMeta {
    /// Its creation time counted down from `u64::MAX`, as 16 hex digits: a
    /// listing cursor at the entry is this followed by its id.
    fn rank(&self) -> String {
        format!("{:016x}", u64::MAX - self.created_micros)
    }
}

/// One declaration of a module, as the search service indexes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDecl {
    pub name: String,
    pub kind: String,
    /// The declaration's signature, with notation; empty when it didn't
    /// print.
    #[serde(default)]
    pub signature: String,
    /// The type with full names and no notation.
    #[serde(default)]
    pub type_pp_full: String,
    #[serde(default)]
    pub type_refs: Vec<String>,
    #[serde(default)]
    pub value_refs: Vec<String>,
    #[serde(default)]
    pub docstring: Option<String>,
    #[serde(default)]
    pub deprecated_to: Option<String>,
    /// A definition's value, as the build printed it with full names and no
    /// notation, cut at [`VALUE_CHARS`] characters. None for other
    /// declarations, and in records from before the export printed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// A private declaration, which the review list shows and search leaves
    /// out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// Why it didn't print, if it didn't: the export has only its name and
    /// kind. Search leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Depends on `sorry`, by the module's check result.
    #[serde(default)]
    pub uses_sorry: bool,
    /// What its own axioms make it, by the module's check result. The
    /// export doesn't print it.
    #[serde(default = "proved")]
    pub status: ProofStatus,
}

/// How much of a definition's value the export prints.
pub const VALUE_CHARS: usize = 4000;

fn proved() -> ProofStatus {
    ProofStatus::Proved
}

/// The format of the exports builds make now: a module-system module's
/// export has its non-public declarations too, and one that didn't print is
/// kept with its error. Older exports of module-system modules are made
/// again ([`Catalog::stale_exports_after`]).
pub const EXPORT_FORMAT: u32 = 2;

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A module's declarations for the search service, with the module's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDecls {
    /// See [`ModuleMeta::header`].
    pub header: String,
    pub verified: bool,
    pub decls: Vec<SnapshotDecl>,
    /// [`EXPORT_FORMAT`] when the export was made; 0 before formats.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub format: u32,
}

impl SnapshotDecls {
    /// `decls` for the module `meta`, flagged from its check result, in
    /// today's format.
    pub fn new(meta: &ModuleMeta, decls: Vec<SnapshotDecl>) -> SnapshotDecls {
        let mut out = SnapshotDecls {
            header: meta.header.clone(),
            verified: false,
            decls,
            format: EXPORT_FORMAT,
        };
        out.refresh(meta);
        out
    }

    /// Takes `verified` from `meta`, and each declaration's `uses_sorry` and
    /// `status` from the declaration of its name in `meta`'s check result.
    /// The export and the check (the axiom report, or the kernel re-check's)
    /// walk the same module's constants by full name, and the check skips
    /// fewer of them (only internal names that aren't private), so the name
    /// finds each. One the check doesn't have has no status to be indexed
    /// with, so it is left out.
    pub(crate) fn refresh(&mut self, meta: &ModuleMeta) {
        self.verified = meta.verified;
        self.decls.retain_mut(|d| {
            let Some(checked) = meta.result.declarations.iter().find(|r| r.name == d.name) else {
                let module = meta.modules.last().map_or("", |m| m.id.as_str());
                tracing::warn!(module, name = %d.name, "the module's check has no such declaration");
                return false;
            };
            d.uses_sorry = checked.axioms.iter().any(|a| a == "sorryAx");
            d.status = checked.status;
            true
        });
    }
}

/// What a transaction changed, by id: readers look up the current state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

/// A [`Change`], with where it is in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logged {
    pub id: i64,
    /// Microseconds since the Unix epoch, by the clock of the worker that
    /// logged it.
    pub logged_micros: u64,
    pub change: Change,
}

/// How long [`Change`]s are kept; see [`Catalog::prune_log`].
pub const LOG_RETENTION: Duration = Duration::from_secs(24 * 3600);

/// A spec's amendment of an earlier spec: what its create found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amendment {
    /// The entry of the spec it amends, which may be gone since.
    pub amends: String,
    /// Why, as the request says.
    pub reason: String,
    /// What changed of the amended spec's declarations.
    pub changes: Vec<DeclarationChange>,
    /// The new spec's declarations that the amended one doesn't have.
    pub added: Vec<String>,
    /// The holes before and after, when the new spec's differ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holes: Option<AmendedHoles>,
}

/// A snapshot's refutation of a spec's target, as its create checked it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refutation {
    /// The spec's entry, which may be gone since.
    pub spec: String,
    /// The refuted target: a theorem of the spec's module.
    pub target: String,
    /// The refuting snapshot's theorem, which proves the negation of the
    /// target's statement.
    pub theorem: String,
    /// The hash of the target's statement; none in records made before it
    /// was kept.
    pub target_hash: Option<String>,
}

/// What a new entry is given.
#[derive(Default)]
pub struct NewEntry {
    pub labels: BTreeMap<String, String>,
    pub key: Option<String>,
    /// The entry it replaces, which the caller has compared it with.
    pub replaces: Option<Replaces>,
    /// What the comparison with it found; set exactly when `replaces` is.
    pub replacement: Option<Replacement>,
    /// The definitions it leaves for the entries that replace it to change.
    pub holes: Vec<String>,
    /// Whether it may replace an entry that another entry replaces already,
    /// making a fork. It isn't stored.
    pub fork: bool,
    /// The spec it amends, if it is a spec that amends another, which must
    /// still be there.
    pub amends: Option<Amendment>,
    /// The spec target it refutes, if it refutes one, whose spec must still
    /// be there.
    pub refutes: Option<Refutation>,
    /// The name of the run it starts; not with `replaces`. A name another run
    /// has is refused while that run has entries, unless it amends that
    /// run's spec, which moves the name to its run.
    pub name: Option<String>,
    /// The code as its create sent it, where it imported runs by name.
    pub source: Option<String>,
    /// What those imports by run name resolved to.
    pub runs: Vec<RunImport>,
}

/// The most bytes of JSON a record can have, so that rows, and the requests
/// that read many of them, stay small.
const MAX_RECORD_BYTES: usize = 6 * 1024 * 1024;

pub struct Catalog {
    db: Db,
    version: String,
}

/// Why a transaction attempt failed: the database, which
/// [`retry`](crate::db::retry) runs again on a conflict, or the request.
pub(crate) enum Fail {
    Db(DbError),
    Api(Error),
}

impl From<DbError> for Fail {
    fn from(e: DbError) -> Self {
        Fail::Db(e)
    }
}

impl From<diesel::result::Error> for Fail {
    fn from(e: diesel::result::Error) -> Self {
        Fail::Db(e.into())
    }
}

impl From<serde_json::Error> for Fail {
    fn from(e: serde_json::Error) -> Self {
        Fail::Api(internal(e))
    }
}

impl IsConflict for Fail {
    fn is_conflict(&self) -> bool {
        matches!(self, Fail::Db(e) if e.is_conflict())
    }
}

impl From<Fail> for Error {
    fn from(f: Fail) -> Self {
        match f {
            Fail::Db(e) => e.into(),
            Fail::Api(e) => e,
        }
    }
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, e.to_string())
}

/// Whether `value` can be stored as a record; see [`to_record`].
pub fn record_fits(value: &impl Serialize) -> bool {
    to_record(value).is_ok()
}

/// `value` as a record, refused as a `bad_request` if its JSON is over
/// [`MAX_RECORD_BYTES`] or has a NUL character, which PostgreSQL can't store.
pub(crate) fn to_record(value: &impl Serialize) -> Result<Value, Fail> {
    let value = serde_json::to_value(value)?;
    let bytes = serde_json::to_vec(&value)?.len();
    if bytes > MAX_RECORD_BYTES {
        return Err(Fail::Api(Error::new(
            ErrorKind::BadRequest,
            format!("snapshot record too large: {bytes} bytes, limit 6 MiB"),
        )));
    }
    if has_nul(&value) {
        return Err(Fail::Api(Error::new(
            ErrorKind::BadRequest,
            "snapshot record has a NUL character".to_string(),
        )));
    }
    Ok(value)
}

fn has_nul(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains('\0'),
        Value::Array(values) => values.iter().any(has_nul),
        Value::Object(fields) => fields.iter().any(|(k, v)| k.contains('\0') || has_nul(v)),
        _ => false,
    }
}

/// An entry's id, module, labels, key, creation time, holes (JSON text), the
/// code as sent and what its imports by run name resolved to (JSON text), as
/// selected.
pub(crate) type EntryRow = (
    String,
    String,
    Value,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// A link: the entry that replaces, the entry it replaces, what it allowed
/// (JSON text) and the comparison (JSON text), as selected.
pub(crate) type LinkRow = (String, String, Option<String>, String);

/// The entry of `row`, with its link if it has one.
pub(crate) fn entry_from_row(
    row: EntryRow,
    link: Option<LinkRow>,
) -> Result<(String, EntryMeta), Fail> {
    let (e, module, labels, key, created, holes, source, runs) = row;
    // A key is stored after its scope's text; give it back without.
    let scope = crate::scope::current();
    let key = key.map(|k| match k.strip_prefix(&scope.key("")) {
        Some(own) if !scope.is_empty() => own.to_string(),
        _ => k,
    });
    let (replaces, replacement) = match link {
        Some((_, replaced, allow, replacement)) => (
            Some(Replaces {
                snapshot_id: replaced,
                allow: allow.map(|text| serde_json::from_str(&text)).transpose()?,
            }),
            Some(serde_json::from_str(&replacement)?),
        ),
        None => (None, None),
    };
    let meta = EntryMeta {
        module,
        labels: serde_json::from_value(labels)?,
        key,
        created_micros: created as u64,
        replaces,
        replacement,
        holes: holes
            .map(|text| serde_json::from_str(&text))
            .transpose()?
            .unwrap_or_default(),
        source,
        runs: runs
            .map(|text| serde_json::from_str(&text))
            .transpose()?
            .unwrap_or_default(),
    };
    Ok((e, meta))
}

/// What a link's `allow` column holds for `replaced`: its `allow` as JSON
/// text.
pub(crate) fn allow_column(replaced: &Replaces) -> Result<Option<String>, Fail> {
    Ok(replaced
        .allow
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?)
}

/// The link and comparison of `replacer` once `deleted`, which it replaces,
/// is gone: `deleted`'s link, allowing what either step allowed, and both
/// steps' comparisons. The allows are what the links' authors declared,
/// which the audit reads; a comparison is what a create's build reported.
/// `None` if `deleted` has no link.
pub(crate) fn splice(deleted: &EntryMeta, replacer: &EntryMeta) -> Option<(Replaces, Replacement)> {
    let link = deleted.replaces.as_ref()?;
    let none = Replacement::default();
    let first = deleted.replacement.as_ref().unwrap_or(&none);
    let second = replacer.replacement.as_ref().unwrap_or(&none);
    let replacement = Replacement {
        filled: union(&first.filled, &second.filled),
        changed_definitions: union(&first.changed_definitions, &second.changed_definitions),
        affected: union(&first.affected, &second.affected),
    };
    let allowed = |r: &EntryMeta| {
        let allow = r.replaces.as_ref().and_then(|r| r.allow.as_ref());
        allow.map_or(&[][..], |a| a.definitions.as_slice()).to_vec()
    };
    let definitions = union(&allowed(deleted), &allowed(replacer));
    let allow = (!definitions.is_empty()).then_some(Allow { definitions });
    let replaces = Replaces {
        snapshot_id: link.snapshot_id.clone(),
        allow,
    };
    Some((replaces, replacement))
}

/// `first`, then the names of `second` it lacks, each name once by its
/// [`normalised`] form.
fn union(first: &[String], second: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let names = first.iter().chain(second);
    names
        .filter(|name| seen.insert(normalised(name).into_owned()))
        .cloned()
        .collect()
}

/// The error for a module that isn't kept.
pub fn unknown_module(m: &str) -> Error {
    Error::new(
        ErrorKind::NotFound,
        format!("unknown snapshot {}", snapshot::module(m)),
    )
}

/// The error for entry `e` of version `v`, which isn't kept, by the
/// snapshot id that names it.
pub(crate) fn unknown_entry(v: &str, e: &str) -> Error {
    let id = SnapshotId {
        version: v.to_string(),
        entry: e.to_string(),
    };
    Error::new(ErrorKind::NotFound, format!("unknown snapshot {id}"))
}

/// The refusal of a run name that another run has.
pub fn name_taken(v: &str, name: &str, root: &str) -> Error {
    let id = SnapshotId {
        version: v.to_string(),
        entry: root.to_string(),
    };
    Error::new(
        ErrorKind::Conflict,
        format!(
            "the run name {name} is taken by the run that {id} starts: name this run otherwise, \
             or amend that spec to take its name over"
        ),
    )
}

pub(crate) fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// `labels` with each key once, or `None` if they give a key two values,
/// which no entry matches: an entry has one value per key.
fn wanted(labels: &[(String, String)]) -> Option<Vec<(String, String)>> {
    // A read keeps to the request's scope too.
    let scope = crate::scope::current().pairs();
    let mut wanted: BTreeMap<&str, &str> = BTreeMap::new();
    for (k, value) in labels.iter().chain(&scope) {
        if wanted.insert(k, value).is_some_and(|old| old != value) {
            return None;
        }
    }
    Some(
        wanted
            .into_iter()
            .map(|(k, value)| (k.to_string(), value.to_string()))
            .collect(),
    )
}

/// The creation time and id of the entry at listing cursor `after` (see
/// [`is_cursor`](pisa_protocol::labels::is_cursor)).
fn parse_cursor(after: &str) -> Result<(i64, &str), Error> {
    let bad = || Error::new(ErrorKind::BadRequest, format!("bad cursor {after:?}"));
    let (r, e) = (after.get(..16).ok_or_else(bad)?, &after[16..]);
    let r = u64::from_str_radix(r, 16).map_err(|_| bad())?;
    Ok((i64::try_from(u64::MAX - r).unwrap_or(i64::MAX), e))
}

impl Catalog {
    pub fn new(db: Db, version: String) -> Catalog {
        Catalog { db, version }
    }

    pub async fn module(&self, m: &str) -> Result<ModuleMeta, Error> {
        dispatch!(&self.db, module(&self.version, m))?.ok_or_else(|| unknown_module(m))
    }

    /// What a listing shows of each of `ms` that is kept, by id, without
    /// their code, check or closure: a few queries, whatever their number.
    pub async fn summaries(&self, ms: &[String]) -> Result<HashMap<String, ModuleSummary>, Error> {
        dispatch!(&self.db, summaries(&self.version, ms))
    }

    /// The records of each of `ms` that is kept, by id, without the builds
    /// of what they need: a few queries, whatever their number.
    pub async fn records(&self, ms: &[String]) -> Result<HashMap<String, ModuleRecord>, Error> {
        dispatch!(&self.db, records(&self.version, ms))
    }

    /// The imports of each module that one of `ms` needs, `ms` among them,
    /// in the header's order: one query, whatever their number.
    /// [`closure_order`] orders a module's closure from them.
    pub async fn import_graph(&self, ms: &[String]) -> Result<HashMap<String, Vec<String>>, Error> {
        dispatch!(&self.db, import_graph(&self.version, ms))
    }

    /// Rewrites module `m`'s record with `f` if the module's own build is
    /// still `build`. `None` if it isn't, because the module was deleted and
    /// built again: the record is then left alone.
    pub async fn update_module(
        &self,
        m: &str,
        build: &str,
        f: impl Fn(&mut ModuleMeta) + Sync,
    ) -> Result<Option<ModuleMeta>, Error> {
        dispatch!(&self.db, update_module(&self.version, m, build, &f))
    }

    /// The entry made with idempotency key `k`, if there is one.
    pub async fn keyed(&self, k: &str) -> Result<Option<(String, EntryMeta)>, Error> {
        dispatch!(&self.db, keyed(&self.version, k))
    }

    /// Stores a freshly built module (unless it exists) with its imports,
    /// plus a new entry on it. Returns the entry and the module's stored
    /// record, which is another worker's if it stored the module first. A
    /// missing parent is `not_found`, and so is one deleted and built again
    /// since `meta` was made from it, whose files `meta` would name, and so
    /// is the entry `entry` replaces, if it is gone. If another entry
    /// replaces that one, it is a `conflict`, unless `entry` forks.
    ///
    /// If `entry`'s key is already on an entry, nothing is written and that
    /// entry comes back, unchanged. On the same module, with its stored
    /// record; on another, with `meta` in place of a record, which callers
    /// tell by the entry's `module`. A `meta` or `decls` that can't be
    /// stored (see [`record_fits`]) is a `bad_request`, and nothing is
    /// written either. `decls` is stored with the module, if this stores it.
    pub async fn commit(
        &self,
        m: &str,
        meta: &ModuleMeta,
        decls: Option<&SnapshotDecls>,
        entry: NewEntry,
    ) -> Result<(String, EntryMeta, ModuleMeta), Error> {
        // Refused before the transaction if it is too large.
        to_record(&ModuleBody::of(meta))?;
        let decls = decls.map(to_record).transpose()?;
        dispatch!(
            &self.db,
            commit(&self.version, m, meta, decls.as_ref(), &entry)
        )
    }

    /// Stores module `m`'s record and `decls` with no entry, as [`commit`]
    /// does, for a build that its create's relation refused: the same code
    /// created again then finds it kept, and until then nothing lists or
    /// searches it, and a sweep removes it once it is older than its grace.
    /// Returns the stored record, another worker's if it stored the module
    /// first.
    ///
    /// [`commit`]: Self::commit
    pub async fn commit_module(
        &self,
        m: &str,
        meta: &ModuleMeta,
        decls: Option<&SnapshotDecls>,
    ) -> Result<ModuleMeta, Error> {
        to_record(&ModuleBody::of(meta))?;
        let decls = decls.map(to_record).transpose()?;
        dispatch!(
            &self.db,
            commit_module(&self.version, m, meta, decls.as_ref())
        )
    }

    /// A new entry on an existing module; `None` if the module is gone. If
    /// `entry`'s key is already on an entry, that one comes back instead,
    /// unchanged, and it may be on another module. The entry `entry`
    /// replaces being gone is `not_found`, and being replaced already is a
    /// `conflict`, unless `entry` forks.
    pub async fn add_entry(
        &self,
        m: &str,
        entry: NewEntry,
    ) -> Result<Option<(String, EntryMeta)>, Error> {
        dispatch!(&self.db, add_entry(&self.version, m, &entry))
    }

    pub async fn entry(&self, e: &str) -> Result<EntryMeta, Error> {
        self.find_entry(e)
            .await?
            .ok_or_else(|| unknown_entry(&self.version, e))
    }

    /// Removes the entry; returns its module's record if that module was
    /// cleaned up too. Each entry that replaces it replaces what it replaced
    /// instead, with the two links composed; if it replaced nothing, they keep
    /// their links to it.
    pub async fn remove_entry(&self, e: &str) -> Result<(EntryMeta, Option<ModuleMeta>), Error> {
        dispatch!(&self.db, remove_entry(&self.version, e))
    }

    /// Removes the entry and every entry that replaces it, directly or
    /// through others, in one transaction, and splices nothing. Returns their
    /// ids, the entry's first, and each module that went with them, with its
    /// record, importers first. What the entry replaced keeps its other
    /// replacers, and is a head again if it has none.
    pub async fn remove_branch(
        &self,
        e: &str,
    ) -> Result<(Vec<String>, Vec<(String, ModuleMeta)>), Error> {
        dispatch!(&self.db, remove_branch(&self.version, e))
    }

    /// The entries, newest first, that have every `(key, value)` in `labels`
    /// (every entry if there are none), at most `limit` of them, starting
    /// after the cursor `after` (see [`is_cursor`]). With `heads`, only those
    /// that no other entry replaces. Also returns the cursor at the last
    /// entry if there are `limit`, to go on from.
    ///
    /// [`is_cursor`]: pisa_protocol::labels::is_cursor
    pub async fn list(
        &self,
        labels: &[(String, String)],
        limit: usize,
        after: Option<&str>,
        heads: bool,
    ) -> Result<(Vec<(String, EntryMeta)>, Option<String>), Error> {
        let Some(wanted) = wanted(labels) else {
            return Ok((Vec::new(), None));
        };
        let after = after.map(parse_cursor).transpose()?;
        let limit_rows = i64::try_from(limit).unwrap_or(i64::MAX);
        let entries: Vec<(String, EntryMeta)> = dispatch!(
            &self.db,
            list(&self.version, &wanted, limit_rows, after, heads)
        )?;
        let next = match entries.last() {
            Some((e, meta)) if entries.len() == limit => Some(format!("{}{e}", meta.rank())),
            _ => None,
        };
        Ok((entries, next))
    }

    /// Each module that imports module `m`, directly or through others, with
    /// those of its imports that are `m` or such modules.
    pub async fn dependents(&self, m: &str) -> Result<BTreeMap<String, Vec<String>>, Error> {
        dispatch!(&self.db, dependents(&self.version, m))
    }

    /// The entries of this version whose `replaces` is `entry` (its bare
    /// hex), whatever their labels and whether or not `entry` is kept, oldest
    /// first.
    pub async fn replacers(&self, entry: &str) -> Result<Vec<(String, EntryMeta)>, Error> {
        dispatch!(&self.db, replacers(&self.version, entry))
    }

    /// The root of the run each of `names` names, by name; a name no run has
    /// is missing.
    pub async fn run_roots(&self, names: &[String]) -> Result<HashMap<String, String>, Error> {
        dispatch!(&self.db, run_roots(&self.version, names))
    }

    /// The name of the run each of `entries` (bare hex) is in, for those
    /// whose run has one.
    pub async fn run_names_of(&self, entries: &[String]) -> Result<HashMap<String, String>, Error> {
        dispatch!(&self.db, run_names_of(&self.version, entries))
    }

    /// Names `name` the run entry `e` (bare hex) is in, in the request's
    /// scope: a name the run had goes, and a name that another run with an
    /// entry has is a conflict.
    pub async fn rename_run(&self, e: &str, name: &str) -> Result<(), Error> {
        dispatch!(&self.db, rename_run(&self.version, e, name))
    }

    /// The root of the run named `name`, if a run has it, and whether that
    /// run has an entry left: a name whose run has none is free.
    pub async fn run_named(&self, name: &str) -> Result<Option<(String, bool)>, Error> {
        dispatch!(&self.db, run_named(&self.version, name))
    }

    /// Every entry of the runs that `entries` (bare hex) are on, by id, with
    /// its link, in one read of the catalog: the entries a run starts at,
    /// and those linked to them through what replaces what. A run's root is
    /// absent once it is deleted, and what replaced it keeps its link.
    pub async fn runs(&self, entries: &[String]) -> Result<HashMap<String, EntryMeta>, Error> {
        dispatch!(&self.db, runs(&self.version, entries))
    }

    /// The entries of this version that replace each of `entries`, oldest
    /// first, keyed by the replaced entry; entries nothing replaces are
    /// absent. A few queries at most, whatever the number of entries.
    pub async fn replacers_among(
        &self,
        entries: &[String],
    ) -> Result<HashMap<String, Vec<(String, EntryMeta)>>, Error> {
        dispatch!(&self.db, replacers_among(&self.version, entries))
    }

    /// The entries on `modules` that have every `(key, value)` in `labels`,
    /// oldest first.
    pub async fn entries_on(
        &self,
        modules: &[String],
        labels: &[(String, String)],
    ) -> Result<Vec<(String, EntryMeta)>, Error> {
        let Some(wanted) = wanted(labels) else {
            return Ok(Vec::new());
        };
        dispatch!(&self.db, entries_on(&self.version, modules, &wanted))
    }

    /// Applies the merge patch `labels` to entry `e`'s labels, removing each
    /// label it maps to `None` and setting each it maps to `Some`, and
    /// returns the entry. The labels that result must follow the rules, or
    /// nothing changes and it is a `bad_request`.
    pub async fn set_labels(
        &self,
        e: &str,
        labels: &BTreeMap<String, Option<String>>,
    ) -> Result<EntryMeta, Error> {
        dispatch!(&self.db, set_labels(&self.version, e, labels))
    }

    /// Removes module `m` if no entry references it and no module imports it.
    pub async fn cleanup(&self, m: &str) -> Result<Option<ModuleMeta>, Error> {
        dispatch!(&self.db, cleanup(&self.version, m))
    }

    /// Deletes the [`Change`]s older than `older_than`; returns how many.
    pub async fn prune_log(&self, older_than: Duration) -> Result<usize, Error> {
        dispatch!(&self.db, prune_log(&self.version, older_than))
    }

    /// Up to `limit` entries in id order, after the entry `after`.
    pub async fn entries_after(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<(String, EntryMeta)>, Error> {
        dispatch!(&self.db, entries_after(&self.version, after, limit.into()))
    }

    /// The modules that no entry is on and no module imports, what a delete
    /// that stopped partway leaves, among the next `limit` modules in id
    /// order after `after`; and the module to go on after, if there may be
    /// more. `limit` stays within a query's bound values.
    pub async fn orphans_after(
        &self,
        after: Option<&str>,
        limit: u32,
        built_by: u64,
    ) -> Result<(Vec<String>, Option<String>), Error> {
        dispatch!(
            &self.db,
            orphans_after(&self.version, after, limit.into(), built_by)
        )
    }

    /// The own build of each of `ids` that has a record, read without the
    /// rest of the record.
    pub async fn own_builds(&self, ids: &[String]) -> Result<HashMap<String, String>, Error> {
        dispatch!(&self.db, own_builds(&self.version, ids))
    }

    /// Up to `limit` ids, in order, of the modules with declarations for
    /// search, after the module `after`.
    pub async fn modules_with_decls_after(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<String>, Error> {
        dispatch!(
            &self.db,
            modules_with_decls_after(&self.version, after, limit.into())
        )
    }

    /// Up to `limit` modules, in order of id after the module `after`, with
    /// their own builds, whose declarations for search are a module-system
    /// module's export older than [`EXPORT_FORMAT`].
    pub async fn stale_exports_after(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<(String, String)>, Error> {
        dispatch!(
            &self.db,
            stale_exports_after(&self.version, after, limit.into())
        )
    }

    /// Stores `decls` as module `m`'s declarations for search, flagged from
    /// its record, and logs the change, if the module's own build is still
    /// `build`; returns whether it stored them. Declarations that can't be
    /// stored (see [`record_fits`]) are a `bad_request`.
    pub async fn replace_decls(
        &self,
        m: &str,
        build: &str,
        decls: &SnapshotDecls,
    ) -> Result<bool, Error> {
        to_record(decls)?;
        dispatch!(&self.db, replace_decls(&self.version, m, build, decls))
    }

    /// The [`Change`]s logged at `since_micros` or later, oldest first, at most
    /// `limit` of them, after the time and id `after` of one from a previous
    /// page.
    pub async fn changes(
        &self,
        since_micros: u64,
        after: Option<(u64, i64)>,
        limit: u32,
    ) -> Result<Vec<Logged>, Error> {
        dispatch!(
            &self.db,
            changes(&self.version, since_micros, after, limit.into())
        )
    }

    /// Stores `result`, what `POST /snapshots/accept` answered, as
    /// acceptance `id`, a new random id, of the spec entries `specs`.
    /// Refused like a record that is too large.
    pub async fn put_acceptance(
        &self,
        id: &str,
        specs: &[String],
        result: &impl Serialize,
    ) -> Result<(), Error> {
        let result = to_record(result).map_err(Error::from)?;
        dispatch!(&self.db, put_acceptance(&self.version, id, specs, &result))
    }

    /// The stored results of spec entry `spec`'s acceptances, newest first,
    /// at most `limit`.
    pub async fn acceptances_of(&self, spec: &str, limit: u32) -> Result<Vec<Value>, Error> {
        dispatch!(&self.db, acceptances_of(&self.version, spec, limit.into()))
    }

    /// The amendments that `entries` make or are subject to, by the entry
    /// that amends, in no order.
    pub async fn amendments_of(
        &self,
        entries: &[String],
    ) -> Result<Vec<(String, Amendment)>, Error> {
        dispatch!(&self.db, amendments_of(&self.version, entries))
    }

    /// The refutations of the targets of `entries`, and those `entries`
    /// make, by the refuting entry, in no order.
    pub async fn refutations_of(
        &self,
        entries: &[String],
    ) -> Result<Vec<(String, Refutation)>, Error> {
        dispatch!(&self.db, refutations_of(&self.version, entries))
    }

    /// Acceptance `id`'s stored result, if it has one.
    pub async fn acceptance(&self, id: &str) -> Result<Option<Value>, Error> {
        dispatch!(&self.db, acceptance(&self.version, id))
    }

    /// This version's catalog's id: random, made the first time it is asked
    /// for, and the same from then on. The object store's marker repeats it,
    /// so that a worker sweeps only a store its own catalog wrote.
    pub async fn identity(&self) -> Result<String, Error> {
        dispatch!(&self.db, identity(&self.version))
    }

    /// Records what this version's sweep of the store's files came to: why
    /// it was refused, or `None` once it swept.
    pub async fn record_sweep(&self, refused: Option<&str>) -> Result<(), Error> {
        dispatch!(&self.db, record_sweep(&self.version, refused))
    }

    /// Why this version's sweeps of the store's files are refused, and since
    /// when (microseconds since the Unix epoch), if they are.
    pub async fn sweep_refused(&self) -> Result<Option<(String, u64)>, Error> {
        dispatch!(&self.db, sweep_refused(&self.version))
    }

    /// Takes or renews lease `name` for `owner` for `ttl`, unless another
    /// owner holds it; returns when it ends if `owner` holds it now.
    pub async fn take_lease(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<Option<u64>, Error> {
        dispatch!(&self.db, take_lease(&self.version, name, owner, ttl))
    }

    /// Whether any entry is on module `m`.
    pub async fn has_entries(&self, m: &str) -> Result<bool, Error> {
        dispatch!(&self.db, has_entries(&self.version, m))
    }

    /// Whether an entry on module `m` is a head: one that no entry replaces.
    pub async fn has_heads(&self, m: &str) -> Result<bool, Error> {
        dispatch!(&self.db, has_heads(&self.version, m))
    }

    /// Those of `entries` (bare hex) that an entry replaces, so aren't heads.
    pub async fn replaced_among(&self, entries: &[String]) -> Result<HashSet<String>, Error> {
        dispatch!(&self.db, replaced_among(&self.version, entries))
    }

    /// Entry `e`, if it exists.
    pub async fn find_entry(&self, e: &str) -> Result<Option<EntryMeta>, Error> {
        dispatch!(&self.db, entry(&self.version, e))
    }

    /// Module `m`'s declarations for search, if it is kept and has them.
    pub async fn decls(&self, m: &str) -> Result<Option<SnapshotDecls>, Error> {
        dispatch!(&self.db, decls(&self.version, m))
    }

    /// The declarations for search of each of `ms` that is kept and has
    /// them, by id: a few queries, whatever their number.
    pub async fn decls_of(&self, ms: &[String]) -> Result<HashMap<String, SnapshotDecls>, Error> {
        dispatch!(&self.db, decls_of(&self.version, ms))
    }

    /// The search service's records for module `m`'s declarations, by name.
    pub async fn described(&self, m: &str) -> Result<Vec<(String, Value)>, Error> {
        dispatch!(&self.db, described(&self.version, m))
    }

    /// Stores the search service's records for declarations of module `m`,
    /// by name, unless the module is gone; returns whether it stored them.
    pub async fn put_described(&self, m: &str, records: &[(String, Value)]) -> Result<bool, Error> {
        dispatch!(&self.db, put_described(&self.version, m, records))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use pisa_protocol::verify::{Declaration, ReportedDeclaration};

    fn meta(id: &str) -> ModuleMeta {
        ModuleMeta {
            imports: Vec::new(),
            modules: vec![Module {
                id: id.into(),
                build: "b".into(),
                files: Vec::new(),
            }],
            header: "import PisaSnap.sx".into(),
            code: String::new(),
            status: ProofStatus::Proved,
            verified: false,
            macros: false,
            result: CheckResult::default(),
            created: 0,
        }
    }

    /// `meta(id)` importing `parent`, built on it.
    fn child(id: &str, parent: &ModuleMeta) -> ModuleMeta {
        let mut m = meta(id);
        let own = m.modules.pop().unwrap();
        m.imports = vec![parent.modules.last().unwrap().id.clone()];
        m.modules = [parent.modules.clone(), vec![own]].concat();
        m
    }

    /// A theorem `name` of a check result, with `axioms` and `status`.
    fn checked(name: &str, axioms: &[&str], status: ProofStatus) -> Declaration {
        let reported = ReportedDeclaration {
            name: name.into(),
            kind: "theorem".into(),
            axioms: axioms.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        };
        reported.with_status(status)
    }

    fn decl(name: &str) -> SnapshotDecl {
        SnapshotDecl {
            name: name.into(),
            kind: "theorem".into(),
            signature: format!("{name} : True"),
            type_pp_full: "True".into(),
            type_refs: Vec::new(),
            value_refs: Vec::new(),
            docstring: None,
            deprecated_to: None,
            value: None,
            private: false,
            error: None,
            uses_sorry: false,
            status: ProofStatus::Proved,
        }
    }

    fn no_labels() -> NewEntry {
        NewEntry {
            labels: BTreeMap::new(),
            key: None,
            replaces: None,
            replacement: None,
            holes: Vec::new(),
            fork: false,
            amends: None,
            refutes: None,
            name: None,
            source: None,
            runs: Vec::new(),
        }
    }

    fn labelled(labels: &[(&str, &str)]) -> NewEntry {
        NewEntry {
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            key: None,
            replaces: None,
            replacement: None,
            holes: Vec::new(),
            fork: false,
            amends: None,
            refutes: None,
            name: None,
            source: None,
            runs: Vec::new(),
        }
    }

    fn keyed(k: &str) -> NewEntry {
        NewEntry {
            labels: BTreeMap::new(),
            key: Some(k.into()),
            replaces: None,
            replacement: None,
            holes: Vec::new(),
            fork: false,
            amends: None,
            refutes: None,
            name: None,
            source: None,
            runs: Vec::new(),
        }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// The link to `old` that allows `allow`, or no `allow` if that is empty.
    fn link(old: &str, allow: &[&str]) -> Option<Replaces> {
        Some(Replaces {
            snapshot_id: old.into(),
            allow: (!allow.is_empty()).then(|| Allow {
                definitions: names(allow),
            }),
        })
    }

    fn replacement(filled: &[&str], changed: &[&str], affected: &[&str]) -> Replacement {
        Replacement {
            filled: names(filled),
            changed_definitions: names(changed),
            affected: names(affected),
        }
    }

    /// A new entry that replaces `old` and allows `changed`, whose
    /// comparison found `filled`, `changed` and `affected`.
    fn step(old: &str, filled: &[&str], changed: &[&str], affected: &[&str]) -> NewEntry {
        NewEntry {
            replaces: link(old, changed),
            replacement: Some(replacement(filled, changed, affected)),
            ..no_labels()
        }
    }

    /// Adds `entry` on module `x`, which must exist; returns its id.
    async fn added(catalog: &Catalog, entry: NewEntry) -> String {
        catalog.add_entry("x", entry).await.unwrap().unwrap().0
    }

    /// The ids of the entries that replace `e`.
    async fn replacer_ids(catalog: &Catalog, e: &str) -> Vec<String> {
        let found = catalog.replacers(e).await.unwrap();
        found.into_iter().map(|(r, _)| r).collect()
    }

    /// A catalog on in-memory SQLite, and one on the PostgreSQL at
    /// `PISA_TEST_DATABASE_URL` if that is set, each with a version of its
    /// own so that runs can share a database. Start a throwaway one with:
    ///
    /// ```sh
    /// docker run -d --rm --name pisa-pg -e POSTGRES_PASSWORD=pisa -p 127.0.0.1:5433:5432 postgres:18
    /// PISA_TEST_DATABASE_URL=postgres://postgres:pisa@127.0.0.1:5433/postgres cargo test -p pisa-db
    /// docker stop pisa-pg
    /// ```
    async fn catalogs() -> Vec<Arc<Catalog>> {
        let mut dbs = vec![Db::memory().await];
        match std::env::var("PISA_TEST_DATABASE_URL") {
            Ok(url) => dbs.push(Db::connect(Some(&url)).await.unwrap()),
            Err(_) => eprintln!("PISA_TEST_DATABASE_URL unset: SQLite only"),
        }
        dbs.into_iter()
            .map(|db| {
                let version = uuid::Uuid::new_v4().simple().to_string();
                Arc::new(Catalog::new(db, version))
            })
            .collect()
    }

    #[tokio::test]
    async fn a_recheck_refreshes_the_declarations_and_is_logged() {
        for catalog in catalogs().await {
            let mut m = meta("x");
            m.result.declarations = ["a", "b", "c"]
                .map(|n| checked(n, &[], ProofStatus::Proved))
                .into();
            let decls = SnapshotDecls::new(&m, vec![decl("a"), decl("b"), decl("c")]);
            catalog
                .commit("x", &m, Some(&decls), no_labels())
                .await
                .unwrap();
            let before = catalog.changes(0, None, 100).await.unwrap().len();

            catalog
                .update_module("x", "b", |meta| {
                    meta.verified = true;
                    meta.status = ProofStatus::Sorry;
                    meta.result.declarations = vec![
                        checked("a", &[], ProofStatus::Proved),
                        checked("b", &["sorryAx"], ProofStatus::Sorry),
                    ];
                })
                .await
                .unwrap();
            let decls = catalog.decls("x").await.unwrap().unwrap();
            assert!(decls.verified);
            // `c`, which the re-check doesn't have, has no status to index.
            let names: Vec<_> = decls.decls.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(names, ["a", "b"]);
            assert!(!decls.decls[0].uses_sorry && decls.decls[1].uses_sorry);
            let statuses: Vec<_> = decls.decls.iter().map(|d| d.status).collect();
            assert_eq!(statuses, [ProofStatus::Proved, ProofStatus::Sorry]);
            let changes = catalog.changes(0, None, 100).await.unwrap();
            assert_eq!(changes.len(), before + 1);
            let last = &changes.last().unwrap().change;
            assert_eq!(
                (last.entry.as_deref(), last.module.as_deref()),
                (None, Some("x"))
            );
            // A record from another build is left alone.
            let other = catalog.update_module("x", "c", |meta| meta.status = ProofStatus::Proved);
            assert!(other.await.unwrap().is_none());
            assert_eq!(
                catalog.module("x").await.unwrap().status,
                ProofStatus::Sorry
            );
        }
    }

    /// `meta(id)` with a module-system header.
    fn module_system(id: &str) -> ModuleMeta {
        let mut m = meta(id);
        m.header = "module\n\npublic import PisaSnap.sx".into();
        m.result.declarations = vec![checked("a", &[], ProofStatus::Proved)];
        m
    }

    #[tokio::test]
    async fn the_exports_to_redo_are_module_system_ones_of_an_older_format() {
        for catalog in catalogs().await {
            for (id, system, format) in [
                ("w", true, None),
                ("x", true, Some(0)),
                ("y", true, Some(EXPORT_FORMAT)),
                ("z", false, Some(0)),
                ("zz", true, Some(1)),
            ] {
                let m = if system { module_system(id) } else { meta(id) };
                let decls = format.map(|format| SnapshotDecls {
                    format,
                    ..SnapshotDecls::new(&m, vec![decl("a")])
                });
                catalog
                    .commit(id, &m, decls.as_ref(), no_labels())
                    .await
                    .unwrap();
            }
            let stale = catalog.stale_exports_after(None, 10).await.unwrap();
            let pair = |id: &str| (id.to_string(), "b".to_string());
            assert_eq!(stale, [pair("x"), pair("zz")]);
            let page = catalog.stale_exports_after(None, 1).await.unwrap();
            assert_eq!(page, [pair("x")]);
            let next = catalog.stale_exports_after(Some("x"), 1).await.unwrap();
            assert_eq!(next, [pair("zz")]);
        }
    }

    #[tokio::test]
    async fn an_export_replaced_for_its_build_is_logged() {
        for catalog in catalogs().await {
            let m = module_system("x");
            let old = SnapshotDecls {
                format: 0,
                ..SnapshotDecls::new(&m, vec![decl("a")])
            };
            catalog
                .commit("x", &m, Some(&old), no_labels())
                .await
                .unwrap();
            let before = catalog.changes(0, None, 100).await.unwrap().len();
            let mut private = decl("_private.PisaSnap.sx.0.b");
            private.private = true;
            let mut m = m;
            m.result
                .declarations
                .push(checked(&private.name, &[], ProofStatus::Proved));
            let new = SnapshotDecls::new(&m, vec![decl("a"), private]);
            assert!(catalog.replace_decls("x", "b", &new).await.unwrap());
            let stored = catalog.decls("x").await.unwrap().unwrap();
            // Flagged from the stored record's check, which has no `b`.
            assert_eq!(stored.format, EXPORT_FORMAT);
            let names: Vec<_> = stored.decls.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(names, ["a"]);
            let changes = catalog.changes(0, None, 100).await.unwrap();
            assert_eq!(changes.len(), before + 1);
            assert_eq!(changes.last().unwrap().change.module.as_deref(), Some("x"));
            // Another build's, or a module's that is gone, is not stored.
            assert!(!catalog.replace_decls("x", "c", &old).await.unwrap());
            assert!(!catalog.replace_decls("nope", "b", &old).await.unwrap());
            assert_eq!(
                catalog.decls("x").await.unwrap().unwrap().format,
                EXPORT_FORMAT
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_entry_is_named_by_its_snapshot_id() {
        for catalog in catalogs().await {
            let e = "0".repeat(32);
            let unknown = format!("unknown snapshot {}~{e}", catalog.version);
            assert_eq!(catalog.entry(&e).await.unwrap_err().message, unknown);
            let removed = catalog.remove_entry(&e).await;
            assert_eq!(removed.unwrap_err().message, unknown);
            let dropped = catalog.remove_branch(&e).await;
            assert_eq!(dropped.unwrap_err().message, unknown);
            let labelled = catalog.set_labels(&e, &BTreeMap::new()).await;
            assert_eq!(labelled.unwrap_err().message, unknown);
        }
    }

    #[tokio::test]
    async fn an_entry_keeps_its_holes() {
        for catalog in catalogs().await {
            for holes in [Vec::new(), names(&["f", "_private.h"])] {
                let entry = NewEntry {
                    holes: holes.clone(),
                    ..no_labels()
                };
                let (e, added, _) = catalog.commit("x", &meta("x"), None, entry).await.unwrap();
                assert_eq!(added.holes, holes);
                assert_eq!(catalog.entry(&e).await.unwrap().holes, holes);
                let (listed, _) = catalog.list(&[], 10, None, false).await.unwrap();
                let found = listed.iter().find(|(id, _)| *id == e).unwrap();
                assert_eq!(found.1.holes, holes);
            }
        }
    }

    #[tokio::test]
    async fn an_entry_keeps_the_link_to_the_entry_it_replaced() {
        use pisa_protocol::api::Allow;
        for catalog in catalogs().await {
            let (old, _, _) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let named = Allow {
                definitions: vec!["f".into(), "_private.h".into()],
            };
            for allow in [None, Some(named)] {
                let replaces = Some(Replaces {
                    snapshot_id: old.clone(),
                    allow,
                });
                let entry = NewEntry {
                    replaces: replaces.clone(),
                    fork: true,
                    ..no_labels()
                };
                let (e, added) = catalog.add_entry("x", entry).await.unwrap().unwrap();
                assert_eq!(added.replaces, replaces);
                assert_eq!(catalog.entry(&e).await.unwrap().replaces, replaces);
                let (listed, _) = catalog.list(&[], 10, None, false).await.unwrap();
                let found = listed.iter().find(|(id, _)| *id == e).unwrap();
                assert_eq!(found.1.replaces, replaces);
            }
            assert_eq!(catalog.entry(&old).await.unwrap().replaces, None);
        }
    }

    #[tokio::test]
    async fn a_scope_labels_what_it_creates_and_bounds_what_it_reads() {
        use crate::scope::{Scope, within};
        for catalog in catalogs().await {
            let (a, b) = (
                Scope::parse("user=a").unwrap(),
                Scope::parse("user=b").unwrap(),
            );
            let entry = |key: &str, name: &str, labels: &[(&str, &str)]| NewEntry {
                key: Some(key.into()),
                name: Some(name.into()),
                labels: labels
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                ..no_labels()
            };
            let made = within(a.clone(), async {
                let (e, meta, _) = catalog
                    .commit("m", &meta("m"), None, entry("k", "n", &[("run", "1")]))
                    .await
                    .unwrap();
                // Stamped, with its key given back as it was sent.
                let wanted =
                    BTreeMap::from([("run".into(), "1".into()), ("user".into(), "a".into())]);
                assert_eq!((meta.labels, meta.key.as_deref()), (wanted, Some("k")));
                assert_eq!(catalog.keyed("k").await.unwrap().unwrap().0, e);
                assert_eq!(catalog.run_roots(&["n".into()]).await.unwrap()["n"], e);
                // Its scope's label can't change, nor be set otherwise.
                let edit = BTreeMap::from([("user".to_string(), None)]);
                let refused = catalog.set_labels(&e, &edit).await.unwrap_err();
                assert_eq!(refused.kind, ErrorKind::BadRequest);
                let other = catalog
                    .add_entry("m", entry("k2", "n2", &[("user", "b")]))
                    .await
                    .unwrap_err();
                assert_eq!(other.kind, ErrorKind::BadRequest);
                e
            })
            .await;
            within(b.clone(), async {
                // Nothing of `a`'s is there: not the entry, its key or its name.
                assert_eq!(
                    catalog.entry(&made).await.unwrap_err().kind,
                    ErrorKind::NotFound
                );
                assert!(
                    catalog
                        .list(&[], 10, None, false)
                        .await
                        .unwrap()
                        .0
                        .is_empty()
                );
                assert!(
                    catalog
                        .entries_on(&["m".into()], &[])
                        .await
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    catalog
                        .runs(std::slice::from_ref(&made))
                        .await
                        .unwrap()
                        .is_empty()
                );
                assert!(catalog.keyed("k").await.unwrap().is_none());
                assert!(catalog.run_roots(&["n".into()]).await.unwrap().is_empty());
                // So the same key and name are free here.
                let (e, ..) = catalog
                    .add_entry("m", entry("k", "n", &[]))
                    .await
                    .unwrap()
                    .unwrap();
                assert_ne!(e, made);
            })
            .await;
            // Without a scope, everything is there.
            let all = catalog.list(&[], 10, None, false).await.unwrap().0;
            assert_eq!(all.len(), 2);
            assert!(catalog.entry(&made).await.is_ok());
        }
    }

    #[tokio::test]
    async fn a_run_is_named_by_its_root_and_keeps_the_name_while_it_has_entries() {
        for catalog in catalogs().await {
            let named = |name: &str| NewEntry {
                name: Some(name.into()),
                source: Some("import PisaRun.base".into()),
                ..no_labels()
            };
            let replacing = |entry: &str| NewEntry {
                replaces: Some(Replaces {
                    snapshot_id: entry.into(),
                    allow: None,
                }),
                replacement: Some(Replacement::default()),
                ..no_labels()
            };
            let (s, ..) = catalog
                .commit("s", &meta("s"), None, named("proj.lemma"))
                .await
                .unwrap();
            assert_eq!(
                catalog.entry(&s).await.unwrap().source.as_deref(),
                Some("import PisaRun.base")
            );
            let (p, _) = catalog
                .add_entry("s", replacing(&s))
                .await
                .unwrap()
                .unwrap();
            let names = ["proj.lemma".to_string(), "other".to_string()];
            assert_eq!(
                catalog.run_roots(&names).await.unwrap(),
                HashMap::from([("proj.lemma".to_string(), s.clone())])
            );
            assert_eq!(
                catalog.run_names_of(&[s.clone(), p.clone()]).await.unwrap(),
                HashMap::from([
                    (s.clone(), "proj.lemma".to_string()),
                    (p.clone(), "proj.lemma".to_string())
                ])
            );
            assert_eq!(
                catalog.run_named("proj.lemma").await.unwrap(),
                Some((s.clone(), true))
            );
            // Another run can't take it, nor can a replacement name its run.
            let taken = catalog
                .add_entry("s", named("proj.lemma"))
                .await
                .unwrap_err();
            assert_eq!(taken.kind, ErrorKind::Conflict, "{taken:?}");
            let renamed = NewEntry {
                name: Some("x".into()),
                ..replacing(&p)
            };
            let refused = catalog.add_entry("s", renamed).await.unwrap_err();
            assert_eq!(refused.kind, ErrorKind::BadRequest, "{refused:?}");
            // The root goes, and the run with its name stays.
            catalog.remove_entry(&s).await.unwrap();
            assert_eq!(
                catalog.run_named("proj.lemma").await.unwrap(),
                Some((s.clone(), true))
            );
            assert_eq!(
                catalog
                    .run_names_of(std::slice::from_ref(&p))
                    .await
                    .unwrap(),
                HashMap::from([(p.clone(), "proj.lemma".to_string())])
            );
            // A spec that amends the run's takes its name over.
            let amending = NewEntry {
                amends: Some(Amendment {
                    amends: p.clone(),
                    reason: "r".into(),
                    changes: Vec::new(),
                    added: Vec::new(),
                    holes: None,
                }),
                ..named("proj.lemma")
            };
            let (a, _) = catalog.add_entry("s", amending).await.unwrap().unwrap();
            assert_eq!(
                catalog.run_named("proj.lemma").await.unwrap(),
                Some((a.clone(), true))
            );
            assert!(
                catalog
                    .run_names_of(std::slice::from_ref(&p))
                    .await
                    .unwrap()
                    .is_empty()
            );
            // Once its run has no entry, the name goes.
            catalog.remove_entry(&a).await.unwrap();
            assert_eq!(catalog.run_named("proj.lemma").await.unwrap(), None);
            catalog.remove_entry(&p).await.unwrap();
            assert!(catalog.db.rows(&catalog.version).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn replacers_lists_every_entry_that_replaces_one() {
        for catalog in catalogs().await {
            let (a, _, _) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let replacing = |entry: &str| NewEntry {
                replaces: Some(Replaces {
                    snapshot_id: entry.into(),
                    allow: None,
                }),
                replacement: Some(Replacement {
                    filled: Vec::new(),
                    changed_definitions: Vec::new(),
                    affected: Vec::new(),
                }),
                fork: true,
                ..no_labels()
            };
            let mut replacers = Vec::new();
            for _ in 0..2 {
                let (e, _) = catalog
                    .add_entry("x", replacing(&a))
                    .await
                    .unwrap()
                    .unwrap();
                replacers.push(e);
            }
            let (c, _) = catalog.add_entry("x", no_labels()).await.unwrap().unwrap();

            let found = catalog.replacers(&a).await.unwrap();
            let found: Vec<&str> = found.iter().map(|(e, _)| e.as_str()).collect();
            assert_eq!(found, [replacers[0].as_str(), replacers[1].as_str()]);
            assert!(catalog.replacers(&c).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn replacers_among_groups_by_what_they_replace() {
        for catalog in catalogs().await {
            catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let a = added(&catalog, no_labels()).await;
            let b = added(&catalog, no_labels()).await;
            let c = added(&catalog, no_labels()).await;
            let fork = |old: &str| NewEntry {
                fork: true,
                ..step(old, &[], &[], &[])
            };
            let a1 = added(&catalog, fork(&a)).await;
            let a2 = added(&catalog, fork(&a)).await;
            let b1 = added(&catalog, fork(&b)).await;

            let found = catalog
                .replacers_among(&[a.clone(), b.clone(), c.clone()])
                .await
                .unwrap();
            let replacers =
                |e: &str| -> Vec<&str> { found[e].iter().map(|(r, _)| r.as_str()).collect() };
            assert_eq!(found.len(), 2);
            assert_eq!(replacers(&a), [a1.as_str(), a2.as_str()]);
            assert_eq!(replacers(&b), [b1.as_str()]);
            assert!(!found.contains_key(&c));
            for (_, meta) in &found[&a] {
                assert_eq!(meta.replaces.as_ref().unwrap().snapshot_id, a);
            }
            assert!(catalog.replacers_among(&[]).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn replacers_among_reads_past_one_chunk() {
        // One more than the 1000 ids a query takes.
        const ENTRIES: usize = 1001;
        for catalog in catalogs().await {
            catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            // Each entry replaces the one before it, so that all but the last
            // are replaced once.
            let mut chain = vec![added(&catalog, no_labels()).await];
            for _ in 0..ENTRIES {
                let next = step(chain.last().unwrap(), &[], &[], &[]);
                chain.push(added(&catalog, next).await);
            }

            let replaced = &chain[..ENTRIES];
            let found = catalog.replacers_among(replaced).await.unwrap();
            assert_eq!(found.len(), ENTRIES);
            for (old, new) in replaced.iter().zip(&chain[1..]) {
                let ids: Vec<&str> = found[old].iter().map(|(r, _)| r.as_str()).collect();
                assert_eq!(ids, [new.as_str()]);
            }
        }
    }

    #[tokio::test]
    async fn a_link_keeps_its_comparison() {
        for catalog in catalogs().await {
            let (old, _, _) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let replacement = Some(Replacement {
                filled: Vec::new(),
                changed_definitions: vec!["f".into()],
                affected: vec!["t".into()],
            });
            let entry = NewEntry {
                replaces: Some(Replaces {
                    snapshot_id: old.clone(),
                    allow: None,
                }),
                replacement: replacement.clone(),
                ..no_labels()
            };
            let (e, added) = catalog.add_entry("x", entry).await.unwrap().unwrap();
            assert_eq!(added.replacement, replacement);
            assert_eq!(catalog.entry(&e).await.unwrap().replacement, replacement);
            let (listed, _) = catalog.list(&[], 10, None, false).await.unwrap();
            let found = listed.iter().find(|(id, _)| *id == e).unwrap();
            assert_eq!(found.1.replacement, replacement);
            // An entry that replaces nothing has no comparison.
            assert_eq!(catalog.entry(&old).await.unwrap().replacement, None);
        }
    }

    #[test]
    fn a_splice_names_each_definition_once_by_its_normalised_form() {
        let entry = |replaces, replacement| EntryMeta {
            module: "x".into(),
            labels: BTreeMap::new(),
            key: None,
            created_micros: 0,
            replaces,
            replacement,
            holes: Vec::new(),
            source: None,
            runs: Vec::new(),
        };
        let root = entry(None, None);
        let top = entry(link("p1", &["f"]), Some(replacement(&[], &["f"], &[])));
        assert!(splice(&root, &top).is_none());

        let private = ["_private.PisaSnap.sa.0.h", "f"];
        let deleted = entry(
            link("s0", &private),
            Some(replacement(&["t", "u"], &private, &["t"])),
        );
        let top = entry(
            link("p1", &["_private.PisaSnap.sb.0.h", "g"]),
            Some(replacement(
                &["u", "v"],
                &["_private.PisaSnap.sb.0.h", "g", "f"],
                &["t", "u"],
            )),
        );
        let (replaces, spliced) = splice(&deleted, &top).unwrap();
        let changed = ["_private.PisaSnap.sa.0.h", "f", "g"];
        assert_eq!(Some(replaces), link("s0", &changed));
        assert_eq!(
            spliced,
            replacement(&["t", "u", "v"], &changed, &["t", "u"])
        );
    }

    #[tokio::test]
    async fn a_delete_splices_what_replaces_it() {
        for catalog in catalogs().await {
            let (s0, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let p1 = added(&catalog, step(&s0, &["t"], &["f"], &["t"])).await;
            let p2 = added(&catalog, step(&p1, &[], &["g"], &["t"])).await;

            catalog.remove_entry(&p1).await.unwrap();

            let spliced = catalog.entry(&p2).await.unwrap();
            assert_eq!(spliced.replaces, link(&s0, &["f", "g"]));
            assert_eq!(
                spliced.replacement,
                Some(replacement(&["t"], &["f", "g"], &["t"]))
            );
            assert_eq!(replacer_ids(&catalog, &s0).await, [p2]);
        }
    }

    #[tokio::test]
    async fn a_run_is_read_whole_from_any_of_its_entries() {
        for catalog in catalogs().await {
            let (s0, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let p1 = added(&catalog, step(&s0, &["t"], &[], &[])).await;
            let p2 = added(&catalog, step(&p1, &[], &[], &[])).await;
            let fork = NewEntry {
                fork: true,
                ..step(&p1, &[], &[], &[])
            };
            let p3 = added(&catalog, fork).await;
            let other = added(&catalog, no_labels()).await;
            let run = |es: Vec<String>| {
                let catalog = catalog.clone();
                async move {
                    let mut ids: Vec<String> =
                        catalog.runs(&es).await.unwrap().into_keys().collect();
                    ids.sort();
                    ids
                }
            };
            let mut whole = vec![s0.clone(), p1.clone(), p2.clone(), p3.clone()];
            whole.sort();
            for from in [&s0, &p1, &p3] {
                assert_eq!(run(vec![from.clone()]).await, whole);
            }
            let both = run(vec![p2.clone(), other.clone()]).await;
            assert_eq!(both.len(), 5);
            assert!(both.contains(&other));

            // A splice keeps the run; so does deleting its root, which goes.
            catalog.remove_entry(&p1).await.unwrap();
            let mut spliced = vec![s0.clone(), p2.clone(), p3.clone()];
            spliced.sort();
            assert_eq!(run(vec![p2.clone()]).await, spliced);
            catalog.remove_entry(&s0).await.unwrap();
            let mut rootless = vec![p2.clone(), p3.clone()];
            rootless.sort();
            assert_eq!(run(vec![p3.clone()]).await, rootless);
            let found = catalog.runs(std::slice::from_ref(&p3)).await.unwrap();
            assert_eq!(found[&p3].replaces.as_ref().unwrap().snapshot_id, s0);
            // A branch delete takes its entries out of the run.
            catalog.remove_branch(&p2).await.unwrap();
            assert_eq!(run(vec![p3.clone()]).await, std::slice::from_ref(&p3));
        }
    }

    /// The order a create once stored a closure in: each import's closure in
    /// the header's order, each module once, then the module's own.
    fn stored_order(m: &str, imports: &HashMap<String, Vec<String>>) -> Vec<String> {
        let mut order: Vec<String> = Vec::new();
        for parent in imports.get(m).into_iter().flatten() {
            for x in stored_order(parent, imports) {
                if !order.contains(&x) {
                    order.push(x);
                }
            }
        }
        order.push(m.to_string());
        order
    }

    #[test]
    fn a_closure_is_ordered_as_creates_stored_it() {
        // Small pseudo-random DAGs: each module imports some earlier ones.
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n.max(1)
        };
        for _ in 0..200 {
            let n = 2 + next(12) as usize;
            let mut imports: HashMap<String, Vec<String>> = HashMap::new();
            for i in 1..n {
                let mut parents = Vec::new();
                for _ in 0..next(4) {
                    let p = format!("m{}", next(i as u64));
                    if !parents.contains(&p) {
                        parents.push(p);
                    }
                }
                imports.insert(format!("m{i}"), parents);
            }
            let top = format!("m{}", n - 1);
            assert_eq!(closure_order(&top, &imports), stored_order(&top, &imports));
        }
    }

    /// A module's record reads back as it was written, its closure from what
    /// its imports import, and a listing's summaries from its columns.
    #[tokio::test]
    async fn a_record_reads_back_with_its_closure() {
        for catalog in catalogs().await {
            let child = |id: &str, imports: &[&str], modules: Vec<Module>| ModuleMeta {
                imports: imports.iter().map(|s| s.to_string()).collect(),
                modules,
                ..meta(id)
            };
            let own = |id: &str| meta(id).modules.pop().unwrap();
            catalog
                .commit("a", &meta("a"), None, no_labels())
                .await
                .unwrap();
            catalog
                .commit("b", &meta("b"), None, no_labels())
                .await
                .unwrap();
            let c = child("c", &["b", "a"], vec![own("b"), own("a"), own("c")]);
            catalog.commit("c", &c, None, no_labels()).await.unwrap();
            let d = child(
                "d",
                &["a", "c"],
                vec![own("a"), own("b"), own("c"), own("d")],
            );
            catalog.commit("d", &d, None, no_labels()).await.unwrap();

            let read = catalog.module("d").await.unwrap();
            let ids: Vec<&str> = read.modules.iter().map(|m| m.id.as_str()).collect();
            assert_eq!(ids, ["a", "b", "c", "d"]);
            assert_eq!(read.imports, ["a", "c"]);
            assert_eq!((read.header, read.status), (d.header.clone(), d.status));
            let summaries = catalog
                .summaries(&["c".to_string(), "d".to_string(), "gone".to_string()])
                .await
                .unwrap();
            assert_eq!(summaries.len(), 2);
            assert_eq!(summaries["c"].imports, ["b", "a"]);
            assert_eq!(summaries["d"], d.summary());

            // Many records at once, and what they build on from one graph.
            let wanted = ["c".to_string(), "d".to_string(), "gone".to_string()];
            let records = catalog.records(&wanted).await.unwrap();
            assert_eq!(records.len(), 2);
            assert_eq!(records["d"].imports, ["a", "c"]);
            assert_eq!(
                (
                    &records["d"].header,
                    &records["d"].code,
                    records["d"].status
                ),
                (&d.header, &d.code, d.status)
            );
            let graph = catalog.import_graph(&wanted).await.unwrap();
            assert_eq!(closure_order("d", &graph), ["a", "b", "c", "d"]);
            assert_eq!(closure_order("c", &graph), ["b", "a", "c"]);
            assert!(catalog.import_graph(&[]).await.unwrap().is_empty());
        }
    }

    #[test]
    fn a_splice_allows_what_either_link_allowed() {
        let entry = |replaces, replacement| EntryMeta {
            module: "x".into(),
            labels: BTreeMap::new(),
            key: None,
            created_micros: 0,
            replaces,
            replacement,
            holes: Vec::new(),
            source: None,
            runs: Vec::new(),
        };
        // The comparisons are what the creates' builds reported; the allows
        // are what the links' authors declared, which an audit reads.
        let deleted = entry(link("s0", &["f", "k"]), Some(replacement(&[], &["f"], &[])));
        let top = entry(link("p1", &["g"]), Some(replacement(&[], &[], &[])));
        let (replaces, spliced) = splice(&deleted, &top).unwrap();
        assert_eq!(Some(replaces), link("s0", &["f", "k", "g"]));
        assert_eq!(spliced, replacement(&[], &["f"], &[]));
    }

    #[tokio::test]
    async fn a_spliced_link_without_changes_has_no_allow() {
        for catalog in catalogs().await {
            let (s0, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let p1 = added(&catalog, step(&s0, &["t"], &[], &[])).await;
            let p2 = added(&catalog, step(&p1, &["u"], &[], &[])).await;

            catalog.remove_entry(&p1).await.unwrap();

            let spliced = catalog.entry(&p2).await.unwrap();
            assert_eq!(spliced.replaces, link(&s0, &[]));
            assert_eq!(
                spliced.replacement,
                Some(replacement(&["t", "u"], &[], &[]))
            );
        }
    }

    #[tokio::test]
    async fn a_deleted_root_leaves_its_replacers_link() {
        for catalog in catalogs().await {
            let (s0, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let p1 = added(&catalog, step(&s0, &["t"], &["f"], &["t"])).await;

            catalog.remove_entry(&s0).await.unwrap();

            let kept = catalog.entry(&p1).await.unwrap();
            assert_eq!(kept.replaces, link(&s0, &["f"]));
            assert_eq!(kept.replacement, Some(replacement(&["t"], &["f"], &["t"])));
        }
    }

    #[tokio::test]
    async fn a_fork_moves_down() {
        for catalog in catalogs().await {
            let (s0, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let p1 = added(&catalog, step(&s0, &["t"], &[], &[])).await;
            let a = added(&catalog, step(&p1, &["u"], &[], &[])).await;
            let forking = NewEntry {
                fork: true,
                ..step(&p1, &["v"], &[], &[])
            };
            let b = added(&catalog, forking).await;

            catalog.remove_entry(&p1).await.unwrap();

            assert_eq!(replacer_ids(&catalog, &s0).await, [a.clone(), b.clone()]);
            let (a, b) = (
                catalog.entry(&a).await.unwrap(),
                catalog.entry(&b).await.unwrap(),
            );
            assert_eq!(a.replacement, Some(replacement(&["t", "u"], &[], &[])));
            assert_eq!(b.replacement, Some(replacement(&["t", "v"], &[], &[])));
        }
    }

    #[tokio::test]
    async fn a_branch_delete_takes_what_replaces_it() {
        for catalog in catalogs().await {
            let (s0, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let forking = |old: &str| NewEntry {
                fork: true,
                ..step(old, &["v"], &[], &[])
            };
            let a1 = added(&catalog, step(&s0, &["t"], &[], &[])).await;
            let a2 = added(&catalog, step(&a1, &["u"], &[], &[])).await;
            let a3 = added(&catalog, forking(&a1)).await;
            let b1 = added(&catalog, forking(&s0)).await;

            let (deleted, removed) = catalog.remove_branch(&a1).await.unwrap();

            assert_eq!(deleted[0], a1);
            let mut rest = deleted[1..].to_vec();
            rest.sort();
            let mut both = [a2.clone(), a3.clone()];
            both.sort();
            assert_eq!(rest, both);
            // `x` still holds the spec and B1.
            assert!(removed.is_empty());
            assert_eq!(replacer_ids(&catalog, &s0).await, std::slice::from_ref(&b1));
            for gone in &deleted {
                assert!(catalog.entry(gone).await.is_err());
            }
            // Nothing was spliced: B1 keeps its link.
            let kept = catalog.entry(&b1).await.unwrap();
            assert_eq!(kept.replaces, link(&s0, &[]));
            assert_eq!(kept.replacement, Some(replacement(&["v"], &[], &[])));
            // A create racing it that commits later finds what it replaces gone.
            let late = catalog.add_entry("x", step(&a2, &[], &[], &[])).await;
            assert_eq!(late.unwrap_err().kind, ErrorKind::NotFound);
        }
    }

    #[tokio::test]
    async fn a_branch_that_was_the_only_replacer_leaves_a_head() {
        for catalog in catalogs().await {
            let (s0, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let a1 = added(&catalog, step(&s0, &["t"], &[], &[])).await;

            let (deleted, _) = catalog.remove_branch(&a1).await.unwrap();

            assert_eq!(deleted, [a1]);
            assert!(replacer_ids(&catalog, &s0).await.is_empty());
            // S0 is a head again, so a create may replace it without forking.
            added(&catalog, step(&s0, &["t"], &[], &[])).await;
        }
    }

    #[tokio::test]
    async fn a_branch_delete_cleans_the_modules_it_frees() {
        for catalog in catalogs().await {
            let (s0, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            // Each module imports the one before, so `a` can go only after
            // `b`, and `b` after `c`.
            let a = meta("a");
            let b = child("b", &a);
            let c = child("c", &b);
            let mut deleted = Vec::new();
            let mut replaced = s0.clone();
            for (m, module) in [("a", &a), ("b", &b), ("c", &c)] {
                let step = step(&replaced, &[], &[], &[]);
                replaced = catalog.commit(m, module, None, step).await.unwrap().0;
                deleted.push(replaced.clone());
            }
            let (kept, ..) = catalog
                .commit("z", &meta("z"), None, no_labels())
                .await
                .unwrap();

            let (found, removed) = catalog.remove_branch(&deleted[0]).await.unwrap();

            assert_eq!(found, deleted);
            let modules: Vec<&str> = removed.iter().map(|(m, _)| m.as_str()).collect();
            assert_eq!(modules, ["c", "b", "a"]);
            assert_eq!(removed[0].1.imports, ["b"]);
            for m in ["a", "b", "c"] {
                assert!(catalog.module(m).await.is_err(), "{m}");
            }
            assert!(catalog.module("x").await.is_ok());
            assert!(catalog.entry(&kept).await.is_ok());
        }
    }

    #[tokio::test]
    async fn a_splice_over_a_deleted_predecessor_keeps_its_link() {
        for catalog in catalogs().await {
            let (s0, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let p1 = added(&catalog, step(&s0, &["t"], &["f"], &["t"])).await;
            let p2 = added(&catalog, step(&p1, &[], &["g"], &["t"])).await;

            catalog.remove_entry(&s0).await.unwrap();
            catalog.remove_entry(&p1).await.unwrap();

            let spliced = catalog.entry(&p2).await.unwrap();
            assert_eq!(spliced.replaces, link(&s0, &["f", "g"]));
            assert_eq!(
                spliced.replacement,
                Some(replacement(&["t"], &["f", "g"], &["t"]))
            );
        }
    }

    #[tokio::test]
    async fn a_create_replacing_a_deleted_snapshot_is_not_found() {
        for catalog in catalogs().await {
            let (old, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let (kept, _) = catalog.add_entry("x", keyed("k")).await.unwrap().unwrap();
            catalog.remove_entry(&old).await.unwrap();
            let gone = "the replaced snapshot was deleted while this one was being built";
            let creating = |key: Option<&str>| NewEntry {
                key: key.map(String::from),
                ..step(&old, &[], &[], &[])
            };

            let err = catalog
                .commit("y", &meta("y"), None, creating(None))
                .await
                .unwrap_err();
            assert_eq!(
                (err.kind, err.message.as_str()),
                (ErrorKind::NotFound, gone)
            );
            assert!(catalog.module("y").await.is_err());
            let err = catalog.add_entry("x", creating(None)).await.unwrap_err();
            assert_eq!(
                (err.kind, err.message.as_str()),
                (ErrorKind::NotFound, gone)
            );
            assert_eq!(catalog.list(&[], 10, None, false).await.unwrap().0.len(), 1);

            // A key replay comes first.
            let (e, ..) = catalog
                .commit("y", &meta("y"), None, creating(Some("k")))
                .await
                .unwrap();
            assert_eq!(e, kept);
            let (e, _) = catalog
                .add_entry("x", creating(Some("k")))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(e, kept);
        }
    }

    #[tokio::test]
    async fn a_create_replacing_a_replaced_snapshot_is_a_conflict_unless_it_forks() {
        for catalog in catalogs().await {
            let (old, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let first = added(&catalog, step(&old, &[], &[], &[])).await;
            let replacing = |key: Option<&str>, fork: bool| NewEntry {
                key: key.map(String::from),
                fork,
                ..step(&old, &[], &[], &[])
            };

            let err = catalog
                .commit("y", &meta("y"), None, replacing(None, false))
                .await
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::Conflict);
            assert!(catalog.module("y").await.is_err());
            let err = catalog
                .add_entry("x", replacing(None, false))
                .await
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::Conflict);
            assert_eq!(
                replacer_ids(&catalog, &old).await,
                std::slice::from_ref(&first)
            );

            // A fork goes through, and is not stored.
            let (second, ..) = catalog
                .commit("y", &meta("y"), None, replacing(Some("k"), true))
                .await
                .unwrap();
            let (third, _) = catalog
                .add_entry("x", replacing(None, true))
                .await
                .unwrap()
                .unwrap();
            let mut found = replacer_ids(&catalog, &old).await;
            found.sort();
            let mut all = [first, second.clone(), third];
            all.sort();
            assert_eq!(found, all);

            // A key replay comes first.
            let (e, ..) = catalog
                .commit("y", &meta("y"), None, replacing(Some("k"), false))
                .await
                .unwrap();
            assert_eq!(e, second);
            let (e, _) = catalog
                .add_entry("x", replacing(Some("k"), false))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(e, second);

            // Deleting the replacers makes it a head again.
            for e in all {
                catalog.remove_entry(&e).await.unwrap();
            }
            catalog
                .add_entry("x", replacing(None, false))
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn dependents_are_every_module_above_with_their_imports() {
        for catalog in catalogs().await {
            // b under c and d, both under e; x on its own.
            let b = meta("b");
            let (c, d) = (child("c", &b), child("d", &b));
            let mut e = child("e", &c);
            e.imports.push("d".into());
            e.modules.insert(2, d.modules[1].clone());
            let p1 = || labelled(&[("p", "1")]);
            for (id, m, entry) in [
                ("b", &b, no_labels()),
                ("c", &c, p1()),
                ("d", &d, labelled(&[("p", "2")])),
                ("e", &e, p1()),
                ("x", &meta("x"), p1()),
            ] {
                catalog.commit(id, m, None, entry).await.unwrap();
            }
            let above = catalog.dependents("b").await.unwrap();
            let edges: Vec<(&str, Vec<&str>)> = above
                .iter()
                .map(|(m, i)| {
                    let mut i: Vec<&str> = i.iter().map(String::as_str).collect();
                    i.sort();
                    (m.as_str(), i)
                })
                .collect();
            assert_eq!(
                edges,
                [("c", vec!["b"]), ("d", vec!["b"]), ("e", vec!["c", "d"])]
            );
            assert!(catalog.dependents("e").await.unwrap().is_empty());

            let modules: Vec<String> = above.keys().cloned().collect();
            let on = |labels: Vec<(String, String)>| {
                let (catalog, modules) = (catalog.clone(), modules.clone());
                async move {
                    let found = catalog.entries_on(&modules, &labels).await.unwrap();
                    found.into_iter().map(|(_, m)| m.module).collect::<Vec<_>>()
                }
            };
            assert_eq!(on(vec![]).await, ["c", "d", "e"]);
            let p = |v: &str| ("p".to_string(), v.to_string());
            assert_eq!(on(vec![p("1")]).await, ["c", "e"]);
            assert!(on(vec![p("1"), p("2")]).await.is_empty());
        }
    }

    #[tokio::test]
    async fn changes_page_in_order_and_prune() {
        for catalog in catalogs().await {
            for id in ["x", "y", "z"] {
                catalog
                    .commit(id, &meta(id), None, no_labels())
                    .await
                    .unwrap();
            }
            let modules = |page: &[Logged]| -> Vec<String> {
                page.iter()
                    .filter_map(|l| l.change.module.clone())
                    .collect()
            };
            let first = catalog.changes(0, None, 2).await.unwrap();
            assert_eq!(modules(&first), ["x", "y"]);
            let after = (first[1].logged_micros, first[1].id);
            let rest = catalog.changes(0, Some(after), 2).await.unwrap();
            assert_eq!(modules(&rest), ["z"]);
            // Nothing is logged in the future.
            assert!(
                catalog
                    .changes(now_micros() + 1_000_000, None, 10)
                    .await
                    .unwrap()
                    .is_empty()
            );

            assert_eq!(
                catalog.prune_log(Duration::from_secs(3600)).await.unwrap(),
                0
            );
            assert_eq!(catalog.prune_log(Duration::ZERO).await.unwrap(), 3);
            assert!(catalog.changes(0, None, 10).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn a_refused_sweep_is_kept_until_one_sweeps() {
        for catalog in catalogs().await {
            catalog.identity().await.unwrap();
            assert_eq!(catalog.sweep_refused().await.unwrap(), None);
            catalog.record_sweep(Some("no marker")).await.unwrap();
            let (why, since) = catalog.sweep_refused().await.unwrap().unwrap();
            assert_eq!(why, "no marker");
            // Since the first of the refusals in a row.
            catalog.record_sweep(Some("another's")).await.unwrap();
            assert_eq!(
                catalog.sweep_refused().await.unwrap(),
                Some(("another's".to_string(), since))
            );
            catalog.record_sweep(None).await.unwrap();
            assert_eq!(catalog.sweep_refused().await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn a_lease_is_held_by_one_owner_until_it_ends() {
        for catalog in catalogs().await {
            let ttl = Duration::from_secs(30);
            assert!(catalog.take_lease("l", "a", ttl).await.unwrap().is_some());
            assert!(catalog.take_lease("l", "b", ttl).await.unwrap().is_none());
            // Renewing is taking it again.
            assert!(catalog.take_lease("l", "a", ttl).await.unwrap().is_some());
            // An ended lease goes to whoever asks.
            catalog.take_lease("l", "a", Duration::ZERO).await.unwrap();
            assert!(catalog.take_lease("l", "b", ttl).await.unwrap().is_some());
        }
    }

    /// An amendment is stored with the entry that makes it, which needs the
    /// amended entry still there; it goes with that entry, and outlives the
    /// amended one.
    #[tokio::test]
    async fn an_amendment_goes_with_the_spec_that_makes_it() {
        use pisa_protocol::verify::Change;
        for catalog in catalogs().await {
            let (old, _, _) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let amendment = Amendment {
                amends: old.clone(),
                reason: "t was false".into(),
                changes: vec![DeclarationChange::new("t", Change::Type)],
                added: vec!["u".into()],
                holes: Some(AmendedHoles {
                    before: vec!["cost".into()],
                    after: Vec::new(),
                }),
            };
            let amending = |a: &Amendment| NewEntry {
                amends: Some(a.clone()),
                ..no_labels()
            };
            let (new, _) = catalog
                .add_entry("x", amending(&amendment))
                .await
                .unwrap()
                .unwrap();
            let made = vec![(new.clone(), amendment.clone())];
            assert_eq!(
                catalog
                    .amendments_of(std::slice::from_ref(&old))
                    .await
                    .unwrap(),
                made
            );
            assert_eq!(
                catalog
                    .amendments_of(std::slice::from_ref(&new))
                    .await
                    .unwrap(),
                made
            );
            let gone = Amendment {
                amends: "0".repeat(32),
                ..amendment.clone()
            };
            let e = catalog.add_entry("x", amending(&gone)).await.unwrap_err();
            assert_eq!(e.kind, ErrorKind::NotFound);
            catalog.remove_entry(&old).await.unwrap();
            assert_eq!(
                catalog
                    .amendments_of(std::slice::from_ref(&new))
                    .await
                    .unwrap(),
                made
            );
            catalog.remove_entry(&new).await.unwrap();
            assert!(catalog.amendments_of(&[new, old]).await.unwrap().is_empty());
        }
    }

    /// An entry can't join a run whose spec is amended, forked or not: the
    /// check in the transaction catches an amendment made while it built.
    #[tokio::test]
    async fn an_entry_cant_join_a_run_whose_spec_is_amended() {
        for catalog in catalogs().await {
            let (spec, _, _) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let replacing = |entry: &str| NewEntry {
                replaces: Some(Replaces {
                    snapshot_id: entry.into(),
                    allow: None,
                }),
                replacement: Some(Replacement {
                    filled: Vec::new(),
                    changed_definitions: Vec::new(),
                    affected: Vec::new(),
                }),
                fork: true,
                ..no_labels()
            };
            let (head, _) = catalog
                .add_entry("x", replacing(&spec))
                .await
                .unwrap()
                .unwrap();
            let amendment = Amendment {
                amends: spec.clone(),
                reason: "t was false".into(),
                changes: Vec::new(),
                added: Vec::new(),
                holes: None,
            };
            let amending = NewEntry {
                amends: Some(amendment),
                ..no_labels()
            };
            let (new, _) = catalog.add_entry("x", amending).await.unwrap().unwrap();
            for replaced in [&spec, &head] {
                let e = catalog
                    .add_entry("x", replacing(replaced))
                    .await
                    .unwrap_err();
                assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}");
            }
            // The new spec's run takes it.
            catalog
                .add_entry("x", replacing(&new))
                .await
                .unwrap()
                .unwrap();
        }
    }

    /// A refutation is stored with the entry that makes it, which needs the
    /// spec still there; it outlives both, as the record that the target is
    /// false.
    #[tokio::test]
    async fn a_refutation_outlives_the_snapshot_that_makes_it() {
        for catalog in catalogs().await {
            let (spec, _, _) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let refutation = Refutation {
                spec: spec.clone(),
                target: "t".into(),
                theorem: "r".into(),
                target_hash: Some("h".into()),
            };
            let refuting = |r: &Refutation| NewEntry {
                refutes: Some(r.clone()),
                ..no_labels()
            };
            let (by, _) = catalog
                .add_entry("x", refuting(&refutation))
                .await
                .unwrap()
                .unwrap();
            let made = vec![(by.clone(), refutation.clone())];
            assert_eq!(
                catalog
                    .refutations_of(std::slice::from_ref(&spec))
                    .await
                    .unwrap(),
                made
            );
            assert_eq!(
                catalog
                    .refutations_of(std::slice::from_ref(&by))
                    .await
                    .unwrap(),
                made
            );
            let gone = Refutation {
                spec: "0".repeat(32),
                ..refutation.clone()
            };
            let e = catalog.add_entry("x", refuting(&gone)).await.unwrap_err();
            assert_eq!(e.kind, ErrorKind::NotFound);
            catalog.remove_entry(&spec).await.unwrap();
            assert_eq!(
                catalog
                    .refutations_of(std::slice::from_ref(&by))
                    .await
                    .unwrap(),
                made
            );
            catalog.remove_entry(&by).await.unwrap();
            assert_eq!(catalog.refutations_of(&[by, spec]).await.unwrap(), made);
        }
    }

    /// An acceptance's result reads back as it was stored, under its id.
    #[tokio::test]
    async fn an_acceptance_reads_back() {
        for catalog in catalogs().await {
            let result =
                serde_json::json!({"accepted": true, "pairs": [{"spec": "s", "head": "h"}]});
            let specs = ["s".to_string(), "t".into()];
            catalog.put_acceptance("a1", &specs, &result).await.unwrap();
            assert_eq!(
                catalog.acceptance("a1").await.unwrap(),
                Some(result.clone())
            );
            assert_eq!(catalog.acceptance("a2").await.unwrap(), None);
            // By spec, newest first.
            let later = serde_json::json!({"accepted": false});
            catalog
                .put_acceptance("a2", &specs[..1], &later)
                .await
                .unwrap();
            assert_eq!(
                catalog.acceptances_of("s", 10).await.unwrap(),
                [later, result.clone()]
            );
            assert_eq!(catalog.acceptances_of("t", 10).await.unwrap(), [result]);
            assert_eq!(catalog.acceptances_of("s", 1).await.unwrap().len(), 1);
            assert!(catalog.acceptances_of("u", 10).await.unwrap().is_empty());
        }
    }

    /// A version's catalog keeps the id it was first given, and another
    /// version's has its own.
    #[tokio::test]
    async fn a_catalog_keeps_one_id() {
        for catalog in catalogs().await {
            let id = catalog.identity().await.unwrap();
            assert_eq!(id.len(), 32);
            assert_eq!(catalog.identity().await.unwrap(), id);
            let other = Catalog::new(catalog.db.clone(), format!("{}-other", catalog.version));
            assert_ne!(other.identity().await.unwrap(), id);
        }
    }

    #[tokio::test]
    async fn whether_a_module_has_entries() {
        for catalog in catalogs().await {
            let (e, _, _) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            assert!(catalog.has_entries("x").await.unwrap());
            assert!(!catalog.has_entries("y").await.unwrap());
            catalog.add_entry("x", no_labels()).await.unwrap();
            catalog.remove_entry(&e).await.unwrap();
            assert!(catalog.has_entries("x").await.unwrap());
        }
    }

    #[tokio::test]
    async fn an_added_entry_logs_the_entry_it_replaces() {
        for catalog in catalogs().await {
            let (s, _, _) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let p = added(&catalog, step(&s, &[], &[], &[])).await;

            let rows: Vec<_> = catalog
                .changes(0, None, 1000)
                .await
                .unwrap()
                .into_iter()
                .map(|l| (l.change.entry, l.change.module))
                .collect();
            let row = |e: &str| (Some(e.to_string()), Some("x".to_string()));
            let own = rows.iter().position(|r| *r == row(&p)).unwrap();
            let replaced = rows.iter().rposition(|r| *r == row(&s)).unwrap();
            assert!(replaced > own, "{rows:?}");
        }
    }

    #[tokio::test]
    async fn which_entries_are_heads() {
        for catalog in catalogs().await {
            let (s, _, _) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            assert!(catalog.has_heads("x").await.unwrap());
            let (p, _, _) = catalog
                .commit("y", &meta("y"), None, step(&s, &[], &[], &[]))
                .await
                .unwrap();
            assert!(!catalog.has_heads("x").await.unwrap());
            assert!(catalog.has_heads("y").await.unwrap());
            assert!(!catalog.has_heads("z").await.unwrap());
            let both = [s.clone(), p.clone()];
            let replaced = catalog.replaced_among(&both).await.unwrap();
            assert_eq!(replaced, HashSet::from([s.clone()]));
            assert!(catalog.replaced_among(&[]).await.unwrap().is_empty());

            catalog.remove_entry(&p).await.unwrap();
            assert!(catalog.has_heads("x").await.unwrap());
            assert!(catalog.replaced_among(&both).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn modules_and_entries_page_in_id_order() {
        for catalog in catalogs().await {
            let mut entries = Vec::new();
            for id in ["x", "y", "z"] {
                let m = meta(id);
                let decls = SnapshotDecls::new(&m, vec![decl("a")]);
                let (e, ..) = catalog
                    .commit(id, &m, Some(&decls), no_labels())
                    .await
                    .unwrap();
                entries.push(e);
            }
            catalog
                .commit("w", &meta("w"), None, no_labels())
                .await
                .unwrap();
            let first = catalog.modules_with_decls_after(None, 2).await.unwrap();
            assert_eq!(first, ["x", "y"]);
            let rest = catalog
                .modules_with_decls_after(Some("y"), 2)
                .await
                .unwrap();
            assert_eq!(rest, ["z"]);

            entries.sort();
            let first = catalog.entries_after(None, 3).await.unwrap();
            let rest = catalog.entries_after(Some(&first[2].0), 3).await.unwrap();
            let ids: Vec<&String> = first.iter().chain(&rest).map(|(e, _)| e).collect();
            assert_eq!(ids.len(), 4);
            assert!(ids.is_sorted());
        }
    }

    #[tokio::test]
    async fn listings_filter_by_label_and_page_newest_first() {
        for catalog in catalogs().await {
            catalog
                .commit("x", &meta("x"), None, labelled(&[("p", "x"), ("k", "a")]))
                .await
                .unwrap();
            let mut made = Vec::new();
            for labels in [&[("p", "x"), ("k", "a")][..], &[("p", "x")], &[("p", "y")]] {
                let (e, _) = catalog
                    .add_entry("x", labelled(labels))
                    .await
                    .unwrap()
                    .unwrap();
                made.push(e);
            }
            let ids = |listed: &[(String, EntryMeta)]| -> Vec<String> {
                listed.iter().map(|(e, _)| e.clone()).collect()
            };
            let pair = |k: &str, v: &str| (k.to_string(), v.to_string());

            let (all, next) = catalog.list(&[], 10, None, false).await.unwrap();
            assert_eq!(all.len(), 4);
            assert!(next.is_none());
            assert_eq!(
                ids(&all[..3]),
                [made[2].as_str(), made[1].as_str(), made[0].as_str()]
            );

            let (p, _) = catalog
                .list(&[pair("p", "x")], 10, None, false)
                .await
                .unwrap();
            assert_eq!(p.len(), 3);
            let both = [pair("k", "a"), pair("p", "x")];
            let (first, next) = catalog.list(&both, 1, None, false).await.unwrap();
            assert_eq!(ids(&first), [made[0].as_str()]);
            let (rest, last) = catalog
                .list(&both, 1, next.as_deref(), false)
                .await
                .unwrap();
            assert_eq!(rest.len(), 1);
            assert_ne!(rest[0].0, made[0]);
            let (none, end) = catalog
                .list(&both, 1, last.as_deref(), false)
                .await
                .unwrap();
            assert!(none.is_empty() && end.is_none());

            // Two values for a key match nothing; the same pair twice is one.
            let (none, _) = catalog
                .list(&[pair("k", "a"), pair("k", "b")], 10, None, false)
                .await
                .unwrap();
            assert!(none.is_empty());
            let (twice, _) = catalog
                .list(&[pair("p", "y"), pair("p", "y")], 10, None, false)
                .await
                .unwrap();
            assert_eq!(ids(&twice), [made[2].as_str()]);

            let patch = BTreeMap::from([("p".to_string(), Some("z".to_string()))]);
            catalog.set_labels(&made[2], &patch).await.unwrap();
            let (moved, _) = catalog
                .list(&[pair("p", "z")], 10, None, false)
                .await
                .unwrap();
            assert_eq!(ids(&moved), [made[2].as_str()]);
            assert!(
                catalog
                    .list(&[pair("p", "y")], 10, None, false)
                    .await
                    .unwrap()
                    .0
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn records_with_nul_are_refused() {
        let mut m = meta("x");
        m.code = "a\0b".into();
        assert!(!record_fits(&m));
        for catalog in catalogs().await {
            let err = catalog
                .commit("x", &m, None, no_labels())
                .await
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::BadRequest, "{err:?}");
            assert!(catalog.module("x").await.is_err());
        }
    }

    #[tokio::test]
    async fn an_imported_module_stays_until_its_importers_go() {
        for catalog in catalogs().await {
            let parent = meta("p");
            let (pe, ..) = catalog
                .commit("p", &parent, None, no_labels())
                .await
                .unwrap();
            let (ce, ..) = catalog
                .commit("c", &child("c", &parent), None, no_labels())
                .await
                .unwrap();
            let (_, removed) = catalog.remove_entry(&pe).await.unwrap();
            assert!(removed.is_none());
            assert!(catalog.module("p").await.is_ok());
            let (_, removed) = catalog.remove_entry(&ce).await.unwrap();
            assert_eq!(removed.unwrap().imports, ["p"]);
            assert!(catalog.cleanup("p").await.unwrap().is_some());
            assert!(catalog.db.rows(&catalog.version).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn orphans_are_the_modules_nothing_holds() {
        for catalog in catalogs().await {
            let parent = meta("p");
            let (pe, ..) = catalog
                .commit("p", &parent, None, no_labels())
                .await
                .unwrap();
            let (ce, ..) = catalog
                .commit("c", &child("c", &parent), None, no_labels())
                .await
                .unwrap();
            catalog
                .commit("k", &meta("k"), None, no_labels())
                .await
                .unwrap();
            assert_eq!(
                catalog.orphans_after(None, 10, 10).await.unwrap().0,
                Vec::<String>::new()
            );
            // P's entry goes, but C imports it.
            catalog.remove_entry(&pe).await.unwrap();
            assert_eq!(
                catalog.orphans_after(None, 10, 10).await.unwrap().0,
                Vec::<String>::new()
            );
            // C's entry goes with its module, and a delete that stopped there
            // leaves P to nothing.
            catalog.remove_entry(&ce).await.unwrap();
            assert_eq!(catalog.orphans_after(None, 10, 10).await.unwrap().0, ["p"]);
            // A page of one module says where to go on.
            assert_eq!(
                catalog.orphans_after(None, 1, 10).await.unwrap(),
                (vec![], Some("k".into()))
            );
            assert_eq!(
                catalog.orphans_after(Some("p"), 10, 10).await.unwrap().0,
                Vec::<String>::new()
            );
            // A module kept with no entry isn't one until it is older than
            // the cutoff.
            let kept = ModuleMeta {
                created: 20,
                ..meta("f")
            };
            let stored = catalog.commit_module("f", &kept, None).await.unwrap();
            assert_eq!(stored.created, 20);
            assert!((catalog.entries_on(&["f".into()], &[]).await.unwrap()).is_empty());
            assert!(catalog.module("f").await.is_ok());
            assert_eq!(catalog.orphans_after(None, 10, 10).await.unwrap().0, ["p"]);
            assert_eq!(
                catalog.orphans_after(None, 10, 20).await.unwrap().0,
                ["f", "p"]
            );
        }
    }

    #[tokio::test]
    async fn own_builds_are_read_for_many_modules_at_once() {
        for catalog in catalogs().await {
            let parent = meta("p");
            let kid = child("c", &parent);
            catalog
                .commit("p", &parent, None, no_labels())
                .await
                .unwrap();
            catalog.commit("c", &kid, None, no_labels()).await.unwrap();
            let own = |m: &ModuleMeta| m.modules.last().unwrap().build.clone();
            let ids = ["p", "c", "gone"].map(String::from);
            assert_eq!(
                catalog.own_builds(&ids).await.unwrap(),
                HashMap::from([("p".into(), own(&parent)), ("c".into(), own(&kid))])
            );
        }
    }

    /// Runs `f` on `n` tasks at once.
    async fn racing<T: Send + 'static, F: Future<Output = T> + Send + 'static>(
        n: usize,
        f: impl Fn(usize) -> F,
    ) -> Vec<T> {
        let tasks: Vec<_> = (0..n).map(|i| tokio::spawn(f(i))).collect();
        let mut out = Vec::new();
        for task in tasks {
            out.push(task.await.unwrap());
        }
        out
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_creates_with_one_key_make_one_entry() {
        for catalog in catalogs().await {
            let made = racing(8, |_| {
                let catalog = catalog.clone();
                async move { catalog.commit("x", &meta("x"), None, keyed("k")).await }
            })
            .await;
            let ids: Vec<String> = made.into_iter().map(|r| r.unwrap().0).collect();
            assert!(ids.iter().all(|e| *e == ids[0]), "{ids:?}");

            let made = racing(8, |i| {
                let catalog = catalog.clone();
                async move {
                    catalog
                        .commit("x", &meta("x"), None, keyed(&format!("k{i}")))
                        .await
                }
            })
            .await;
            let mut ids: Vec<String> = made.into_iter().map(|r| r.unwrap().0).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), 8);
            assert_eq!(
                catalog.list(&[], 100, None, false).await.unwrap().0.len(),
                9
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn creates_racing_to_replace_one_head_make_one_replacer() {
        for catalog in catalogs().await {
            let (old, ..) = catalog
                .commit("x", &meta("x"), None, no_labels())
                .await
                .unwrap();
            let made = racing(8, |_| {
                let (catalog, old) = (catalog.clone(), old.clone());
                async move { catalog.add_entry("x", step(&old, &[], &[], &[])).await }
            })
            .await;
            let refused: Vec<ErrorKind> = made
                .iter()
                .filter_map(|r| r.as_ref().err().map(|e| e.kind))
                .collect();
            assert_eq!(refused, [ErrorKind::Conflict; 7]);
            assert_eq!(replacer_ids(&catalog, &old).await.len(), 1);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn deletes_racing_on_one_chain_splice_both() {
        for catalog in catalogs().await {
            for round in 0..5 {
                let m = format!("x{round}");
                let (s0, ..) = catalog
                    .commit(&m, &meta(&m), None, no_labels())
                    .await
                    .unwrap();
                let mut chain = vec![s0.clone()];
                for changed in ["a", "b", "c"] {
                    let entry = step(chain.last().unwrap(), &[], &[changed], &[]);
                    let (e, _) = catalog.add_entry(&m, entry).await.unwrap().unwrap();
                    chain.push(e);
                }
                racing(2, |i| {
                    let catalog = catalog.clone();
                    let e = chain[i + 1].clone();
                    async move { catalog.remove_entry(&e).await.unwrap() }
                })
                .await;

                let top = catalog.entry(&chain[3]).await.unwrap();
                assert_eq!(top.replaces, link(&s0, &["a", "b", "c"]), "{round}");
                let spliced = replacement(&[], &["a", "b", "c"], &[]);
                assert_eq!(top.replacement, Some(spliced), "{round}");
                assert_eq!(replacer_ids(&catalog, &s0).await, [chain[3].clone()]);
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_removals_of_the_last_entries_remove_the_module() {
        for catalog in catalogs().await {
            for round in 0..5 {
                let m = format!("x{round}");
                let (a, ..) = catalog
                    .commit(&m, &meta(&m), None, no_labels())
                    .await
                    .unwrap();
                let (b, _) = catalog.add_entry(&m, no_labels()).await.unwrap().unwrap();
                let removed = racing(2, |i| {
                    let catalog = catalog.clone();
                    let e = [a.clone(), b.clone()][i].clone();
                    async move { catalog.remove_entry(&e).await.unwrap().1 }
                })
                .await;
                assert_eq!(removed.iter().flatten().count(), 1, "{round}");
                assert!(catalog.module(&m).await.is_err());
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_module_is_kept_iff_an_entry_is_on_it() {
        for catalog in catalogs().await {
            for round in 0..5 {
                let m = format!("x{round}");
                let (e, ..) = catalog
                    .commit(&m, &meta(&m), None, no_labels())
                    .await
                    .unwrap();
                let remove = {
                    let catalog = catalog.clone();
                    tokio::spawn(async move { catalog.remove_entry(&e).await.unwrap() })
                };
                let add = {
                    let catalog = catalog.clone();
                    let m = m.clone();
                    tokio::spawn(async move { catalog.add_entry(&m, no_labels()).await.unwrap() })
                };
                let (_, removed) = remove.await.unwrap();
                let added = add.await.unwrap();
                assert_eq!(removed.is_some(), added.is_none(), "{round}");
                assert_eq!(
                    catalog.module(&m).await.is_ok(),
                    catalog.has_entries(&m).await.unwrap()
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn one_of_racing_owners_takes_a_lease() {
        for catalog in catalogs().await {
            let taken = racing(8, |i| {
                let catalog = catalog.clone();
                async move {
                    let ttl = Duration::from_secs(30);
                    catalog
                        .take_lease("l", &format!("o{i}"), ttl)
                        .await
                        .unwrap()
                }
            })
            .await;
            assert_eq!(taken.iter().flatten().count(), 1, "{taken:?}");
        }
    }
}
