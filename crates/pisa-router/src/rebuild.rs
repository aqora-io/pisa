//! A rebuild that the server runs: the plan a worker makes (see
//! `POST /snapshots/rebuild` with `dry_run`), created a level at a time
//! through ordinary create jobs, then the support snapshots deleted, and, if
//! asked, the old snapshots retired. Each create has the plan's idempotency
//! key, so running the same rebuild again replays the creates that are done.

use std::collections::HashSet;
use std::sync::Arc;

use pisa_protocol::api::{
    CheckResult, CreateSnapshotRequest, CreateSnapshotResponse, Error, ErrorKind, RebuildCreated,
    RebuildFailed, RebuildItem, RebuildSkipped, RebuildSnapshotsRequest, RebuildSnapshotsResponse,
};
use pisa_protocol::header::split_header;
use pisa_protocol::snapshot;
use pisa_protocol::wire::{Job, JobResult, SnapshotDeleteJob, SnapshotRebuildJob};

use crate::dispatch::{DispatchError, Priority};
use crate::http::{ApiError, App};
use crate::sessions::unexpected;
use crate::snapshots::{parse_id, prepare_create, submit};

/// How many creates of a level run at once.
const PARALLEL: usize = 8;

/// How many times a create that no worker took in time is sent again.
const CAPACITY_RETRIES: u32 = 5;

/// The version `req` rebuilds in: that of its `from` and its `to`, which
/// must be the same.
pub(crate) fn version(req: &RebuildSnapshotsRequest) -> Result<String, ApiError> {
    let (from, to) = (parse_id(&req.from)?.version, parse_id(&req.to)?.version);
    if from != to {
        return Err(ApiError::new(
            ErrorKind::BadRequest,
            format!(
                "from is of version {from} and to of version {to}: a rebuild stays in one version"
            ),
        ));
    }
    Ok(from)
}

/// Plans `req` (as a dry run) and runs the plan on `version`, the one
/// [`version`] found; see [`RebuildSnapshotsResponse`]. The snapshots of the
/// plan's support items, made so that the items above them could build, are
/// deleted once every level has run, whether or not they did.
pub(crate) async fn run(
    app: &Arc<App>,
    req: &RebuildSnapshotsRequest,
    version: &str,
) -> Result<RebuildSnapshotsResponse, ApiError> {
    let job = Job::SnapshotRebuild(SnapshotRebuildJob {
        from: req.from.clone(),
        to: req.to.clone(),
        labels: req.labels.clone(),
        allow_changes: req.allow_changes,
    });
    let version = Some(version.to_string());
    let mut response = match submit(app, job, version.clone(), Priority::Interactive).await? {
        JobResult::SnapshotRebuild(r) => r.map_err(ApiError)?,
        other => return Err(unexpected(other)),
    };
    if !response.ok {
        return Ok(response);
    }

    // The new snapshots that weren't made: what imports one isn't either.
    let mut missing: HashSet<String> = HashSet::new();
    // The support snapshots made, to delete once every level has run.
    let mut support: Vec<String> = Vec::new();
    let mut levels: Vec<usize> = response.plan.iter().map(|i| i.level).collect();
    levels.dedup();
    for level in levels {
        let mut ready: Vec<&RebuildItem> = Vec::new();
        for item in response.plan.iter().filter(|i| i.level == level) {
            if builds_on(item, &missing) {
                missing.insert(item.new_module.clone());
                response.skipped.push(RebuildSkipped {
                    snapshot_id: item.snapshot_id.clone(),
                    module: item.module.clone(),
                });
            } else {
                ready.push(item);
            }
        }
        for chunk in ready.chunks(PARALLEL) {
            let running: Vec<_> = chunk
                .iter()
                .map(|item| {
                    let (app, req, item) = (app.clone(), req.clone(), (*item).clone());
                    let version = version.clone();
                    tokio::spawn(async move { create(&app, &req, version, &item).await })
                })
                .collect();
            for (item, running) in chunk.iter().zip(running) {
                let made = running.await.unwrap_or_else(|e| {
                    Err(Error::new(
                        ErrorKind::Internal,
                        format!("a create stopped: {e}"),
                    ))
                });
                match made {
                    Ok(CreateSnapshotResponse {
                        snapshot: Some(made),
                        ..
                    }) if made.module == item.new_module => {
                        let new_snapshot_id = match item.snapshot_id {
                            Some(_) => Some(made.id),
                            None => {
                                support.push(made.id);
                                None
                            }
                        };
                        response.created.push(RebuildCreated {
                            snapshot_id: item.snapshot_id.clone(),
                            module: item.module.clone(),
                            new_snapshot_id,
                            new_module: made.module,
                        });
                    }
                    made => {
                        missing.insert(item.new_module.clone());
                        let check = match made {
                            Ok(r) => r.check,
                            Err(error) => CheckResult {
                                error: Some(error),
                                ..Default::default()
                            },
                        };
                        response.failed.push(RebuildFailed {
                            snapshot_id: item.snapshot_id.clone(),
                            module: item.module.clone(),
                            check,
                        });
                    }
                }
            }
        }
    }
    response.ok = response.failed.is_empty() && response.skipped.is_empty();
    for entry in support.into_iter().rev() {
        delete(app, entry, version.clone()).await?;
    }
    if req.retire && response.ok {
        response.retired = retire(app, &response.plan, version).await?;
    }
    Ok(response)
}

