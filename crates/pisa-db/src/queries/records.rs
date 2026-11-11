// The queries for `Records` (see `crate::records`), and `Db::rows`. Included
// by `backend.rs` once per backend.

use diesel::{ExpressionMethods, OptionalExtension, QueryDsl};
use diesel_async::RunQueryDsl;
use serde_json::Value;

use crate::db::{DbError, retry};
use crate::records::{DocumentRow, StateRow};
use crate::schema::{
    catalogs, described, documents, entries, leases, links, module_imports, modules, sessions,
    states,
};

pub(crate) async fn put_state(
    db: &Handle,
    v: &str,
    id: &str,
    state: &StateRow,
    created: u64,
) -> Result<(), DbError> {
    retry(|| async {
        diesel::insert_into(states::table)
            .values((
                states::version.eq(v),
                states::id.eq(id),
                states::session.eq(&state.session),
                states::sorry.eq(state.sorry.map(|n| n as i32)),
                states::parent.eq(&state.parent),
                states::tactic.eq(&state.tactic),
                states::created.eq(created as i64),
            ))
            .on_conflict((states::version, states::id))
            .do_update()
            .set(states::created.eq(created as i64))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn state(db: &Handle, v: &str, id: &str) -> Result<Option<StateRow>, DbError> {
    let row = states::table
        .find((v, id))
        .select((
            states::session,
            states::sorry,
            states::parent,
            states::tactic,
        ))
        .first::<(String, Option<i32>, Option<String>, Option<String>)>(&mut *conn(db).await?)
        .await
        .optional()?;
    Ok(row.map(|(session, sorry, parent, tactic)| StateRow {
        session,
        sorry: sorry.map(|n| n as u32),
        parent,
        tactic,
    }))
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

pub(crate) async fn put_document(
    db: &Handle,
    v: &str,
    id: &str,
    code: &str,
    holder: &str,
    now: u64,
) -> Result<(), DbError> {
    retry(|| async {
        diesel::insert_into(documents::table)
            .values((
                documents::version.eq(v),
                documents::id.eq(id),
                documents::code.eq(code),
                documents::revision.eq(1i64),
                documents::holder.eq(holder),
                documents::updated.eq(now as i64),
            ))
            .on_conflict((documents::version, documents::id))
            .do_update()
            .set((
                documents::code.eq(code),
                documents::revision.eq(1i64),
                documents::holder.eq(holder),
                documents::updated.eq(now as i64),
            ))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn document(
    db: &Handle,
    v: &str,
    id: &str,
) -> Result<Option<DocumentRow>, DbError> {
    let row = documents::table
        .find((v, id))
        .select((documents::code, documents::revision, documents::holder))
        .first::<(String, i64, Option<String>)>(&mut *conn(db).await?)
        .await
        .optional()?;
    Ok(row.map(|(code, revision, holder)| DocumentRow {
        code,
        revision: revision as u64,
        holder,
    }))
}

pub(crate) async fn edit_document(
    db: &Handle,
    v: &str,
    id: &str,
    code: &str,
    revision: u64,
    holder: &str,
    now: u64,
) -> Result<Option<u64>, DbError> {
    retry(|| async {
        let revision = diesel::update(
            documents::table
                .find((v, id))
                .filter(documents::revision.eq(revision as i64)),
        )
        .set((
            documents::code.eq(code),
            documents::revision.eq(documents::revision + 1),
            documents::holder.eq(holder),
            documents::updated.eq(now as i64),
        ))
        .returning(documents::revision)
        .get_result::<i64>(&mut *conn(db).await?)
        .await
        .optional()?;
        Ok(revision.map(|r| r as u64))
    })
    .await
}

pub(crate) async fn hold_document(
    db: &Handle,
    v: &str,
    id: &str,
    holder: &str,
    now: u64,
) -> Result<bool, DbError> {
    retry(|| async {
        let n = diesel::update(documents::table.find((v, id)))
            .set((
                documents::holder.eq(holder),
                documents::updated.eq(now as i64),
            ))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(n == 1)
    })
    .await
}

pub(crate) async fn delete_document(db: &Handle, v: &str, id: &str) -> Result<bool, DbError> {
    retry(|| async {
        let n = diesel::delete(documents::table.find((v, id)))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(n == 1)
    })
    .await
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
        let documents = diesel::delete(
            documents::table
                .filter(documents::version.eq(v))
                .filter(documents::updated.lt(cutoff)),
        )
        .execute(&mut *c)
        .await?;
        Ok(states + sessions + documents)
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
    for e in links::table
        .filter(links::version.eq(v))
        .select(links::entry)
        .load::<String>(c)
        .await?
    {
        rows.push(named("links", vec![e]));
    }
    for (m, name) in described::table
        .filter(described::version.eq(v))
        .select((described::module, described::name))
        .load::<(String, String)>(c)
        .await?
    {
        rows.push(named("described", vec![m, name]));
    }
    for id in catalogs::table
        .filter(catalogs::version.eq(v))
        .select(catalogs::id)
        .load::<String>(c)
        .await?
    {
        rows.push(named("catalogs", vec![id]));
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
        .select(states::id)
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
    for id in documents::table
        .filter(documents::version.eq(v))
        .select(documents::id)
        .load::<String>(c)
        .await?
    {
        rows.push(named("documents", vec![id]));
    }
    Ok(rows)
}
