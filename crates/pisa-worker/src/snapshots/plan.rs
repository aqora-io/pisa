//! Planning a rebuild of the dependents of a snapshot on its replacement
//! (`POST /snapshots/rebuild`).

use super::*;

/// The modules of `above` (see [`Catalog::dependents`]) to rebuild for
/// `matching`: those, and the ones among `above` they import, directly or
/// through others.
pub(super) fn needed(
    above: &BTreeMap<String, Vec<String>>,
    matching: impl IntoIterator<Item = String>,
) -> BTreeSet<String> {
    let mut needed = BTreeSet::new();
    let mut todo: Vec<String> = matching.into_iter().collect();
    while let Some(m) = todo.pop() {
        if let Some(imports) = above.get(&m)
            && needed.insert(m)
        {
            todo.extend(imports.iter().cloned());
        }
    }
    needed
}

/// The level of each module in `above` (see [`Catalog::dependents`]) over
/// `root`, which is 0: one more than the highest of its imports among them.
pub(super) fn levels(above: &BTreeMap<String, Vec<String>>, root: &str) -> HashMap<String, usize> {
    let mut levels = HashMap::from([(root.to_string(), 0)]);
    while levels.len() <= above.len() {
        let before = levels.len();
        for (m, imports) in above {
            if levels.contains_key(m) {
                continue;
            }
            let known: Option<Vec<usize>> =
                imports.iter().map(|i| levels.get(i).copied()).collect();
            if let Some(known) = known {
                levels.insert(m.clone(), 1 + known.into_iter().max().unwrap_or(0));
            }
        }
        // Imports always lead down to `root`, so each pass places some.
        if levels.len() == before {
            break;
        }
    }
    levels
}

/// A rebuild item before its level: when its snapshot was made (or its
/// module, for an item without one), and its old module.
pub(super) struct Planned {
    pub(super) created: u64,
    pub(super) module: String,
    pub(super) item: RebuildItem,
}

/// The level of each item of a rebuild plan (see [`RebuildItem::level`]):
/// one more than the highest among the items on the modules it imports,
/// `imports[i]`.
pub(super) fn waves(imports: &[Vec<usize>]) -> Vec<usize> {
    let mut waves: Vec<Option<usize>> = vec![None; imports.len()];
    // Imports always lead down to `from`, so each pass places some.
    while waves.contains(&None) {
        for i in 0..waves.len() {
            if waves[i].is_some() {
                continue;
            }
            let below = imports[i].iter().map(|&j| waves[j]);
            if let Some(known) = below.collect::<Option<Vec<usize>>>() {
                waves[i] = Some(1 + known.into_iter().max().unwrap_or(0));
            }
        }
    }
    waves.into_iter().flatten().collect()
}