/// Whether `item`'s code imports one of `missing`.
fn builds_on(item: &RebuildItem, missing: &HashSet<String>) -> bool {
    let imports = snapshot::imports(&split_header(&item.code).header).unwrap_or_default();
    imports
        .iter()
        .any(|id| missing.contains(&snapshot::module(id)))
}

/// Creates `item` on `version` as the plan says, sending it again while no
/// worker takes it in time.
async fn create(
    app: &App,
    req: &RebuildSnapshotsRequest,
    version: Option<String>,
    item: &RebuildItem,
) -> Result<CreateSnapshotResponse, Error> {
    let create = CreateSnapshotRequest {
        code: item.code.clone(),
        version,
        timeout: req.timeout,
        verify: req.verify,
        labels: item.labels.clone(),
        key: Some(item.key.clone()),
        replaces: item.replaces.clone(),
        fork: false,
        holes: None,
    };
    let p = prepare_create(app, create).map_err(|e| e.0)?;
    let mut tries = 0;
    loop {
        match app.dispatcher.submit(p.clone()).await {
            Err(DispatchError::NoCapacity) if tries < CAPACITY_RETRIES => tries += 1,
            Err(e) => return Err(ApiError::from(e).0),
            Ok(JobResult::SnapshotCreated(r)) => return Ok(*r),
            Ok(other) => return Err(unexpected(other).0),
        }
    }
}

/// Deletes the plan's old snapshots whose copies replace them, from the top
/// down, each delete splicing the run through the copy; the rest, and
/// `from`, stay. One already gone counts, as when a rebuild that retired
/// some is sent again.
async fn retire(
    app: &App,
    plan: &[RebuildItem],
    version: Option<String>,
) -> Result<Vec<String>, ApiError> {
    let linked = plan.iter().rev().filter(|i| i.replaces.is_some());
    let old = linked.filter_map(|i| i.snapshot_id.clone());
    let mut retired = Vec::new();
    for entry in old {
        delete(app, entry.clone(), version.clone()).await?;
        retired.push(entry);
    }
    Ok(retired)
}

/// Deletes snapshot `entry` on `version`; one already gone counts.
async fn delete(app: &App, entry: String, version: Option<String>) -> Result<(), ApiError> {
    let job = Job::SnapshotDelete(SnapshotDeleteJob {
        entry,
        with_replacers: false,
    });
    match submit(app, job, version, Priority::Interactive).await? {
        JobResult::SnapshotDeleted(Ok(_)) => Ok(()),
        JobResult::SnapshotDeleted(Err(e)) if e.kind == ErrorKind::NotFound => Ok(()),
        JobResult::SnapshotDeleted(Err(e)) => Err(ApiError(e)),
        other => Err(unexpected(other)),
    }
}
