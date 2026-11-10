// The snapshot catalog's queries, for `Catalog` (see `crate::catalog`).
// Included by `backend.rs` once per backend.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use diesel::dsl::{exists, not};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, OptionalExtension, QueryDsl,
};
use diesel_async::RunQueryDsl;
use pisa_protocol::api::{Error, ErrorKind, Replaces};
use pisa_protocol::labels::check_labels;
use pisa_protocol::snapshot;
use serde_json::Value;

use crate::catalog::{
    EntryMeta, EntryRow, Fail, LinkRow, Logged, Module, ModuleBody, ModuleMeta, ModuleSummary,
    NewEntry, SnapshotDecls, allow_column, closure_order, entry_from_row, now_micros, splice,
    status_of, status_text, to_record, unknown_entry, unknown_module,
};
use crate::db::{DbError, retry};
use crate::schema::{changes, described, entries, leases, links, module_imports, modules};

const ENTRY: (
    entries::id,
    entries::module,
    entries::labels,
    entries::idempotency_key,
    entries::created_micros,
    entries::holes,
) = (
    entries::id,
    entries::module,
    entries::labels,
    entries::idempotency_key,
    entries::created_micros,
    entries::holes,
);

const LINK: (links::entry, links::replaces, links::allow, links::replacement) = (
    links::entry,
    links::replaces,
    links::allow,
    links::replacement,
);

/// The ids a query takes at once, so that SQLite's limit on bound values is
/// never reached.
const REPLACED_CHUNK: usize = 1000;

/// The entries of `rows`, in order, with their links.
async fn with_links(
    conn: &mut Conn,
    v: &str,
    rows: Vec<EntryRow>,
) -> Result<Vec<(String, EntryMeta)>, Fail> {
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    let mut found: HashMap<String, LinkRow> = HashMap::new();
    for chunk in ids.chunks(REPLACED_CHUNK) {
        let links = links::table
            .filter(links::version.eq(v))
            .filter(links::entry.eq_any(chunk))
            .select(LINK)
            .load::<LinkRow>(conn)
            .await?;
        found.extend(links.into_iter().map(|l| (l.0.clone(), l)));
    }
    rows.into_iter()
        .map(|row| {
            let link = found.remove(&row.0);
            entry_from_row(row, link)
        })
        .collect()
}

/// The entries with ids `ids`, oldest first, with their links.
async fn entries_by_id(
    conn: &mut Conn,
    v: &str,
    ids: &[String],
) -> Result<Vec<(String, EntryMeta)>, Fail> {
    let mut rows = Vec::new();
    for chunk in ids.chunks(REPLACED_CHUNK) {
        rows.extend(
            entries::table
                .filter(entries::version.eq(v))
                .filter(entries::id.eq_any(chunk))
                .select(ENTRY)
                .load::<EntryRow>(conn)
                .await?,
        );
    }
    rows.sort_by(|a, b| (a.4, &a.0).cmp(&(b.4, &b.0)));
    with_links(conn, v, rows).await
}

/// An import, as the closure query gives it.
#[derive(diesel::QueryableByName)]
struct Edge {
    #[diesel(sql_type = diesel::sql_types::Text)]
    module: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    parent: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    position: i32,
}

/// A module's columns: its build, files, header, status, whether it is
/// verified and when it was built.
type ModuleRow = (String, String, Value, String, String, bool, i64);

const MODULE: (
    modules::id,
    modules::build,
    modules::files,
    modules::header,
    modules::status,
    modules::verified,
    modules::created,
) = (
    modules::id,
    modules::build,
    modules::files,
    modules::header,
    modules::status,
    modules::verified,
    modules::created,
);