impl Snapshots {
    /// Plans the creates that rebuild `job.from`'s dependents on `job.to`;
    /// see [`RebuildSnapshotsResponse`].
    pub async fn rebuild_plan(
        &self,
        job: &SnapshotRebuildJob,
    ) -> Result<RebuildSnapshotsResponse, Error> {
        let bad = |m: &str| Error::new(ErrorKind::BadRequest, m);
        let (from, to) = (self.parse(&job.from)?, self.parse(&job.to)?);
        if job.labels.is_empty() {
            return Err(bad("a rebuild takes labels: which dependents to rebuild"));
        }
        check_labels(&job.labels).map_err(|e| bad(&e))?;
        let (from_entry, to_entry) = (
            self.catalog.entry(&from).await?,
            self.catalog.entry(&to).await?,
        );
        let (old, new) = (from_entry.module, to_entry.module);
        if old == new {
            return Err(bad(
                "from and to are on the same module: there is nothing to rebuild",
            ));
        }
        let (old_meta, new_meta) = (
            self.catalog.module(&old).await?,
            self.catalog.module(&new).await?,
        );
        if ids(&new_meta.modules).any(|m| *m == old) {
            return Err(bad(
                "to builds on from's module, so it can't take its place",
            ));
        }

        let now = &new_meta.result.declarations;
        // `to` compared as a replacement of `from` would be.
        let replacing = |allow| Replacing {
            replaced: Replaces {
                snapshot_id: job.from.clone(),
                allow,
            },
            entry: from.clone(),
            module: old.clone(),
            declarations: old_meta.result.declarations.clone(),
            needs: ids(&old_meta.modules)
                .filter(|m| **m != old)
                .cloned()
                .collect(),
            holes: from_entry.holes.clone(),
        };
        let strict = replacing(None);
        let needs: Vec<String> = ids(&new_meta.modules).cloned().collect();
        let swapped = self.swapped(&strict, &needs, &new).await?;
        // A replacement's create compared them already.
        let link = to_entry.replaces.filter(|r| r.snapshot_id == from);
        let changes = match link {
            Some(_) => Vec::new(),
            None => compare(&strict, now, &swapped)
                .err()
                .unwrap_or_default()
                .iter()
                .map(ToString::to_string)
                .collect(),
        };
        // Under its own `allow` if `to` replaces `from`, else any definition.
        let allow = match &link {
            Some(link) => link.allow.clone(),
            None => Some(Allow {
                definitions: allowable(&strict, &swapped)
                    .map(|d| d.name.clone())
                    .collect(),
            }),
        };
        let compared = compare(&replacing(allow), now, &swapped);
        let changed_definitions = match compared {
            Ok(r) => r.changed_definitions,
            // What else changed is in `changes`.
            Err(_) => statement_changes(&old_meta.result.declarations, now)
                .into_iter()
                .filter(|c| c.change == Change::Value)
                .map(|c| c.name)
                .collect(),
        };
        if !changes.is_empty() && !job.allow_changes {
            return Ok(RebuildSnapshotsResponse {
                ok: false,
                changes,
                changed_definitions,
                ..Default::default()
            });
        }

        let above = self.catalog.dependents(&old).await?;
        let levels = levels(&above, &old);
        let filter: Vec<(String, String)> = job.labels.clone().into_iter().collect();
        let modules: Vec<String> = above.keys().cloned().collect();
        let has_labels = |e: &EntryMeta| job.labels.iter().all(|(k, v)| e.labels.get(k) == Some(v));
        let mut matching: HashMap<String, Vec<(String, EntryMeta)>> = HashMap::new();
        let mut replaced: Vec<RebuildReplaced> = Vec::new();
        let on = self.catalog.entries_on(&modules, &filter).await?;
        let on_ids: Vec<String> = on.iter().map(|(e, _)| e.clone()).collect();
        let mut replacers_of = self.catalog.replacers_among(&on_ids).await?;
        for (e, entry_meta) in on {
            // Only heads are copied, and entries that only their copies from
            // this same rebuild replace, so that it replays them when sent
            // again.
            let key = format!("rebuild:{old}:{new}:{e}");
            let replacers = replacers_of.remove(&e).unwrap_or_default();
            if replacers.iter().all(|(_, r)| r.key.as_ref() == Some(&key)) {
                matching
                    .entry(entry_meta.module.clone())
                    .or_default()
                    .push((e, entry_meta));
                continue;
            }
            let outside: Vec<String> = replacers
                .iter()
                .filter(|(_, r)| !has_labels(r))
                .map(|(r, _)| self.id(r))
                .collect();
            if !outside.is_empty() {
                replaced.push(RebuildReplaced {
                    snapshot_id: self.id(&e),
                    replaced_by: outside,
                });
            }
        }
        let mut order: Vec<String> = needed(&above, matching.keys().cloned())
            .into_iter()
            .collect();
        order.sort_by_key(|m| levels.get(m).copied().unwrap_or(usize::MAX));
        let items: usize = order
            .iter()
            .map(|m| matching.get(m).map_or(1, Vec::len))
            .sum();
        if items > MAX_LIMIT {
            return Err(bad(&format!(
                "the rebuild would create {items} snapshots, more than {MAX_LIMIT}: narrow its labels"
            )));
        }

        // Which theorems use a changed definition, read from what the
        // modules' declarations use, level by level: what uses one below is
        // used here.
        let mut used: HashSet<String> = changed_definitions.iter().cloned().collect();
        if !changed_definitions.is_empty() {
            let (users, _) = uses(&used, now);
            used.extend(users);
        }

        let mut renamed = BTreeMap::from([(old.clone(), new.clone())]);
        // Each new module's original, whose declarations it is taken to have.
        let mut originals: HashMap<String, String> = HashMap::new();
        // What a copy builds on through each module it imports, as its create
        // will find it, the import itself last: through `from`, `to` and what
        // it builds on; through a module the plan rebuilds, that module's copy
        // and what the copy builds on; through any other, the module and what
        // it builds on.
        let mut through: HashMap<String, Vec<String>> =
            HashMap::from([(old.clone(), needs.clone())]);
        let mut planned: Vec<Planned> = Vec::new();
        // The records the plan reads, each once.
        let mut metas: HashMap<String, ModuleMeta> = HashMap::new();
        for m in order {
            let meta = self.module_cached(&m, &mut metas).await?.clone();
            let code = snapshot::rewrite_imports(&meta.code, &renamed);
            let id = snapshot::id(&code);
            let header = snapshot::next_header(&split_header(&code).header, &id);
            renamed.insert(m.clone(), id.clone());
            originals.insert(id.clone(), m.clone());
            // What the copy builds on, from its imports, its own module last.
            // The plan rebuilds what is below first, so an import not seen yet
            // is one it leaves.
            let mut needs_now: Vec<String> = Vec::new();
            for i in &meta.imports {
                if !through.contains_key(i) {
                    let kept = self.module_cached(i, &mut metas).await?;
                    through.insert(i.clone(), ids(&kept.modules).cloned().collect());
                }
                for n in &through[i] {
                    if !needs_now.contains(n) {
                        needs_now.push(n.clone());
                    }
                }
            }
            needs_now.push(id.clone());
            through.insert(m.clone(), needs_now.clone());
            let affected = if changed_definitions.is_empty() {
                Vec::new()
            } else {
                let (users, theorems) = uses(&used, &meta.result.declarations);
                used.extend(users);
                theorems
            };
            let item = |entry: Option<String>, labels, replaces| RebuildItem {
                level: 0,
                key: format!("rebuild:{old}:{new}:{}", entry.as_deref().unwrap_or(&m)),
                replaces,
                snapshot_id: entry.as_deref().map(|e| self.id(e)),
                module: snapshot::module(&m),
                new_module: snapshot::module(&id),
                new_header: header.clone(),
                code: code.clone(),
                labels,
                affected: affected.clone(),
            };
            let Some(entries) = matching.remove(&m) else {
                // Nothing to copy: it supports the copies above it, with no
                // labels and no link.
                planned.push(Planned {
                    created: meta.created * 1_000_000,
                    module: m.clone(),
                    item: item(None, BTreeMap::new(), None),
                });
                continue;
            };
            let needs_before: Vec<String> =
                ids(&meta.modules).filter(|n| **n != m).cloned().collect();
            for (e, entry_meta) in entries {
                // The comparison the copy's create will run, the copy taken to
                // declare what its original does, under an `allow` of every
                // definition: the copy replaces its original if it passes.
                let mut copied = Replacing {
                    replaced: Replaces {
                        snapshot_id: self.id(&e),
                        allow: None,
                    },
                    entry: e.clone(),
                    module: m.clone(),
                    declarations: meta.result.declarations.clone(),
                    needs: needs_before.clone(),
                    holes: entry_meta.holes.clone(),
                };
                let swapped = self
                    .predicted_swap(&copied, &needs_now, &id, &originals, &mut metas)
                    .await?;
                let definitions: Vec<String> = allowable(&copied, &swapped)
                    .map(|d| d.name.clone())
                    .collect();
                copied.replaced.allow = (!definitions.is_empty()).then(|| Allow {
                    definitions: definitions.clone(),
                });
                // What `allow` can name of what changed: the types, not what
                // Lean made from them.
                let replaces = compare(&copied, &copied.declarations, &swapped)
                    .ok()
                    .map(|r| {
                        let named: Vec<String> = r
                            .changed_definitions
                            .into_iter()
                            .filter(|n| definitions.contains(n))
                            .collect();
                        Replaces {
                            snapshot_id: copied.replaced.snapshot_id.clone(),
                            allow: (!named.is_empty()).then_some(Allow { definitions: named }),
                        }
                    });
                planned.push(Planned {
                    created: entry_meta.created_micros,
                    module: m.clone(),
                    item: item(Some(e), entry_meta.labels, replaces),
                });
            }
        }

        let mut on: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, p) in planned.iter().enumerate() {
            on.entry(&p.module).or_default().push(i);
        }
        let imports: Vec<Vec<usize>> = planned
            .iter()
            .map(|p| {
                let modules = above[&p.module].iter();
                modules
                    .filter_map(|m| on.get(m.as_str()))
                    .flatten()
                    .copied()
                    .collect()
            })
            .collect();
        let wave = waves(&imports);
        for (p, wave) in planned.iter_mut().zip(wave) {
            p.item.level = wave;
        }
        planned.sort_by_key(|p| (p.item.level, p.created));
        Ok(RebuildSnapshotsResponse {
            ok: true,
            changes,
            changed_definitions,
            plan: planned.into_iter().map(|p| p.item).collect(),
            replaced,
            ..Default::default()
        })
    }
}
