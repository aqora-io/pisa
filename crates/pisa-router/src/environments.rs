//! `GET /environments`: the versions workers serve or have served, each with
//! the Lake project behind it. The server reads the database itself, with no
//! job.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use pisa_db::environments::Environments;
use pisa_db::queue::LIVE;
use pisa_protocol::api::{self, ApiErrors, Environment, ListEnvironmentsResponse};

use crate::http::{ApiError, App};

#[utoipa::path(
    get, path = "/environments", operation_id = "listEnvironments", tag = "health",
    summary = "The environments workers serve, or have served",
    description = "Every version a worker has registered, with the Lake project behind it: \
        its toolchain, the packages it pins and its files. An environment stays listed \
        after its workers are gone, so what a snapshot's or a check's `version` ran on can \
        be read, and built again; `pool` has its live workers, and is absent while it has \
        none. A request reaches an environment by naming its `version`.",
    responses(
        (status = 200, description = "The environments, by version", body = ListEnvironmentsResponse),
        ApiErrors,
    ),
)]
/// `GET /environments`.
pub async fn list(State(app): State<Arc<App>>) -> Result<Json<ListEnvironmentsResponse>, ApiError> {
    let queue = app.dispatcher.queue();
    let environments = Environments::new(queue.db().clone());
    let (recorded, pools) = tokio::try_join!(environments.list(), queue.pools(LIVE))
        .map_err(|e| ApiError(api::Error::from(e)))?;
    let mut pools: HashMap<String, api::PoolHealth> = (pools.into_iter())
        .map(|pool| (pool.version.clone(), pool))
        .collect();
    let environments = recorded
        .into_iter()
        .map(|e| Environment {
            pool: pools.remove(&e.version),
            version: e.version,
            toolchain: e.toolchain,
            packages: e.packages,
            files: e.files,
            created: e.created_micros / 1_000_000,
        })
        .collect();
    Ok(Json(ListEnvironmentsResponse { environments }))
}