/// The modules `m` needs, its own last, each after what it imports, by the
/// imports of what it imports: one query for the imports, and one for the
/// modules' builds.
async fn closure_in(conn: &mut Conn, v: &str, m: &str) -> Result<Vec<Module>, Fail> {
    let edges: Vec<Edge> = diesel::sql_query(CLOSURE_SQL)
        .bind::<diesel::sql_types::Text, _>(v)
        .bind::<diesel::sql_types::Text, _>(m)
        .load(conn)
        .await?;
    let mut imports: HashMap<String, Vec<(i32, String)>> = HashMap::new();
    for e in edges {
        imports.entry(e.module).or_default().push((e.position, e.parent));
    }
    let imports: HashMap<String, Vec<String>> = imports
        .into_iter()
        .map(|(module, mut parents)| {
            parents.sort();
            (module, parents.into_iter().map(|(_, p)| p).collect())
        })
        .collect();
    let order = closure_order(m, &imports);
    let mut builds: HashMap<String, Module> = HashMap::new();
    for chunk in order.chunks(REPLACED_CHUNK) {
        let rows = modules::table
            .filter(modules::version.eq(v))
            .filter(modules::id.eq_any(chunk))
            .select(MODULE)
            .load::<ModuleRow>(conn)
            .await?;
        for (id, build, files, ..) in rows {
            let files = serde_json::from_value(files)?;
            builds.insert(id.clone(), Module { id, build, files });
        }
    }
    order
        .iter()
        .map(|id| builds.remove(id).ok_or_else(|| Fail::Api(unknown_module(id))))
        .collect()
}

/// The imports of each of `ms`, in the header's order.
async fn imports_in(
    conn: &mut Conn,
    v: &str,
    ms: &[String],
) -> Result<HashMap<String, Vec<String>>, Fail> {
    let mut found: HashMap<String, Vec<(i32, String)>> = HashMap::new();
    for chunk in ms.chunks(REPLACED_CHUNK) {
        let rows: Vec<(String, String, i32)> = module_imports::table
            .filter(module_imports::version.eq(v))
            .filter(module_imports::module.eq_any(chunk))
            .select((module_imports::module, module_imports::parent, module_imports::position))
            .load(conn)
            .await?;
        for (module, parent, position) in rows {
            found.entry(module).or_default().push((position, parent));
        }
    }
    Ok(found
        .into_iter()
        .map(|(module, mut parents)| {
            parents.sort();
            (module, parents.into_iter().map(|(_, p)| p).collect())
        })
        .collect())
}

async fn module_in(conn: &mut Conn, v: &str, m: &str) -> Result<Option<ModuleMeta>, Fail> {
    let found = modules::table
        .find((v, m))
        .select((MODULE, modules::meta))
        .first::<(ModuleRow, Value)>(conn)
        .await
        .optional()?;
    let Some(((_, _, _, header, status, verified, created), body)) = found else {
        return Ok(None);
    };
    let body: ModuleBody = serde_json::from_value(body)?;
    let imports = imports_in(conn, v, &[m.to_string()]).await?.remove(m).unwrap_or_default();
    Ok(Some(ModuleMeta {
        imports,
        modules: closure_in(conn, v, m).await?,
        header,
        code: body.code,
        status: status_of(&status)?,
        verified,
        macros: body.macros,
        result: body.result,
        created: created as u64,
    }))
}

/// What a listing shows of each of `ms` that is kept.
async fn summaries_in(
    conn: &mut Conn,
    v: &str,
    ms: &[String],
) -> Result<HashMap<String, ModuleSummary>, Fail> {
    let mut imports = imports_in(conn, v, ms).await?;
    let mut found = HashMap::new();
    for chunk in ms.chunks(REPLACED_CHUNK) {
        let rows = modules::table
            .filter(modules::version.eq(v))
            .filter(modules::id.eq_any(chunk))
            .select(MODULE)
            .load::<ModuleRow>(conn)
            .await?;
        for (id, _, _, header, status, verified, created) in rows {
            let summary = ModuleSummary {
                imports: imports.remove(&id).unwrap_or_default(),
                header,
                status: status_of(&status)?,
                verified,
                created: created as u64,
            };
            found.insert(id, summary);
        }
    }
    Ok(found)
}

