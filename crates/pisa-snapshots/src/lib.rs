//! What reads the snapshot catalog, for the servers and the workers alike:
//! snapshots and their exports, dependents and labels (`read`), the
//! comparison of a replacement with the snapshot it replaces (`compare`),
//! audits (`audit`) and their review lists (`review`), and rebuild plans
//! (`plan`). None of it runs Lean or touches the object store: the workers
//! build, verify and delete snapshots.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use pisa_db::catalog::{Catalog, EntryMeta, Module, ModuleMeta, unknown_module};
use pisa_db::db::Db;
use pisa_protocol::api::{
    Allow, AuditAssumption, AuditComparison, AuditFork, AuditLink, AuditSnapshotsResponse,
    AuditTarget, AuditUnproved, Declaration, Error, ErrorKind, Export, ListDependentsResponse,
    ListSnapshotsResponse, RebuildItem, RebuildReplaced, RebuildSnapshotsResponse, Replacement,
    Replaces, Snapshot, SnapshotDetail, SnapshotId,
};
use pisa_protocol::header::split_header;
use pisa_protocol::labels::{MAX_LABELS, MAX_LIMIT, check_labels, is_cursor, snapshot_id_error};
use pisa_protocol::search::{Description, Informal};
use pisa_protocol::snapshot;
use pisa_protocol::verify::{
    Change, DeclarationChange, ProofStatus, grouped, normalised, parent_in, statement_changes,
};
use pisa_protocol::wire::SnapshotRebuildJob;

mod audit;
pub mod compare;
mod plan;
mod read;
pub mod review;
#[cfg(test)]
mod tests;

use compare::*;
use plan::*;

/// Reads the snapshot catalog of one version.
pub struct Reader {
    catalog: Catalog,
    version: String,
    /// The axioms every proof may use, which an audit leaves out of what it
    /// lists of a declaration that isn't proved.
    allowed_axioms: Vec<String>,
}

/// Makes a [`Reader`] for each version, on one database.
#[derive(Clone)]
pub struct Readers {
    db: Db,
    allowed_axioms: Vec<String>,
}

impl Readers {
    pub fn new(db: Db, allowed_axioms: Vec<String>) -> Self {
        Readers { db, allowed_axioms }
    }

    /// A reader of version `version`'s catalog.
    pub fn version(&self, version: &str) -> Reader {
        Reader::new(
            self.db.clone(),
            version.to_string(),
            self.allowed_axioms.clone(),
        )
    }
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, e.to_string())
}

/// The ids of `modules`.
fn ids(modules: &[Module]) -> impl Iterator<Item = &String> {
    modules.iter().map(|m| &m.id)
}

impl Reader {
    pub fn new(db: Db, version: String, allowed_axioms: Vec<String>) -> Self {
        Reader {
            catalog: Catalog::new(db, version.clone()),
            version,
            allowed_axioms,
        }
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// The snapshot id of entry `entry`, as the API answers it.
    pub fn id(&self, entry: &str) -> String {
        let id = SnapshotId {
            version: self.version.clone(),
            entry: entry.to_string(),
        };
        id.to_string()
    }

    /// The entry snapshot id `id` names, if it is of this reader's version.
    pub fn parse(&self, id: &str) -> Result<String, Error> {
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
    pub fn snapshot(&self, entry: String, entry_meta: EntryMeta, meta: &ModuleMeta) -> Snapshot {
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
