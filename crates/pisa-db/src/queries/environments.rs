// The queries for `Environments` (see `crate::environments`). Included by
// `backend.rs` once per backend.

use diesel::{ExpressionMethods, QueryDsl};
use diesel_async::RunQueryDsl;

use crate::db::{DbError, retry};
use crate::schema::environments;

/// Records `version`'s project, or replaces what was recorded of it.
pub(crate) async fn record_environment(
    db: &Handle,
    version: &str,
    toolchain: &str,
    packages: &str,
    files: &str,
    created: i64,
) -> Result<(), DbError> {
    retry(|| async {
        let project = (
            environments::toolchain.eq(toolchain),
            environments::packages.eq(packages),
            environments::files.eq(files),
        );
        diesel::insert_into(environments::table)
            .values((
                environments::version.eq(version),
                project,
                environments::created_micros.eq(created),
            ))
            .on_conflict(environments::version)
            .do_update()
            .set(project)
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

/// Every environment, in no order.
pub(crate) async fn all_environments(
    db: &Handle,
) -> Result<Vec<(String, String, String, String, i64)>, DbError> {
    Ok(environments::table
        .select((
            environments::version,
            environments::toolchain,
            environments::packages,
            environments::files,
            environments::created_micros,
        ))
        .load(&mut *conn(db).await?)
        .await?)
}