/// Writes module `m`'s columns and body as `meta` has them.
async fn write_module(
    conn: &mut Conn,
    v: &str,
    m: &str,
    meta: &ModuleMeta,
    decls: Option<&Value>,
) -> Result<(), Fail> {
    let own = meta
        .modules
        .last()
        .ok_or_else(|| Fail::Api(unknown_module(m)))?;
    let values = (
        modules::meta.eq(to_record(&ModuleBody::of(meta))?),
        modules::decls.eq(decls),
        modules::build.eq(&own.build),
        modules::files.eq(serde_json::to_value(&own.files)?),
        modules::header.eq(&meta.header),
        modules::status.eq(status_text(meta.status)?),
        modules::verified.eq(meta.verified),
        modules::created.eq(meta.created as i64),
    );
    diesel::insert_into(modules::table)
        .values((modules::version.eq(v), modules::id.eq(m), values.clone()))
        .on_conflict((modules::version, modules::id))
        .do_update()
        .set(values)
        .execute(conn)
        .await?;
    Ok(())
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
    let found = with_links(conn, v, row.into_iter().collect()).await?;
    Ok(found.into_iter().next().map(|(_, meta)| meta))
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
    Ok(with_links(conn, v, row.into_iter().collect()).await?.pop())
}

/// The entries whose link replaces `e`, oldest first.
async fn replacers_in(conn: &mut Conn, v: &str, e: &str) -> Result<Vec<(String, EntryMeta)>, Fail> {
    let ids: Vec<String> = links::table
        .filter(links::version.eq(v))
        .filter(links::replaces.eq(e))
        .select(links::entry)
        .load(conn)
        .await?;
    entries_by_id(conn, v, &ids).await
}

