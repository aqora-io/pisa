// The queries for `Records` (see `crate::records`), and `Db::rows`. Included
// by `backend.rs` once per backend.

use diesel::{ExpressionMethods, OptionalExtension, QueryDsl};
use diesel_async::RunQueryDsl;
use serde_json::Value;

use crate::db::{DbError, retry};
use crate::schema::{described, entries, leases, module_imports, modules, sessions, states};

pub(crate) async fn put_state(
    db: &Handle,
    v: &str,
    hash: &str,
    created: u64,
    meta: &Value,
    holder: &str,
) -> Result<(), DbError> {
    retry(|| async {
        diesel::insert_into(states::table)
            .values((
                states::version.eq(v),
                states::hash.eq(hash),
                states::meta.eq(meta),
                states::created.eq(created as i64),
                states::holder.eq(holder),
            ))
            .on_conflict((states::version, states::hash))
            .do_update()
            .set((
                states::meta.eq(meta),
                states::created.eq(created as i64),
                states::holder.eq(holder),
            ))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn state(db: &Handle, v: &str, hash: &str) -> Result<Option<Value>, DbError> {
    Ok(states::table
        .find((v, hash))
        .select(states::meta)
        .first(&mut *conn(db).await?)
        .await
        .optional()?)
}

pub(crate) async fn put_session(
    db: &Handle,
    v: &str,
    id: &str,
    created: u64,
    meta: &Value,
) -> Result<(), DbError> {
    retry(|| async {
        diesel::insert_into(sessions::table)
            .values((
                sessions::version.eq(v),
                sessions::id.eq(id),
                sessions::meta.eq(meta),
                sessions::created.eq(created as i64),
            ))
            .on_conflict((sessions::version, sessions::id))
            .do_update()
            .set((
                sessions::meta.eq(meta),
                sessions::created.eq(created as i64),
            ))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn session(db: &Handle, v: &str, id: &str) -> Result<Option<Value>, DbError> {
    Ok(sessions::table
        .find((v, id))
        .select(sessions::meta)
        .first(&mut *conn(db).await?)
        .await
        .optional()?)
}

pub(crate) async fn sweep(db: &Handle, v: &str, cutoff: u64) -> Result<usize, DbError> {
    let cutoff = cutoff as i64;
    retry(|| async {
        let mut c = conn(db).await?;
        let states = diesel::delete(
            states::table
                .filter(states::version.eq(v))
                .filter(states::created.lt(cutoff)),
        )
        .execute(&mut *c)
        .await?;
        let sessions = diesel::delete(
            sessions::table
                .filter(sessions::version.eq(v))
                .filter(sessions::created.lt(cutoff)),
        )
        .execute(&mut *c)
        .await?;
        Ok(states + sessions)
    })
    .await
}

pub(crate) async fn rows(db: &Handle, v: &str) -> Result<Vec<Vec<String>>, DbError> {
    let mut c = conn(db).await?;
    let c = &mut *c;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let named = |table: &str, key: Vec<String>| [vec![table.to_string()], key].concat();
    for m in modules::table
        .filter(modules::version.eq(v))
        .select(modules::id)
        .load::<String>(c)
        .await?
    {
        rows.push(named("modules", vec![m]));
    }
    for (m, parent) in module_imports::table
        .filter(module_imports::version.eq(v))
        .select((module_imports::module, module_imports::parent))
        .load::<(String, String)>(c)
        .await?
    {
        rows.push(named("module_imports", vec![m, parent]));
    }
    for e in entries::table
        .filter(entries::version.eq(v))
        .select(entries::id)
        .load::<String>(c)
        .await?
    {
        rows.push(named("entries", vec![e]));
    }
    for (m, name) in described::table
        .filter(described::version.eq(v))
        .select((described::module, described::name))
        .load::<(String, String)>(c)
        .await?
    {
        rows.push(named("described", vec![m, name]));
    }
    for name in leases::table
        .filter(leases::version.eq(v))
        .select(leases::name)
        .load::<String>(c)
        .await?
    {
        rows.push(named("leases", vec![name]));
    }
    for hash in states::table
        .filter(states::version.eq(v))
        .select(states::hash)
        .load::<String>(c)
        .await?
    {
        rows.push(named("states", vec![hash]));
    }
    for id in sessions::table
        .filter(sessions::version.eq(v))
        .select(sessions::id)
        .load::<String>(c)
        .await?
    {
        rows.push(named("sessions", vec![id]));
    }
    Ok(rows)
}
