// The queries for `DescriptionFlags` (see `crate::description_flags`).
// Included by `backend.rs` once per backend.

use diesel::{ExpressionMethods, QueryDsl};
use diesel_async::RunQueryDsl;

use crate::db::{DbError, retry};
use crate::schema::description_flags;

/// Names per `IN (…)`, well under either backend's parameter limit.
const FLAG_CHUNK: usize = 500;

pub(crate) async fn add_description_flag(
    db: &Handle,
    name: &str,
    hash: &str,
    reason: &str,
    created: i64,
) -> Result<(), DbError> {
    retry(|| async {
        diesel::insert_into(description_flags::table)
            .values((
                description_flags::name.eq(name),
                description_flags::description_hash.eq(hash),
                description_flags::reason.eq(reason),
                description_flags::created_micros.eq(created),
            ))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

/// How many flags each of `names` has on each of its descriptions.
pub(crate) async fn description_flag_counts(
    db: &Handle,
    names: &[String],
) -> Result<Vec<(String, String, i64)>, DbError> {
    let mut c = conn(db).await?;
    let mut found = Vec::new();
    for chunk in names.chunks(FLAG_CHUNK) {
        let rows: Vec<(String, String, i64)> = description_flags::table
            .filter(description_flags::name.eq_any(chunk))
            .group_by((description_flags::name, description_flags::description_hash))
            .select((
                description_flags::name,
                description_flags::description_hash,
                diesel::dsl::count_star(),
            ))
            .load(&mut *c)
            .await?;
        found.extend(rows);
    }
    Ok(found)
}

/// Every flag, oldest first.
pub(crate) async fn all_description_flags(
    db: &Handle,
) -> Result<Vec<(String, String, String, i64)>, DbError> {
    Ok(description_flags::table
        .order(description_flags::id)
        .select((
            description_flags::name,
            description_flags::description_hash,
            description_flags::reason,
            description_flags::created_micros,
        ))
        .load(&mut *conn(db).await?)
        .await?)
}