/// Fails with `not_found` if `entry` replaces an entry that is gone, and
/// with `conflict` if another entry replaces it already and `entry` doesn't
/// fork.
async fn check_replaced(conn: &mut Conn, v: &str, entry: &NewEntry) -> Result<(), Fail> {
    let Some(replaces) = &entry.replaces else {
        return Ok(());
    };
    let kept: bool = diesel::select(exists(entries::table.find((v, &replaces.snapshot_id))))
        .get_result(conn)
        .await?;
    if !kept {
        return Err(Fail::Api(Error::new(
            ErrorKind::NotFound,
            "the replaced snapshot was deleted while this one was being built".to_string(),
        )));
    }
    if entry.fork {
        return Ok(());
    }
    let replaced: bool = diesel::select(exists(
        links::table
            .filter(links::version.eq(v))
            .filter(links::replaces.eq(&replaces.snapshot_id)),
    ))
    .get_result(conn)
    .await?;
    if replaced {
        return Err(Fail::Api(Error::new(
            ErrorKind::Conflict,
            "the replaced snapshot was replaced while this one was being built".to_string(),
        )));
    }
    Ok(())
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

/// Logs the entry that `link` names, with its module, if it is kept: a link
/// that the transaction adds or removes makes it a head or stops it being one.
async fn log_replaced(conn: &mut Conn, v: &str, link: Option<&Replaces>) -> Result<(), Fail> {
    let Some(link) = link else {
        return Ok(());
    };
    if let Some(meta) = entry_in(conn, v, &link.snapshot_id).await? {
        log(conn, v, Some(&link.snapshot_id), Some(&meta.module)).await?;
    }
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
        replacement: entry.replacement.clone(),
        holes: entry.holes.clone(),
    };
    let holes = (!meta.holes.is_empty())
        .then(|| serde_json::to_string(&meta.holes))
        .transpose()?;
    diesel::insert_into(entries::table)
        .values((
            entries::version.eq(v),
            entries::id.eq(&e),
            entries::module.eq(m),
            entries::labels.eq(to_record(&meta.labels)?),
            entries::idempotency_key.eq(meta.key.as_deref()),
            entries::created_micros.eq(meta.created_micros as i64),
            entries::holes.eq(holes),
        ))
        .execute(conn)
        .await?;
    if let Some(replaced) = &meta.replaces {
        // The run starts where the replaced entry's starts, or at it.
        let root: Option<String> = links::table
            .find((v, &replaced.snapshot_id))
            .select(links::run_root)
            .first(conn)
            .await
            .optional()?;
        let replacement = meta.replacement.clone().unwrap_or_default();
        diesel::insert_into(links::table)
            .values((
                links::version.eq(v),
                links::entry.eq(&e),
                links::replaces.eq(&replaced.snapshot_id),
                links::allow.eq(allow_column(replaced)?),
                links::replacement.eq(serde_json::to_string(&replacement)?),
                links::run_root.eq(root.as_deref().unwrap_or(&replaced.snapshot_id)),
            ))
            .execute(conn)
            .await?;
    }
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

pub(crate) async fn summaries(
    db: &Handle,
    v: &str,
    ms: &[String],
) -> Result<HashMap<String, ModuleSummary>, Error> {
    let mut c = conn(db).await?;
    Ok(summaries_in(&mut c, v, ms).await?)
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
            let Some(mut meta) = module_in(conn, v, m).await? else {
                return Err(Fail::Api(unknown_module(m)));
            };
            let decls: Option<Value> = modules::table
                .find((v, m))
                .select(modules::decls)
                .first(conn)
                .await?;
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
            write_module(conn, v, m, &meta, decls.as_ref()).await?;
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
            check_replaced(conn, v, entry).await?;
            // Each import is still the build the module was built on: a
            // module that another imports can't go, so its own imports can't
            // have changed either.
            for parent in &meta.imports {
                let Some(build): Option<String> = modules::table
                    .find((v, parent))
                    .select(modules::build)
                    .first(conn)
                    .await
                    .optional()?
                else {
                    return Err(Fail::Api(unknown_module(parent)));
                };
                let built_on = meta.modules.iter().find(|x| x.id == *parent);
                if built_on.map(|x| &x.build) != Some(&build) {
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
                    write_module(conn, v, m, meta, decls).await?;
                    for (position, parent) in meta.imports.iter().enumerate() {
                        diesel::insert_into(module_imports::table)
                            .values((
                                module_imports::version.eq(v),
                                module_imports::module.eq(m),
                                module_imports::parent.eq(parent),
                                module_imports::position.eq(position as i32),
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
            log_replaced(conn, v, entry.replaces.as_ref()).await?;
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
            check_replaced(conn, v, entry).await?;
            if !module_exists(conn, v, m).await? {
                return Ok(None);
            }
            let added = add(conn, v, m, entry).await?;
            log(conn, v, Some(&added.0), Some(m)).await?;
            log_replaced(conn, v, entry.replaces.as_ref()).await?;
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
                return Err(Fail::Api(unknown_entry(v, e)));
            };
            let mut repointed = Vec::new();
            // Load-bearing even for a root: under SERIALIZABLE this read makes
            // the delete conflict with a create replacing the entry.
            for (r, replacer) in replacers_in(conn, v, e).await? {
                // A root has no link to give: what replaces it keeps its own.
                let Some((replaces, replacement)) = splice(&meta, &replacer) else {
                    break;
                };
                // The run's root stays: the link now skips `e`.
                diesel::update(links::table.find((v, &r)))
                    .set((
                        links::replaces.eq(&replaces.snapshot_id),
                        links::allow.eq(allow_column(&replaces)?),
                        links::replacement.eq(serde_json::to_string(&replacement)?),
                    ))
                    .execute(conn)
                    .await?;
                repointed.push((r, replacer.module));
            }
            diesel::delete(entries::table.find((v, e)))
                .execute(conn)
                .await?;
            let module = clean(conn, v, &meta.module).await?;
            log(conn, v, Some(e), Some(&meta.module)).await?;
            log_replaced(conn, v, meta.replaces.as_ref()).await?;
            for (r, module) in &repointed {
                log(conn, v, Some(r), Some(module)).await?;
            }
            Ok((meta, module))
        })
    })
    .await?;
    Ok(removed)
}

pub(crate) async fn remove_branch(
    db: &Handle,
    v: &str,
    e: &str,
) -> Result<(Vec<String>, Vec<(String, ModuleMeta)>), Error> {
    let removed = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            let Some(first) = entry_in(conn, v, e).await? else {
                return Err(Fail::Api(unknown_entry(v, e)));
            };
            // The branch: `e`, then what replaces each entry found, with the
            // module of each.
            let mut ids = vec![e.to_string()];
            let mut owners = vec![first.module];
            let mut next = 0;
            while next < ids.len() {
                for (r, meta) in replacers_in(conn, v, &ids[next]).await? {
                    ids.push(r);
                    owners.push(meta.module);
                }
                next += 1;
            }
            diesel::delete(
                entries::table
                    .filter(entries::version.eq(v))
                    .filter(entries::id.eq_any(&ids)),
            )
            .execute(conn)
            .await?;
            let mut modules: Vec<String> = Vec::new();
            for (entry, module) in ids.iter().zip(owners) {
                log(conn, v, Some(entry), Some(&module)).await?;
                if !modules.contains(&module) {
                    modules.push(module);
                }
            }
            log_replaced(conn, v, first.replaces.as_ref()).await?;
            // A module goes once the modules that import it have, so go round
            // until a round cleans none.
            let mut cleaned = Vec::new();
            while !modules.is_empty() {
                let mut kept = Vec::new();
                for module in &modules {
                    match clean(conn, v, module).await? {
                        Some(meta) => cleaned.push((module.clone(), meta)),
                        None => kept.push(module.clone()),
                    }
                }
                if kept.len() == modules.len() {
                    break;
                }
                modules = kept;
            }
            Ok((ids, cleaned))
        })
    })
    .await?;
    Ok(removed)
}

