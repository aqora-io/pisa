//! Snapshot metadata in the database: the modules, which import each other,
//! and the entries, which keep them.
//!
//! Tables (see `migrations/`), each keyed by pool version first:
//! - `modules`: module `m` ([`ModuleMeta`]), whose files are in object storage
//!   under the build named in the record, uploaded before the row is written,
//!   and its declarations for the search service ([`SnapshotDecls`])
//! - `module_imports`: `m` imports `parent`
//! - `entries`: entry `e` ([`EntryMeta`]), its labels, and the idempotency key
//!   it was made with
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

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, SystemTime};

use pisa_protocol::api::{
    Allow, CheckResult, Error, ErrorKind, ProofStatus, Replacement, Replaces, SnapshotId,
};
use pisa_protocol::snapshot;
use pisa_protocol::verify::normalised;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::dispatch;
use crate::db::{Db, DbError, IsConflict};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleMeta {
    /// Ids of the modules it imports.
    pub imports: Vec<String>,
    /// Every module it needs, its own last.
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
    /// The declaration's signature, with notation.
    pub signature: String,
    /// The type with full names and no notation.
    pub type_pp_full: String,
    #[serde(default)]
    pub type_refs: Vec<String>,
    #[serde(default)]
    pub value_refs: Vec<String>,
    #[serde(default)]
    pub docstring: Option<String>,
    #[serde(default)]
    pub deprecated_to: Option<String>,
    /// Depends on `sorry`, by the module's check result.
    #[serde(default)]
    pub uses_sorry: bool,
    /// What its own axioms make it, by the module's check result. The
    /// export doesn't print it.
    #[serde(default = "proved")]
    pub status: ProofStatus,
}

fn proved() -> ProofStatus {
    ProofStatus::Proved
}

/// A module's declarations for the search service, with the module's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDecls {
    /// See [`ModuleMeta::header`].
    pub header: String,
    pub verified: bool,
    pub decls: Vec<SnapshotDecl>,
}

impl SnapshotDecls {
    /// `decls` for the module `meta`, flagged from its check result.
    pub fn new(meta: &ModuleMeta, decls: Vec<SnapshotDecl>) -> SnapshotDecls {
        let mut out = SnapshotDecls {
            header: meta.header.clone(),
            verified: false,
            decls,
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

/// What a new entry is given.
pub struct NewEntry {
    pub labels: BTreeMap<String, String>,
    pub key: Option<String>,
    /// The entry it replaces, which the caller has compared it with.
    pub replaces: Option<Replaces>,
    /// What the comparison with it found; set exactly when `replaces` is.
    pub replacement: Option<Replacement>,
    /// Whether it may replace an entry that another entry replaces already,
    /// making a fork. It isn't stored.
    pub fork: bool,
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

/// An entry's id, module, labels, key, creation time, the entry it replaces
/// with what it allowed and the comparison, both as JSON text, as selected.
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

pub(crate) fn entry_from_row(row: EntryRow) -> Result<(String, EntryMeta), Fail> {
    let (e, module, labels, key, created, replaces, allow, replacement) = row;
    let replaces = match replaces {
        Some(entry) => Some(Replaces {
            snapshot_id: entry,
            allow: allow.map(|text| serde_json::from_str(&text)).transpose()?,
        }),
        None => None,
    };
    let meta = EntryMeta {
        module,
        labels: serde_json::from_value(labels)?,
        key,
        created_micros: created as u64,
        replaces,
        replacement: replacement
            .map(|text| serde_json::from_str(&text))
            .transpose()?,
    };
    Ok((e, meta))
}

/// What the `replaces_allow` column holds for `replaced`: its `allow` as
/// JSON text.
pub(crate) fn allow_column(replaced: &Replaces) -> Result<Option<String>, Fail> {
    Ok(replaced
        .allow
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?)
}

/// The link and comparison of `replacer` once `deleted`, which it replaces,
/// is gone: `deleted`'s link with the allowed changes of both steps. `None`
/// if `deleted` has no link.
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
    let allow = (!replacement.changed_definitions.is_empty()).then(|| Allow {
        definitions: replacement.changed_definitions.clone(),
    });
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

pub(crate) fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// `labels` with each key once, or `None` if they give a key two values,
/// which no entry matches: an entry has one value per key.
fn wanted(labels: &[(String, String)]) -> Option<Vec<(String, String)>> {
    let mut wanted: BTreeMap<&str, &str> = BTreeMap::new();
    for (k, value) in labels {
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
        let record = to_record(meta)?;
        let decls = decls.map(to_record).transpose()?;
        dispatch!(
            &self.db,
            commit(&self.version, m, meta, &record, decls.as_ref(), &entry)
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
            fork: false,
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
            fork: false,
        }
    }

    fn keyed(k: &str) -> NewEntry {
        NewEntry {
            labels: BTreeMap::new(),
            key: Some(k.into()),
            replaces: None,
            replacement: None,
            fork: false,
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
