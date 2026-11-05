// The snapshot catalog's queries, for `Catalog` (see `crate::catalog`).
// Included by `backend.rs` once per backend.

use std::collections::BTreeMap;
use std::time::Duration;

use diesel::dsl::exists;
use diesel::{BoolExpressionMethods, ExpressionMethods, OptionalExtension, QueryDsl};
use diesel_async::RunQueryDsl;
use pisa_protocol::api::{Error, ErrorKind};
use pisa_protocol::labels::check_labels;
use pisa_protocol::snapshot;
use serde_json::Value;

use crate::catalog::{
    EntryMeta, EntryRow, Fail, Logged, ModuleMeta, NewEntry, SnapshotDecls, allow_column,
    entry_from_row,
    now_micros, to_record, unknown_entry, unknown_module,
};
use crate::db::{DbError, retry};
use crate::schema::{changes, described, entries, leases, module_imports, modules};

const ENTRY: (
    entries::id,
    entries::module,
    entries::labels,
    entries::idempotency_key,
    entries::created_micros,
    entries::replaces,
    entries::replaces_allow,
) = (
    entries::id,
    entries::module,
    entries::labels,
    entries::idempotency_key,
    entries::created_micros,
    entries::replaces,
    entries::replaces_allow,
);

async fn module_in(conn: &mut Conn, v: &str, m: &str) -> Result<Option<ModuleMeta>, Fail> {
    let record = modules::table
        .find((v, m))
        .select(modules::meta)
        .first::<Value>(conn)
        .await
        .optional()?;
    Ok(record.map(serde_json::from_value).transpose()?)
}

async fn module_exists(conn: &mut Conn, v: &str, m: &str) -> Result<bool, Fail> {
    Ok(diesel::select(exists(modules::table.find((v, m))))
        .get_result(conn)
        .await?)
}

async fn entry_in(conn: &mut Conn, v: &str, e: &str) -> Result<Option<EntryMeta>, Fail> {
    let row = entries::table
        .find((v, e))
        .select(ENTRY)
        .first::<EntryRow>(conn)
        .await
        .optional()?;
    Ok(row.map(entry_from_row).transpose()?.map(|(_, meta)| meta))
}

/// The entry made with idempotency key `k`, if there is one.
async fn keyed_in(conn: &mut Conn, v: &str, k: &str) -> Result<Option<(String, EntryMeta)>, Fail> {
    let row = entries::table
        .filter(entries::version.eq(v))
        .filter(entries::idempotency_key.eq(k))
        .select(ENTRY)
        .first::<EntryRow>(conn)
        .await
        .optional()?;
    row.map(entry_from_row).transpose()
}

