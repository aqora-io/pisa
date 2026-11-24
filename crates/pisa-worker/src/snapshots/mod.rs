//! Environment snapshots: code compiled to a Lean module that later code
//! imports, its files kept in object storage so that any worker of the
//! version can import it. Entries keep modules alive: the same code is one
//! module, however many entries point at it, and a module goes once no entry
//! or kept module needs it. The records, with each module's check result, are
//! in the [`Catalog`], and so are the idempotency keys that make a create safe
//! to retry.
//!
//! Object storage, per pool version:
//! - `catalogs/<version>`: the id of the catalog whose modules the store
//!   holds, written with a worker's first upload. A worker sweeps only the
//!   files of a store that names its own catalog.
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

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, PutMode, PutPayload};
use pisa_protocol::api::{
    AmendedHoles, CheckResult, CreateSnapshotResponse, DeleteSnapshotResponse, Error, ErrorKind,
    Replacement, Snapshot, VerifyFailure, VerifySnapshotResponse,
};
use pisa_protocol::header::{self, Split, header_key, split_header};
use pisa_protocol::labels::{check_key, check_labels};
use pisa_protocol::repl::CommandResponse;
use pisa_protocol::snapshot;
use pisa_protocol::verify::{ReportedDeclaration, Verification};
use pisa_protocol::wire::{CheckOutcome, SnapshotCreateJob};
use sha2::{Digest, Sha256};

use crate::catalog::{
    Catalog, EntryMeta, Module, ModuleFile, ModuleMeta, NewEntry, SnapshotDecls, record_fits,
};
use crate::check::{header_error, policy_error, policy_messages};
use crate::db::Db;
use crate::launcher::{Unfinished, Verdict};
use crate::policy::{Policy, auto_implicit_hint, parse_declarations, parse_export, uses_macros};
use crate::pool::{Lease, Pool};

mod build;
mod cache;
mod create;
mod delete;
#[cfg(test)]
mod tests;
mod verify;

pub(crate) use cache::link_or_copy;
use cache::*;
use create::*;
pub use delete::FileSweep;
use verify::*;

pub use pisa_snapshots::Reader;
use pisa_snapshots::compare::*;

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
    /// What reads the catalog: snapshots, audits, rebuild plans.
    reader: Reader,
    /// Whether this worker has claimed the store, or found it another's.
    marked: AtomicBool,
    /// Headers whose snapshot imports declare no name twice: modules are
    /// named by their code, so one stays clean.
    clean_headers: Mutex<HashSet<String>>,
    /// Whether to make the store's marker name this catalog whatever it
    /// names (`PISA_CLAIM_STORE`).
    claim_store: bool,
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

/// How long [`Snapshots::store_reachable`] waits for the store.
const STORE_PROBE: Duration = Duration::from_secs(10);

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
            catalog: Catalog::new(db.clone(), version.clone()),
            reader: Reader::new(db, version.clone(), policy.allowed_axioms),
            version,
            root,
            confirm_every,
            cached: Mutex::default(),
            confirmed: Mutex::default(),
            locks: Mutex::default(),
            marked: AtomicBool::new(false),
            clean_headers: Mutex::default(),
            claim_store: false,
        }
    }

    /// With `claim`, the store's marker is made to name this catalog, as
    /// `PISA_CLAIM_STORE` asks.
    pub fn claiming_store(self, claim: bool) -> Self {
        Snapshots {
            claim_store: claim,
            ..self
        }
    }

    /// What reads the catalog of this worker's version.
    pub fn reader(&self) -> &Reader {
        &self.reader
    }

    pub(crate) fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The snapshot id of entry `entry`, as the API answers it.
    pub(crate) fn id(&self, entry: &str) -> String {
        self.reader.id(entry)
    }

    /// Whether the module store answers a request, which is a listing of its
    /// first object, within [`STORE_PROBE`].
    pub async fn store_reachable(&self) -> bool {
        let mut listing = self.store.list(None);
        let first = futures::StreamExt::next(&mut listing);
        match tokio::time::timeout(STORE_PROBE, first).await {
            Ok(Some(Err(e))) => {
                tracing::warn!("the module store did not answer: {e}");
                false
            }
            Ok(_) => true,
            Err(_) => {
                tracing::warn!("the module store did not answer within {STORE_PROBE:?}");
                false
            }
        }
    }

    /// The entry snapshot id `id` names, if it is of this worker's version.
    pub(crate) fn parse(&self, id: &str) -> Result<String, Error> {
        self.reader.parse(id)
    }

    /// Entry `entry`, on the module `meta` records, as the API answers it.
    fn snapshot(&self, entry: String, entry_meta: EntryMeta, meta: &ModuleMeta) -> Snapshot {
        self.reader.snapshot(entry, entry_meta, meta)
    }
}
