//! A resubmit that the server runs: its plan (see `POST /snapshots/resubmit`
//! with `dry_run`), then a create for each head that keeps the amending
//! spec's statements: the head's code again, replacing that spec. Each
//! create has the plan's idempotency key, so running the same resubmit again
//! replays the creates that are done.

use std::sync::Arc;

use pisa_protocol::api::{
    CheckResult, CreateSnapshotRequest, ErrorKind, Replaces, ResubmitSnapshotsRequest,
    ResubmitSnapshotsResponse,
};

use crate::http::{ApiError, App};
use crate::rebuild::create_retrying;
use crate::snapshots::parse_id;

/// The version `req` runs in: that of its `from` and its `to`, which must
/// be the same.
pub(crate) fn version(req: &ResubmitSnapshotsRequest) -> Result<String, ApiError> {
    let (from, to) = (parse_id(&req.from)?.version, parse_id(&req.to)?.version);
    if from != to {
        return Err(ApiError::new(
            ErrorKind::BadRequest,
            format!(
                "from is of version {from} and to of version {to}: an amending spec is of its \
                 spec's version"
            ),
        ));
    }
    Ok(from)
}

/// Plans `req` and, unless it is a dry run, creates the heads that pass on
/// `version`, oldest first; see [`ResubmitSnapshotsResponse`].
pub(crate) async fn run(
    app: &Arc<App>,
    req: &ResubmitSnapshotsRequest,
    version: &str,
) -> Result<ResubmitSnapshotsResponse, ApiError> {
    let reader = app.snapshots.version(version);
    let mut response = reader.resubmit_plan(&req.from, &req.to).await?;
    if req.dry_run {
        return Ok(response);
    }
    let to = reader.parse(&req.to)?;
    for item in response.items.iter_mut().filter(|i| i.passes) {
        // A head comes over forked once the new spec has a replacer: an
        // earlier item's, so that a forked run comes over forked, or
        // another create's. A key replay comes before that check, so the
        // same resubmit sent again replays.
        let fork = match reader.replaced_by(&to).await {
            Ok(replaced) => replaced.is_some(),
            Err(error) => {
                item.check = Some(CheckResult {
                    error: Some(error),
                    ..Default::default()
                });
                continue;
            }
        };
        let made = async {
            let head = reader.get(&item.snapshot_id).await?;
            let create = CreateSnapshotRequest {
                code: head.code,
                version: Some(version.to_string()),
                timeout: req.timeout,
                verify: req.verify,
                labels: req.labels.clone().unwrap_or(head.snapshot.labels),
                key: Some(item.key.clone()),
                replaces: Some(Replaces {
                    snapshot_id: req.to.clone(),
                    allow: None,
                }),
                fork,
                holes: None,
                amends: None,
                refutes: None,
                detail: Vec::new(),
            };
            create_retrying(app, create).await
        };
        match made.await {
            Ok(r) => match r.snapshot {
                Some(s) => item.new_snapshot_id = Some(s.id),
                None => item.check = Some(r.check),
            },
            Err(error) => {
                item.check = Some(CheckResult {
                    error: Some(error),
                    ..Default::default()
                });
            }
        }
    }
    response.ok = (response.items.iter()).all(|i| i.passes && i.new_snapshot_id.is_some());
    Ok(response)
}