/// Records that the transaction changes entry `entry` and/or module `module`.
async fn log(
    conn: &mut Conn,
    v: &str,
    entry: Option<&str>,
    module: Option<&str>,
) -> Result<(), Fail> {
    diesel::insert_into(changes::table)
        .values((
            changes::version.eq(v),
            changes::logged_micros.eq(now_micros() as i64),
            changes::entry.eq(entry),
            changes::module.eq(module),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

/// Writes a new entry on module `m`, which the transaction has found, with
/// its key, which it has found unused.
async fn add(
    conn: &mut Conn,
    v: &str,
    m: &str,
    entry: &NewEntry,
) -> Result<(String, EntryMeta), Fail> {
    let e = uuid::Uuid::new_v4().simple().to_string();
    let meta = EntryMeta {
        module: m.to_string(),
        labels: entry.labels.clone(),
        key: entry.key.clone(),
        created_micros: now_micros(),
        replaces: entry.replaces.clone(),
    };
    let allow = meta.replaces.as_ref().map(allow_column).transpose()?;
    diesel::insert_into(entries::table)
        .values((
            entries::version.eq(v),
            entries::id.eq(&e),
            entries::module.eq(m),
            entries::labels.eq(to_record(&meta.labels)?),
            entries::idempotency_key.eq(meta.key.as_deref()),
            entries::created_micros.eq(meta.created_micros as i64),
            entries::replaces.eq(meta.replaces.as_ref().map(|r| r.entry.as_str())),
            entries::replaces_allow.eq(allow.flatten()),
        ))
        .execute(conn)
        .await?;
    Ok((e, meta))
}

/// Removes module `m` if no entry references it and no module imports it,
/// with its imports and the search service's records of it.
async fn clean(conn: &mut Conn, v: &str, m: &str) -> Result<Option<ModuleMeta>, Fail> {
    let Some(meta) = module_in(conn, v, m).await? else {
        return Ok(None);
    };
    let referenced: bool = diesel::select(exists(
        entries::table
            .filter(entries::version.eq(v))
            .filter(entries::module.eq(m)),
    ))
    .get_result(conn)
    .await?;
    let imported: bool = diesel::select(exists(
        module_imports::table
            .filter(module_imports::version.eq(v))
            .filter(module_imports::parent.eq(m)),
    ))
    .get_result(conn)
    .await?;
    if referenced || imported {
        return Ok(None);
    }
    diesel::delete(modules::table.find((v, m)))
        .execute(conn)
        .await?;
    Ok(Some(meta))
}

pub(crate) async fn module(db: &Handle, v: &str, m: &str) -> Result<Option<ModuleMeta>, Error> {
    let mut c = conn(db).await?;
    Ok(module_in(&mut c, v, m).await?)
}

pub(crate) async fn update_module(
    db: &Handle,
    v: &str,
    m: &str,
    build: &str,
    f: &(impl Fn(&mut ModuleMeta) + Sync),
) -> Result<Option<ModuleMeta>, Error> {
    let updated = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            let Some((record, decls)) = modules::table
                .find((v, m))
                .select((modules::meta, modules::decls))
                .first::<(Value, Option<Value>)>(conn)
                .await
                .optional()?
            else {
                return Err(Fail::Api(unknown_module(m)));
            };
            let mut meta: ModuleMeta = serde_json::from_value(record)?;
            if meta.modules.last().map(|own| own.build.as_str()) != Some(build) {
                return Ok(None);
            }
            f(&mut meta);
            let decls = match decls {
                Some(decls) => {
                    let mut decls: SnapshotDecls = serde_json::from_value(decls)?;
                    decls.refresh(&meta);
                    Some(to_record(&decls)?)
                }
                None => None,
            };
            diesel::update(modules::table.find((v, m)))
                .set((
                    modules::meta.eq(to_record(&meta)?),
                    modules::decls.eq(decls),
                ))
                .execute(conn)
                .await?;
            log(conn, v, None, Some(m)).await?;
            Ok(Some(meta))
        })
    })
    .await?;
    Ok(updated)
}

pub(crate) async fn keyed(
    db: &Handle,
    v: &str,
    k: &str,
) -> Result<Option<(String, EntryMeta)>, Error> {
    let mut c = conn(db).await?;
    Ok(keyed_in(&mut c, v, k).await?)
}

pub(crate) async fn commit(
    db: &Handle,
    v: &str,
    m: &str,
    meta: &ModuleMeta,
    record: &Value,
    decls: Option<&Value>,
    entry: &NewEntry,
) -> Result<(String, EntryMeta, ModuleMeta), Error> {
    let added = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            if let Some(k) = &entry.key
                && let Some((e, taken)) = keyed_in(conn, v, k).await?
            {
                let stored = if taken.module == m {
                    module_in(conn, v, m)
                        .await?
                        .ok_or_else(|| Fail::Api(unknown_module(m)))?
                } else {
                    meta.clone()
                };
                return Ok((e, taken, stored));
            }
            for parent in &meta.imports {
                let Some(parent_meta) = module_in(conn, v, parent).await? else {
                    return Err(Fail::Api(unknown_module(parent)));
                };
                if !parent_meta.modules.iter().all(|p| meta.modules.contains(p)) {
                    return Err(Fail::Api(Error::new(
                        ErrorKind::NotFound,
                        format!(
                            "{} was deleted and built again while this snapshot was being built",
                            snapshot::module(parent)
                        ),
                    )));
                }
            }
            // Another worker may have just built the same code: keep theirs.
            let stored = match module_in(conn, v, m).await? {
                Some(stored) => stored,
                None => {
                    diesel::insert_into(modules::table)
                        .values((
                            modules::version.eq(v),
                            modules::id.eq(m),
                            modules::meta.eq(record),
                            modules::decls.eq(decls),
                        ))
                        .execute(conn)
                        .await?;
                    for parent in &meta.imports {
                        diesel::insert_into(module_imports::table)
                            .values((
                                module_imports::version.eq(v),
                                module_imports::module.eq(m),
                                module_imports::parent.eq(parent),
                            ))
                            .on_conflict_do_nothing()
                            .execute(conn)
                            .await?;
                    }
                    meta.clone()
                }
            };
            let (e, entry_meta) = add(conn, v, m, entry).await?;
            log(conn, v, Some(&e), Some(m)).await?;
            Ok((e, entry_meta, stored))
        })
    })
    .await?;
    Ok(added)
}

