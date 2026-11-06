//! A rebuild that the server runs: the plan a worker makes (see
//! `POST /snapshots/rebuild` with `dry_run`), created a level at a time
//! through ordinary create jobs, then, if asked, the old snapshots retired.
//! Each create has the plan's idempotency key, so running the same rebuild
//! again replays the creates that are done.

use std::collections::HashSet;
use std::sync::Arc;

use pisa_protocol::api::{
    CheckResult, CreateSnapshotRequest, CreateSnapshotResponse, Error, ErrorKind, RebuildCreated,
    RebuildFailed, RebuildItem, RebuildSkipped, RebuildSnapshotsRequest, RebuildSnapshotsResponse,
    VersionQuery,
};
use pisa_protocol::header::split_header;
use pisa_protocol::snapshot;
use pisa_protocol::wire::{Job, JobResult, SnapshotJob, SnapshotRebuildJob};

use crate::dispatch::{DispatchError, Priority};
use crate::http::{ApiError, App};
use crate::sessions::unexpected;
use crate::snapshots::{prepare_create, submit};

/// How many creates of a level run at once.
const PARALLEL: usize = 8;

/// How many times a create that no worker took in time is sent again.
const CAPACITY_RETRIES: u32 = 5;

/// Plans `req` (as a dry run) and runs the plan; see [`RebuildSnapshotsResponse`].
pub(crate) async fn run(
    app: &Arc<App>,
    req: &RebuildSnapshotsRequest,
) -> Result<RebuildSnapshotsResponse, ApiError> {
    let q = VersionQuery {
        version: req.version.clone(),
    };
    let job = Job::SnapshotRebuild(SnapshotRebuildJob {
        from: req.from.clone(),
        to: req.to.clone(),
        labels: req.labels.clone(),
        allow_changes: req.allow_changes,
    });
    let mut response = match submit(app, job, q.clone(), Priority::Interactive).await? {
        JobResult::SnapshotRebuild(r) => r.map_err(ApiError)?,
        other => return Err(unexpected(other)),
    };
    if !response.ok {
        return Ok(response);
    }

    // The new snapshots that weren't made: what imports one isn't either.
    let mut missing: HashSet<String> = HashSet::new();
    let mut levels: Vec<usize> = response.plan.iter().map(|i| i.level).collect();
    levels.dedup();
    for level in levels {
        let items = response.plan.iter().filter(|i| i.level == level);
        let (ready, blocked): (Vec<&RebuildItem>, Vec<&RebuildItem>) =
            items.partition(|i| !builds_on(i, &missing));
        for item in blocked {
            missing.insert(item.new_module.clone());
            response.skipped.push(RebuildSkipped {
                snapshot_id: item.snapshot_id.clone(),
                module: item.module.clone(),
            });
        }
        for chunk in ready.chunks(PARALLEL) {
            let running: Vec<_> = chunk
                .iter()
                .map(|item| {
                    let (app, req, item) = (app.clone(), req.clone(), (*item).clone());
                    tokio::spawn(async move { create(&app, &req, &item).await })
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
                        response.created.push(RebuildCreated {
                            snapshot_id: item.snapshot_id.clone(),
                            module: item.module.clone(),
                            new_snapshot_id: made.id,
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
    if req.retire && response.ok {
        response.retired = retire(app, req, &response.plan, q).await?;
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

/// Creates `item` as the plan says, sending it again while no worker takes
/// it in time.
async fn create(
    app: &App,
    req: &RebuildSnapshotsRequest,
    item: &RebuildItem,
) -> Result<CreateSnapshotResponse, Error> {
    let create = CreateSnapshotRequest {
        code: item.code.clone(),
        version: req.version.clone(),
        timeout: req.timeout,
        verify: req.verify,
        labels: item.labels.clone(),
        key: Some(item.key.clone()),
        replaces: item.replaces.clone(),
    };
    let p = prepare_create(app, create);
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

/// Deletes the plan's old snapshots, from the top down, then `from`. One
/// already gone counts, as when a rebuild that retired some is sent again.
async fn retire(
    app: &App,
    req: &RebuildSnapshotsRequest,
    plan: &[RebuildItem],
    q: VersionQuery,
) -> Result<Vec<String>, ApiError> {
    let old = plan.iter().rev().filter_map(|i| i.snapshot_id.clone());
    let mut retired = Vec::new();
    for entry in old.chain([req.from.clone()]) {
        let job = Job::SnapshotDelete(SnapshotJob {
            entry: entry.clone(),
        });
        match submit(app, job, q.clone(), Priority::Interactive).await? {
            JobResult::SnapshotDeleted(Ok(_)) => retired.push(entry),
            JobResult::SnapshotDeleted(Err(e)) if e.kind == ErrorKind::NotFound => {
                retired.push(entry)
            }
            JobResult::SnapshotDeleted(Err(e)) => return Err(ApiError(e)),
            other => return Err(unexpected(other)),
        }
    }
    Ok(retired)
}
