//! Runs by name: `GET /runs/{name}` reads one and `PUT /runs/{name}` names
//! one. The server reads and writes the catalog itself, with no job.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use pisa_protocol::api::{ApiErrors, ErrorKind, NameRunRequest, NamedRun, RunQuery, SnapshotId};
use pisa_protocol::labels::snapshot_id_error;

use crate::http::{ApiError, App, Body};
use crate::jobs::Params;

#[utoipa::path(
    get, path = "/runs/{name}", operation_id = "getRun", tag = "snapshots",
    summary = "A run by name",
    description = "The run `import PisaRun.<name>` imports: its root, its heads, and, when \
        they are on one module, the module and the head the name resolves to now. A name \
        no run has, or whose run has no snapshot left, is `not_found`.",
    params(("name" = String, Path, description = "The run's name"), RunQuery),
    responses((status = 200, description = "The run", body = NamedRun), ApiErrors),
)]
/// `GET /runs/{name}`.
pub async fn get(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    q: Result<Params<RunQuery>, ApiError>,
) -> Result<Json<NamedRun>, ApiError> {
    let Params(q) = q?;
    let version = app.read_version(q.version.as_deref()).await?;
    let reader = app.snapshots.version(&version);
    Ok(Json(reader.named_run(&name).await?))
}

#[utoipa::path(
    put, path = "/runs/{name}", operation_id = "nameRun", tag = "snapshots",
    summary = "Name a run",
    description = "Names the run that a snapshot is in, as a create's `name` names the run \
        it starts, so that code can import its head as `PisaRun.<name>`. A run has one \
        name: one it had goes. A name another run with snapshots has is a `conflict`.",
    params(("name" = String, Path, description = "The run's new name: dotted parts of letters, digits and `_`")),
    request_body = NameRunRequest,
    responses((status = 200, description = "The run", body = NamedRun), ApiErrors),
)]
/// `PUT /runs/{name}`.
pub async fn put(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    Body(req): Body<NameRunRequest>,
) -> Result<Json<NamedRun>, ApiError> {
    let id = SnapshotId::parse(&req.snapshot_id)
        .ok_or_else(|| ApiError::new(ErrorKind::BadRequest, snapshot_id_error(&req.snapshot_id)))?;
    let reader = app.snapshots.version(&id.version);
    Ok(Json(reader.name_run(&name, &req.snapshot_id).await?))
}