pub(crate) async fn add_entry(
    db: &Handle,
    v: &str,
    m: &str,
    entry: &NewEntry,
) -> Result<Option<(String, EntryMeta)>, Error> {
    let added = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            if let Some(k) = &entry.key
                && let Some(taken) = keyed_in(conn, v, k).await?
            {
                return Ok(Some(taken));
            }
            if !module_exists(conn, v, m).await? {
                return Ok(None);
            }
            let added = add(conn, v, m, entry).await?;
            log(conn, v, Some(&added.0), Some(m)).await?;
            Ok(Some(added))
        })
    })
    .await?;
    Ok(added)
}

pub(crate) async fn entry(db: &Handle, v: &str, e: &str) -> Result<Option<EntryMeta>, Error> {
    let mut c = conn(db).await?;
    Ok(entry_in(&mut c, v, e).await?)
}

pub(crate) async fn remove_entry(
    db: &Handle,
    v: &str,
    e: &str,
) -> Result<(EntryMeta, Option<ModuleMeta>), Error> {
    let removed = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            let Some(meta) = entry_in(conn, v, e).await? else {
                return Err(Fail::Api(unknown_entry(e)));
            };
            diesel::delete(entries::table.find((v, e)))
                .execute(conn)
                .await?;
            let module = clean(conn, v, &meta.module).await?;
            log(conn, v, Some(e), Some(&meta.module)).await?;
            Ok((meta, module))
        })
    })
    .await?;
    Ok(removed)
}

/// Up to `limit` entries, newest first, with every label in `labels`, from
/// before the entry created at `after`'s time with `after`'s id.
pub(crate) async fn list(
    db: &Handle,
    v: &str,
    labels: &[(String, String)],
    limit: i64,
    after: Option<(i64, &str)>,
) -> Result<Vec<(String, EntryMeta)>, Error> {
    let mut query = entries::table.filter(entries::version.eq(v)).into_boxed();
    if let Some((created, e)) = after {
        query = query.filter(
            entries::created_micros
                .lt(created)
                .or(entries::created_micros.eq(created).and(entries::id.lt(e))),
        );
    }
    let rows = label_filter(query, labels)
        .order((entries::created_micros.desc(), entries::id.desc()))
        .limit(limit)
        .select(ENTRY)
        .load::<EntryRow>(&mut *conn(db).await?)
        .await
        .map_err(DbError::from)?;
    Ok(rows
        .into_iter()
        .map(entry_from_row)
        .collect::<Result<_, _>>()?)
}

/// Each module that imports `m`, directly or through others, with those of
/// its imports that are `m` or such modules: one query per level.
pub(crate) async fn dependents(
    db: &Handle,
    v: &str,
    m: &str,
) -> Result<BTreeMap<String, Vec<String>>, Error> {
    let found = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            let mut above: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let mut frontier = vec![m.to_string()];
            while !frontier.is_empty() {
                let edges: Vec<(String, String)> = module_imports::table
                    .filter(module_imports::version.eq(v))
                    .filter(module_imports::parent.eq_any(&frontier))
                    .select((module_imports::module, module_imports::parent))
                    .load(conn)
                    .await?;
                frontier.clear();
                for (module, parent) in edges {
                    if !above.contains_key(&module) {
                        frontier.push(module.clone());
                    }
                    above.entry(module).or_default().push(parent);
                }
            }
            Ok(above)
        })
    })
    .await?;
    Ok(found)
}

