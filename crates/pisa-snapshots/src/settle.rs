//! Planning a settle (`POST /snapshots/settle`): the rebuild of a run's heads
//! whose targets hold only through links, onto the heads they are read
//! through, in one plan. Where a rebuild swaps one module for another, a
//! settle swaps several at once: each module the audit read a theorem of
//! through links, for the head of that module's run. A swapped module is
//! never rebuilt, a swap's head that builds on another swapped module is
//! copied first, and what is between a head and a swapped module is rebuilt
//! as a support item, as a rebuild does.

use pisa_db::catalog::ModuleRecord;

use super::*;

impl Reader {
    /// Plans the settle of the run that the spec snapshots `against` start,
    /// audited under `allow`; see [`RebuildSnapshotsResponse`]. Nothing to
    /// settle is an empty plan; a head the audit says can't be settled, or
    /// swaps that would make a cycle, refuse the plan, with why in
    /// `changes`.
    pub async fn settle_plan(
        &self,
        against: &[String],
        allow: &[String],
    ) -> Result<RebuildSnapshotsResponse, Error> {
        let refuse = |changes: Vec<String>| RebuildSnapshotsResponse {
            ok: false,
            changes,
            ..Default::default()
        };
        let unsettled = self.audit_run(against, allow).await?.audit.unsettled;
        let blocked: Vec<String> = (unsettled.iter())
            .flat_map(|u| u.blockers.iter().map(move |b| format!("{}: {b}", u.head)))
            .collect();
        if !blocked.is_empty() {
            return Ok(refuse(blocked));
        }
        if unsettled.is_empty() {
            return Ok(RebuildSnapshotsResponse {
                ok: true,
                ..Default::default()
            });
        }

        // Which module each swap takes the place of, and the heads to copy.
        let mut swaps: BTreeMap<String, String> = BTreeMap::new();
        let mut copies: HashMap<String, Vec<(String, EntryMeta)>> = HashMap::new();
        for u in &unsettled {
            let e = self.parse(&u.head)?;
            let meta = self.catalog.entry(&e).await?;
            copies
                .entry(meta.module.clone())
                .or_default()
                .push((e, meta));
            for s in &u.swaps {
                let old = self
                    .catalog
                    .entry(&self.parse(&s.snapshot_id)?)
                    .await?
                    .module;
                let new = self.catalog.entry(&self.parse(&s.head)?).await?.module;
                if swaps
                    .insert(old.clone(), new.clone())
                    .is_some_and(|n| n != new)
                {
                    return Ok(refuse(vec![format!(
                        "{} would be swapped for two heads",
                        snapshot::module(&old)
                    )]));
                }
            }
        }
        // What builds on each swapped module, with its imports that lead to
        // one.
        let mut above: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for root in swaps.keys() {
            for (m, imports) in self.catalog.dependents(root).await? {
                above.entry(m).or_default().extend(imports);
            }
        }
        // What to rebuild: the heads, down to the swapped modules, and each
        // swap's head that builds on another.
        let mut rebuild: BTreeSet<String> = BTreeSet::new();
        let mut todo: Vec<String> = copies.keys().cloned().collect();
        while let Some(m) = todo.pop() {
            if swaps.contains_key(&m) || !above.contains_key(&m) || !rebuild.insert(m.clone()) {
                continue;
            }
            for i in &above[&m] {
                todo.push(swaps.get(i).unwrap_or(i).clone());
            }
        }
        // Each module after what it needs: its rebuilt imports, and the
        // copied heads that take the place of its swapped imports.
        let needs = |m: &String| -> Vec<String> {
            (above[m].iter())
                .map(|i| swaps.get(i).unwrap_or(i))
                .filter(|i| rebuild.contains(*i))
                .cloned()
                .collect()
        };
        let mut order: Vec<String> = Vec::new();
        let mut placed: HashSet<String> = HashSet::new();
        while order.len() < rebuild.len() {
            let ready: Vec<String> = (rebuild.iter())
                .filter(|m| !placed.contains(*m) && needs(m).iter().all(|n| placed.contains(n)))
                .cloned()
                .collect();
            if ready.is_empty() {
                let stuck = (rebuild.iter())
                    .filter(|m| !placed.contains(*m))
                    .map(|m| snapshot::module(m))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Ok(refuse(vec![format!(
                    "the swaps make a cycle through {stuck}: settle its runs one at a time"
                )]));
            }
            placed.extend(ready.iter().cloned());
            order.extend(ready);
        }
        if order.len() > MAX_REBUILD_ITEMS {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("the settle would rebuild {} modules", order.len()),
            ));
        }

        // The new name of each module the plan swaps or copies.
        let mut renamed: BTreeMap<String, String> = swaps.clone();
        // Each copy's original, whose declarations it is taken to have.
        let mut originals: HashMap<String, String> = HashMap::new();
        // What a copy builds on through each module it imports, as its create
        // will find it, as for a rebuild.
        let mut through: HashMap<String, Vec<String>> = HashMap::new();
        let mut records = Records::default();
        records.load(&self.catalog, &order).await?;
        records.load_closures(&self.catalog, &order).await?;
        let mut planned: Vec<Planned> = Vec::new();
        for m in order {
            let meta = records.kept(&self.catalog, &m).await?.clone();
            let code = snapshot::rewrite_imports(&meta.code, &renamed);
            let id = snapshot::id(&code);
            let header = snapshot::next_header(&split_header(&code).header, &id);
            // A swap onto this module now swaps onto its copy.
            for target in renamed.values_mut().filter(|t| **t == m) {
                *target = id.clone();
            }
            renamed.insert(m.clone(), id.clone());
            originals.insert(id.clone(), m.clone());
            let mut needs_now: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for i in &meta.imports {
                let now = renamed.get(i).cloned().unwrap_or_else(|| i.clone());
                if !through.contains_key(&now) {
                    records.kept(&self.catalog, &now).await?;
                    let kept = records.closure(&self.catalog, &now).await?;
                    through.insert(now.clone(), kept);
                }
                for n in &through[&now] {
                    if seen.insert(n.clone()) {
                        needs_now.push(n.clone());
                    }
                }
            }
            needs_now.push(id.clone());
            through.insert(id.clone(), needs_now.clone());
            let item = |entry: Option<&str>, labels, replaces, source| RebuildItem {
                level: 0,
                key: format!("settle:{}:{id}", entry.unwrap_or(&m)),
                replaces,
                snapshot_id: entry.map(|e| self.id(e)),
                module: snapshot::module(&m),
                new_module: snapshot::module(&id),
                new_header: header.clone(),
                code: code.clone(),
                labels,
                affected: Vec::new(),
                source,
            };
            let Some(entries) = copies.remove(&m) else {
                planned.push(Planned {
                    created: meta.created * 1_000_000,
                    module: m.clone(),
                    item: item(None, BTreeMap::new(), None, None),
                });
                continue;
            };
            let needs_before: Vec<String> = records
                .closure(&self.catalog, &m)
                .await?
                .into_iter()
                .filter(|n| *n != m)
                .collect();
            for (e, entry_meta) in entries {
                let replaces = self
                    .copy_link(
                        &e,
                        &entry_meta,
                        &meta,
                        &m,
                        &needs_before,
                        &needs_now,
                        &id,
                        &originals,
                        &mut records,
                    )
                    .await?;
                planned.push(Planned {
                    created: entry_meta.created_micros,
                    module: m.clone(),
                    item: item(
                        Some(&e),
                        entry_meta.labels,
                        replaces,
                        // The text as written, its names kept, its hashes
                        // swapped.
                        (entry_meta.source.as_deref())
                            .map(|s| snapshot::rewrite_imports(s, &renamed)),
                    ),
                });
            }
        }

        // Levels from what each copy imports among the copies.
        let by_module: HashMap<String, usize> = (planned.iter().enumerate())
            .map(|(i, p)| (p.item.new_module.clone(), i))
            .collect();
        let imports: Vec<Vec<usize>> = (planned.iter())
            .map(|p| {
                let header = split_header(&p.item.code).header;
                (snapshot::imports(&header).unwrap_or_default().iter())
                    .filter_map(|id| by_module.get(&snapshot::module(id)).copied())
                    .collect()
            })
            .collect();
        for (p, wave) in planned.iter_mut().zip(waves(&imports)) {
            p.item.level = wave;
        }
        planned.sort_by_key(|p| (p.item.level, p.created));
        Ok(RebuildSnapshotsResponse {
            ok: true,
            plan: planned.into_iter().map(|p| p.item).collect(),
            ..Default::default()
        })
    }

    /// The link of the copy of head entry `e` on module `m`, whose record is
    /// `meta`: it replaces `e`, allowing what the swaps changed of what it
    /// builds on, as a rebuild's copy does, if the comparison its create
    /// will run passes.
    #[allow(clippy::too_many_arguments)]
    async fn copy_link(
        &self,
        e: &str,
        entry_meta: &EntryMeta,
        meta: &ModuleRecord,
        m: &str,
        needs_before: &[String],
        needs_now: &[String],
        id: &str,
        originals: &HashMap<String, String>,
        records: &mut Records,
    ) -> Result<Option<Replaces>, Error> {
        let mut copied = Replacing {
            replaced: Replaces {
                snapshot_id: self.id(e),
                allow: None,
            },
            entry: e.to_string(),
            module: m.to_string(),
            declarations: meta.result.declarations.clone(),
            needs: needs_before.to_vec(),
            holes: entry_meta.holes.clone(),
        };
        let swapped = self
            .predicted_swap(&copied, needs_now, id, originals, records)
            .await?;
        let definitions: Vec<String> = allowable(&copied, &swapped)
            .map(|d| d.name.clone())
            .collect();
        copied.replaced.allow = (!definitions.is_empty()).then(|| Allow {
            definitions: definitions.clone(),
        });
        Ok(compare(&copied, &copied.declarations, &swapped)
            .ok()
            .map(|r| {
                let named: Vec<String> = (r.changed_definitions.into_iter())
                    .filter(|n| definitions.contains(n))
                    .collect();
                Replaces {
                    snapshot_id: copied.replaced.snapshot_id.clone(),
                    allow: (!named.is_empty()).then_some(Allow { definitions: named }),
                }
            }))
    }
}