/// Up to `limit` entries, newest first, with every label in `labels`, from
/// before the entry created at `after`'s time with `after`'s id. With
/// `heads`, only those that no entry replaces.
pub(crate) async fn list(
    db: &Handle,
    v: &str,
    labels: &[(String, String)],
    limit: i64,
    after: Option<(i64, &str)>,
    heads: bool,
) -> Result<Vec<(String, EntryMeta)>, Error> {
    let mut c = conn(db).await?;
    let mut query = entries::table.filter(entries::version.eq(v)).into_boxed();
    if heads {
        query = query.filter(not(exists(
            links::table.filter(
                links::version
                    .eq(entries::version)
                    .and(links::replaces.eq(entries::id)),
            ),
        )));
    }
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
        .load::<EntryRow>(&mut *c)
        .await
        .map_err(DbError::from)?;
    Ok(with_links(&mut c, v, rows).await?)
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

pub(crate) async fn replacers(
    db: &Handle,
    v: &str,
    e: &str,
) -> Result<Vec<(String, EntryMeta)>, Error> {
    Ok(replacers_in(&mut *conn(db).await?, v, e).await?)
}

/// Every entry of the runs that `es` are on, with its link, in one read:
/// the run of an entry with a link is its link's root's, and an entry
/// without one starts a run.
pub(crate) async fn runs(
    db: &Handle,
    v: &str,
    es: &[String],
) -> Result<HashMap<String, EntryMeta>, Error> {
    let found = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        read_tx!(conn, {
            let mut roots: BTreeSet<String> = es.iter().cloned().collect();
            for chunk in es.chunks(REPLACED_CHUNK) {
                let linked: Vec<String> = links::table
                    .filter(links::version.eq(v))
                    .filter(links::entry.eq_any(chunk))
                    .select(links::run_root)
                    .load(conn)
                    .await?;
                roots.extend(linked);
            }
            let roots: Vec<String> = roots.into_iter().collect();
            let mut members: BTreeSet<String> = roots.iter().cloned().collect();
            for chunk in roots.chunks(REPLACED_CHUNK) {
                let linked: Vec<String> = links::table
                    .filter(links::version.eq(v))
                    .filter(links::run_root.eq_any(chunk))
                    .select(links::entry)
                    .load(conn)
                    .await?;
                members.extend(linked);
            }
            let members: Vec<String> = members.into_iter().collect();
            Ok(entries_by_id(conn, v, &members).await?.into_iter().collect())
        })
    })
    .await?;
    Ok(found)
}