/// The entries on `modules` with every label in `labels`, oldest first.
pub(crate) async fn entries_on(
    db: &Handle,
    v: &str,
    modules: &[String],
    labels: &[(String, String)],
) -> Result<Vec<(String, EntryMeta)>, Error> {
    let query = entries::table
        .filter(entries::version.eq(v))
        .filter(entries::module.eq_any(modules.to_vec()))
        .into_boxed();
    let rows = label_filter(query, labels)
        .order((entries::created_micros, entries::id))
        .select(ENTRY)
        .load::<EntryRow>(&mut *conn(db).await?)
        .await
        .map_err(DbError::from)?;
    Ok(rows
        .into_iter()
        .map(entry_from_row)
        .collect::<Result<_, _>>()?)
}

pub(crate) async fn set_labels(
    db: &Handle,
    v: &str,
    e: &str,
    set: &BTreeMap<String, String>,
    remove: &[String],
) -> Result<EntryMeta, Error> {
    let updated = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            let Some(mut meta) = entry_in(conn, v, e).await? else {
                return Err(Fail::Api(unknown_entry(e)));
            };
            for k in remove {
                meta.labels.remove(k);
            }
            meta.labels.extend(set.clone());
            check_labels(&meta.labels)
                .map_err(|m| Fail::Api(Error::new(ErrorKind::BadRequest, m)))?;
            diesel::update(entries::table.find((v, e)))
                .set(entries::labels.eq(to_record(&meta.labels)?))
                .execute(conn)
                .await?;
            log(conn, v, Some(e), Some(&meta.module)).await?;
            Ok(meta)
        })
    })
    .await?;
    Ok(updated)
}

pub(crate) async fn cleanup(db: &Handle, v: &str, m: &str) -> Result<Option<ModuleMeta>, Error> {
    let removed = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            let removed = clean(conn, v, m).await?;
            if removed.is_some() {
                log(conn, v, None, Some(m)).await?;
            }
            Ok(removed)
        })
    })
    .await?;
    Ok(removed)
}

pub(crate) async fn prune_log(db: &Handle, v: &str, older_than: Duration) -> Result<usize, Error> {
    let cutoff = now_micros().saturating_sub(older_than.as_micros() as u64) as i64;
    let deleted = retry(|| async {
        let deleted = diesel::delete(
            changes::table
                .filter(changes::version.eq(v))
                .filter(changes::logged_micros.lt(cutoff)),
        )
        .execute(&mut *conn(db).await?)
        .await?;
        Ok::<_, DbError>(deleted)
    })
    .await?;
    Ok(deleted)
}

pub(crate) async fn entries_after(
    db: &Handle,
    v: &str,
    after: Option<&str>,
    limit: i64,
) -> Result<Vec<(String, EntryMeta)>, Error> {
    let mut query = entries::table.filter(entries::version.eq(v)).into_boxed();
    if let Some(after) = after {
        query = query.filter(entries::id.gt(after));
    }
    let rows = query
        .order(entries::id)
        .limit(limit)
        .select(ENTRY)
        .load::<EntryRow>(&mut *conn(db).await?)
        .await
        .map_err(DbError::from)?;
    Ok(rows
        .into_iter()
        .map(entry_from_row)
        .collect::<Result<_, _>>()?)
}

pub(crate) async fn modules_with_decls_after(
    db: &Handle,
    v: &str,
    after: Option<&str>,
    limit: i64,
) -> Result<Vec<String>, Error> {
    let mut query = modules::table
        .filter(modules::version.eq(v))
        .filter(modules::decls.is_not_null())
        .into_boxed();
    if let Some(after) = after {
        query = query.filter(modules::id.gt(after));
    }
    let ids = query
        .order(modules::id)
        .limit(limit)
        .select(modules::id)
        .load(&mut *conn(db).await?)
        .await
        .map_err(DbError::from)?;
    Ok(ids)
}

