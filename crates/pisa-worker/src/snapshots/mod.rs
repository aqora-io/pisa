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
//! The kernel re-check runs in a process of its own, after the build: at
//! creation unless the request opts out, and later on request. A module's
//! record is rewritten once it passes.
//!
//! The work is split by what it needs: `compare` is pure, `read`, `plan` and
//! `audit` read the catalog, and `cache`, `create`, `build`, `verify` and
//! `delete` the store and the local cache too.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use pisa_protocol::api::{
    Allow, AuditAssumption, AuditComparison, AuditFork, AuditLink, AuditSnapshotsResponse,
    AuditTarget, AuditUnproved, CheckResult, CreateSnapshotResponse, Declaration,
    DeleteSnapshotResponse, Error, ErrorKind, Export, ListDependentsResponse,
    ListSnapshotsResponse, RebuildItem, RebuildReplaced, RebuildSnapshotsResponse, Replacement,
    Replaces, Snapshot, SnapshotDetail, SnapshotId, VerifyFailure, VerifySnapshotResponse,
};
use pisa_protocol::header::{Split, header_key, split_header};
use pisa_protocol::labels::{
    MAX_LABELS, MAX_LIMIT, check_key, check_labels, is_cursor, snapshot_id_error,
};
use pisa_protocol::repl::CommandResponse;
use pisa_protocol::search::{Description, Informal};
use pisa_protocol::snapshot;
use pisa_protocol::verify::{
    Change, DeclarationChange, ProofStatus, ReportedDeclaration, Verification, grouped, normalised,
    parent_in, statement_changes,
};
use pisa_protocol::wire::{CheckOutcome, SnapshotCreateJob, SnapshotRebuildJob};
use sha2::{Digest, Sha256};

use crate::catalog::{
    Catalog, EntryMeta, Module, ModuleFile, ModuleMeta, NewEntry, SnapshotDecls, record_fits,
    unknown_module,
};
use crate::check::{header_error, policy_error};
use crate::db::Db;
use crate::launcher::{Unfinished, Verdict};
use crate::policy::{Policy, parse_declarations, parse_export, uses_macros};
use crate::pool::{Lease, Pool};

mod audit;
mod build;
mod cache;
mod compare;
mod create;
mod delete;
mod plan;
mod read;
#[cfg(test)]
mod tests;
mod verify;

use cache::*;
use compare::*;
use create::*;
use plan::*;
use verify::*;

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

/// The ids of `modules`.
fn ids(modules: &[Module]) -> impl Iterator<Item = &String> {
    modules.iter().map(|m| &m.id)
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

    /// The snapshot id of entry `entry`, as the API answers it.
    pub(crate) fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub(crate) fn id(&self, entry: &str) -> String {
        let id = SnapshotId {
            version: self.version.clone(),
            entry: entry.to_string(),
        };
        id.to_string()
    }

    /// The entry snapshot id `id` names, if it is of this worker's version.
    pub(crate) fn parse(&self, id: &str) -> Result<String, Error> {
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
            holes: entry_meta.holes,
            status: meta.status,
            verified: meta.verified,
            created: entry_meta.created_micros / 1_000_000,
        }
    }
}