pub(crate) async fn replacers_among(
    db: &Handle,
    v: &str,
    es: &[String],
) -> Result<HashMap<String, Vec<(String, EntryMeta)>>, Error> {
    let mut found: HashMap<String, Vec<(String, EntryMeta)>> = HashMap::new();
    let mut c = conn(db).await?;
    let mut replacers: Vec<String> = Vec::new();
    for chunk in es.chunks(REPLACED_CHUNK) {
        let ids: Vec<String> = links::table
            .filter(links::version.eq(v))
            .filter(links::replaces.eq_any(chunk))
            .select(links::entry)
            .load(&mut *c)
            .await
            .map_err(DbError::from)?;
        replacers.extend(ids);
    }
    for (e, meta) in entries_by_id(&mut c, v, &replacers).await? {
        if let Some(replaced) = &meta.replaces {
            found
                .entry(replaced.snapshot_id.clone())
                .or_default()
                .push((e, meta));
        }
    }
    Ok(found)
}

/// The entries on `modules` with every label in `labels`, oldest first.
pub(crate) async fn entries_on(
    db: &Handle,
    v: &str,
    modules: &[String],
    labels: &[(String, String)],
) -> Result<Vec<(String, EntryMeta)>, Error> {
    let mut c = conn(db).await?;
    let query = entries::table
        .filter(entries::version.eq(v))
        .filter(entries::module.eq_any(modules.to_vec()))
        .into_boxed();
    let rows = label_filter(query, labels)
        .order((entries::created_micros, entries::id))
        .select(ENTRY)
        .load::<EntryRow>(&mut *c)
        .await
        .map_err(DbError::from)?;
    Ok(with_links(&mut c, v, rows).await?)
}

pub(crate) async fn set_labels(
    db: &Handle,
    v: &str,
    e: &str,
    labels: &BTreeMap<String, Option<String>>,
) -> Result<EntryMeta, Error> {
    let updated = retry(|| async {
        let mut c = conn(db).await?;
        let conn = &mut *c;
        tx!(conn, {
            let Some(mut meta) = entry_in(conn, v, e).await? else {
                return Err(Fail::Api(unknown_entry(v, e)));
            };
            for (key, value) in labels {
                match value {
                    Some(value) => meta.labels.insert(key.clone(), value.clone()),
                    None => meta.labels.remove(key),
                };
            }
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
    let mut c = conn(db).await?;
    let mut query = entries::table.filter(entries::version.eq(v)).into_boxed();
    if let Some(after) = after {
        query = query.filter(entries::id.gt(after));
    }
    let rows = query
        .order(entries::id)
        .limit(limit)
        .select(ENTRY)
        .load::<EntryRow>(&mut *c)
        .await
        .map_err(DbError::from)?;
    Ok(with_links(&mut c, v, rows).await?)
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

/// Whether an entry on `m` is a head: one that no entry replaces.
pub(crate) async fn has_heads(db: &Handle, v: &str, m: &str) -> Result<bool, Error> {
    let found = diesel::select(exists(
        entries::table
            .filter(entries::version.eq(v))
            .filter(entries::module.eq(m))
            .filter(not(exists(
                links::table.filter(
                    links::version
                        .eq(entries::version)
                        .and(links::replaces.eq(entries::id)),
                ),
            ))),
    ))
    .get_result(&mut *conn(db).await?)
    .await
    .map_err(DbError::from)?;
    Ok(found)
}

/// Those of `es` that an entry replaces.
pub(crate) async fn replaced_among(
    db: &Handle,
    v: &str,
    es: &[String],
) -> Result<HashSet<String>, Error> {
    let mut replaced = HashSet::new();
    let mut c = conn(db).await?;
    for chunk in es.chunks(REPLACED_CHUNK) {
        let found: Vec<String> = links::table
            .filter(links::version.eq(v))
            .filter(links::replaces.eq_any(chunk))
            .select(links::replaces)
            .distinct()
            .load(&mut *c)
            .await
            .map_err(DbError::from)?;
        replaced.extend(found);
    }
    Ok(replaced)
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