pub(crate) async fn changes(
    db: &Handle,
    v: &str,
    since_micros: u64,
    after: Option<(u64, i64)>,
    limit: i64,
) -> Result<Vec<Logged>, Error> {
    let mut query = changes::table
        .filter(changes::version.eq(v))
        .filter(changes::logged_micros.ge(since_micros as i64))
        .into_boxed();
    if let Some((t, id)) = after {
        let t = t as i64;
        query = query.filter(
            changes::logged_micros
                .gt(t)
                .or(changes::logged_micros.eq(t).and(changes::id.gt(id))),
        );
    }
    let rows = query
        .order((changes::logged_micros, changes::id))
        .limit(limit)
        .select((
            changes::id,
            changes::logged_micros,
            changes::entry,
            changes::module,
        ))
        .load::<(i64, i64, Option<String>, Option<String>)>(&mut *conn(db).await?)
        .await
        .map_err(DbError::from)?;
    Ok(rows
        .into_iter()
        .map(|(id, logged, entry, module)| Logged {
            id,
            logged_micros: logged as u64,
            change: crate::catalog::Change { entry, module },
        })
        .collect())
}

pub(crate) async fn take_lease(
    db: &Handle,
    v: &str,
    name: &str,
    owner: &str,
    ttl: Duration,
) -> Result<Option<u64>, Error> {
    let taken = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            let now = now_micros();
            let held = leases::table
                .find((v, name))
                .select((leases::owner, leases::until_micros))
                .first::<(String, i64)>(conn)
                .await
                .optional()?;
            if let Some((held_by, until)) = held
                && held_by != owner
                && until as u64 > now
            {
                return Ok(None);
            }
            let until = now + ttl.as_micros() as u64;
            diesel::insert_into(leases::table)
                .values((
                    leases::version.eq(v),
                    leases::name.eq(name),
                    leases::owner.eq(owner),
                    leases::until_micros.eq(until as i64),
                ))
                .on_conflict((leases::version, leases::name))
                .do_update()
                .set((
                    leases::owner.eq(owner),
                    leases::until_micros.eq(until as i64),
                ))
                .execute(conn)
                .await?;
            Ok(Some(until))
        })
    })
    .await?;
    Ok(taken)
}

pub(crate) async fn has_entries(db: &Handle, v: &str, m: &str) -> Result<bool, Error> {
    let found = diesel::select(exists(
        entries::table
            .filter(entries::version.eq(v))
            .filter(entries::module.eq(m)),
    ))
    .get_result(&mut *conn(db).await?)
    .await
    .map_err(DbError::from)?;
    Ok(found)
}

pub(crate) async fn decls(db: &Handle, v: &str, m: &str) -> Result<Option<SnapshotDecls>, Error> {
    let decls = modules::table
        .find((v, m))
        .select(modules::decls)
        .first::<Option<Value>>(&mut *conn(db).await?)
        .await
        .optional()
        .map_err(DbError::from)?
        .flatten();
    Ok(decls
        .map(serde_json::from_value)
        .transpose()
        .map_err(Fail::from)?)
}

pub(crate) async fn described(
    db: &Handle,
    v: &str,
    m: &str,
) -> Result<Vec<(String, Value)>, Error> {
    let found = described::table
        .filter(described::version.eq(v))
        .filter(described::module.eq(m))
        .order(described::name)
        .select((described::name, described::record))
        .load(&mut *conn(db).await?)
        .await
        .map_err(DbError::from)?;
    Ok(found)
}

pub(crate) async fn put_described(
    db: &Handle,
    v: &str,
    m: &str,
    records: &[(String, Value)],
) -> Result<bool, Error> {
    let stored = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            if !module_exists(conn, v, m).await? {
                return Ok(false);
            }
            for (name, record) in records {
                diesel::insert_into(described::table)
                    .values((
                        described::version.eq(v),
                        described::module.eq(m),
                        described::name.eq(name),
                        described::record.eq(record),
                    ))
                    .on_conflict((described::version, described::module, described::name))
                    .do_update()
                    .set(described::record.eq(record))
                    .execute(conn)
                    .await?;
            }
            Ok(true)
        })
    })
    .await?;
    Ok(stored)
}
