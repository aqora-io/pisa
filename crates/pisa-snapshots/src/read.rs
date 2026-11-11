//! Reading the catalog: snapshots, their exports, dependents and labels, and
//! what a create reads of the run it joins.

use super::*;

impl Reader {
    /// The entry `r` names, with its module's declarations.
    pub async fn replacing(&self, r: &Replaces) -> Result<Replacing, Error> {
        let entry = self.parse(&r.snapshot_id)?;
        let entry_meta = self.catalog.entry(&entry).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        Ok(Replacing {
            replaced: r.clone(),
            entry,
            module: entry_meta.module.clone(),
            declarations: meta.result.declarations,
            needs: ids(&meta.modules)
                .filter(|m| **m != entry_meta.module)
                .cloned()
                .collect(),
            holes: entry_meta.holes,
        })
    }

    /// The error for a spec's `holes` that name something other than a
    /// definition or inductive type of its module, which declares
    /// `declarations`, or of the snapshot modules it builds on, `closure`.
    pub async fn holes_error(
        &self,
        holes: &[String],
        declarations: &[Declaration],
        closure: &[String],
    ) -> Result<Option<Error>, Error> {
        if holes.is_empty() {
            return Ok(None);
        }
        let closure: Vec<String> = closure
            .iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .cloned()
            .collect();
        let mut records = self.catalog.records(&closure).await?;
        let mut below = Vec::new();
        for m in &closure {
            let record = records.remove(m).ok_or_else(|| unknown_module(m))?;
            below.extend(record.result.declarations);
        }
        let names: HashSet<_> = declarations
            .iter()
            .chain(&below)
            .filter(|d| changeable(d))
            .map(|d| normalised(&d.name))
            .collect();
        Ok(misnamed(holes, &names).map(|(name, hint)| {
            Error::new(
                ErrorKind::BadRequest,
                format!(
                    "holes names {name}, which isn't a definition or inductive type of the \
                     snapshot or of a snapshot it builds on{hint}"
                ),
            )
        }))
    }

    /// The refusal of code whose header imports `imports`, snapshot
    /// modules, when two of the modules it would load declare one name.
    /// Lean loads both when they are theorems of one statement, and the code
    /// then uses whichever comes first in its imports, while the kernel
    /// re-check may describe it from the other. Within one module's own
    /// imports Lean refuses a name declared again, so only two or more
    /// imports can clash.
    pub async fn clashes(&self, imports: &[String]) -> Result<Option<Error>, Error> {
        let imports: BTreeSet<&String> = imports.iter().collect();
        if imports.len() < 2 {
            return Ok(None);
        }
        let mut closure = BTreeSet::new();
        for m in imports {
            closure.extend(ids(&self.catalog.module(m).await?.modules).cloned());
        }
        let closure: Vec<String> = closure.into_iter().collect();
        let records = self.catalog.records(&closure).await?;
        let declared = closure.iter().filter_map(|m| {
            let record = records.get(m)?;
            Some((m.as_str(), record.result.declarations.as_slice()))
        });
        let Some((name, modules)) = clash(declared) else {
            return Ok(None);
        };
        let entries = self.catalog.entries_on(&modules, &[]).await?;
        let named: Vec<String> = modules
            .iter()
            .map(|m| match entries.iter().find(|(_, e)| &e.module == m) {
                Some((e, _)) => format!("{} (snapshot {})", snapshot::module(m), self.id(e)),
                None => snapshot::module(m),
            })
            .collect();
        let mut message = format!(
            "{} each declare {name}, so no code can import both: Lean would take the first \
             import's, and the code would mean one or the other by the order of its imports",
            named.join(" and ")
        );
        // Versions of one run lead to the same heads: one of those is what to
        // import.
        let ids: Vec<String> = entries.iter().map(|(e, _)| e.clone()).collect();
        let run = Run::read(&self.catalog, &ids).await?;
        let reach = |m: &String| -> BTreeSet<String> {
            entries
                .iter()
                .filter(|(_, e)| &e.module == m)
                .flat_map(|(e, _)| run.heads_from(e))
                .collect()
        };
        let (first, second) = (reach(&modules[0]), reach(&modules[1]));
        let heads: Vec<String> = first.intersection(&second).map(|e| self.id(e)).collect();
        if !heads.is_empty() {
            message.push_str(&format!(
                ". They are versions of one run: import its head {} instead",
                heads.join(" or ")
            ));
        }
        Ok(Some(Error::new(ErrorKind::BadRequest, message)))
    }

    /// The refusal of a create that replaces `entry` when another entry
    /// replaces it already: the error, which names the replacers and the
    /// run's heads from there, and the heads. `None` if `entry` is a head.
    pub async fn replaced_by(&self, entry: &str) -> Result<Option<(Error, Vec<String>)>, Error> {
        let run = Run::read(&self.catalog, &[entry.to_string()]).await?;
        let replacers = run.replacers(entry);
        if replacers.is_empty() {
            return Ok(None);
        }
        let heads: Vec<String> = run.heads_from(entry).iter().map(|e| self.id(e)).collect();
        let replacers: Vec<String> = replacers.iter().map(|(r, _)| self.id(r)).collect();
        let message = format!(
            "{} is replaced by {}; the run's heads from there are {}. Replace a head, or set fork",
            self.id(entry),
            replacers.join(", "),
            heads.join(", ")
        );
        Ok(Some((Error::new(ErrorKind::Conflict, message), heads)))
    }

    /// The refusal of a create that replaces `entry` when the spec of the
    /// run it is on was amended: the error, which names the spec that
    /// supersedes it, and that spec's run's heads. A fork is refused too: the
    /// run is the old spec's either way.
    pub async fn superseded(&self, entry: &str) -> Result<Option<(Error, Vec<String>)>, Error> {
        let root = self.run_root(entry).await?;
        let Some(current) = self.superseding(&root).await? else {
            return Ok(None);
        };
        let heads = self.heads_from(&current).await?;
        let message = format!(
            "{}'s run, from spec {}, was amended by {}: replace one of the heads of its run, {}, \
             or carry a head over with POST /snapshots/resubmit",
            self.id(entry),
            self.id(&root),
            self.id(&current),
            heads.join(", ")
        );
        Ok(Some((Error::new(ErrorKind::Conflict, message), heads)))
    }

    /// The spec now current in place of `spec`: the newest of the specs that
    /// amend it, followed through the amendments made of those. `None` if
    /// nothing amends it.
    pub async fn superseding(&self, spec: &str) -> Result<Option<String>, Error> {
        let mut current = spec.to_string();
        let mut seen = HashSet::new();
        loop {
            seen.insert(current.clone());
            let mut amenders = Vec::new();
            for (e, a) in self
                .catalog
                .amendments_of(std::slice::from_ref(&current))
                .await?
            {
                if a.amends == current && !seen.contains(&e) {
                    let created = self.catalog.find_entry(&e).await?.map(|m| m.created_micros);
                    amenders.push((created.unwrap_or(0), e));
                }
            }
            match amenders.into_iter().max() {
                Some((_, e)) => current = e,
                None => break,
            }
        }
        Ok((current != spec).then_some(current))
    }

    /// The refutations of the targets of spec `spec`, each with the name of
    /// the theorem of `spec` it refutes: those recorded against `spec`, and
    /// those recorded against a spec it amends, directly or not, of a target
    /// that `spec` still means the same: a theorem of `spec` states it the
    /// same, by the hash of the statement, reaches no definition that an
    /// amendment on the way changed, and uses none of `spec`'s holes. Those
    /// whose refuting snapshot is deleted are `gone`.
    pub async fn refutations_against(
        &self,
        spec: &str,
    ) -> Result<Vec<(String, Refutation)>, Error> {
        // The specs it amends, back to the first, each with the names that
        // the amendments from `spec` back to it changed.
        let mut specs = vec![(spec.to_string(), HashSet::new())];
        loop {
            let (at, changed) = specs.last().unwrap().clone();
            let amendment = (self
                .catalog
                .amendments_of(std::slice::from_ref(&at))
                .await?)
                .into_iter()
                .find(|(e, _)| *e == at)
                .map(|(_, a)| a);
            match amendment {
                Some(a) if !specs.iter().any(|(s, _)| *s == a.amends) => {
                    let mut changed = changed;
                    changed.extend(a.changes.into_iter().map(|c| c.name));
                    specs.push((a.amends, changed));
                }
                _ => break,
            }
        }
        let changed_since: HashMap<String, HashSet<String>> = specs.into_iter().collect();
        let specs: Vec<String> = changed_since.keys().cloned().collect();
        let recorded: Vec<_> = (self.catalog.refutations_of(&specs).await?)
            .into_iter()
            .filter(|(_, r)| specs.contains(&r.spec))
            .collect();
        if recorded.is_empty() {
            return Ok(Vec::new());
        }
        // `spec`'s declarations, those of what it imports, and its holes.
        let (declarations, closure, holes) = match self.catalog.find_entry(spec).await? {
            Some(e) => {
                let meta = self.catalog.module(&e.module).await?;
                let imported: Vec<String> = (meta.modules.iter())
                    .map(|m| m.id.clone())
                    .filter(|m| *m != e.module)
                    .collect();
                let records = self.catalog.records(&imported).await?;
                let closure: Vec<Declaration> = (records.into_values())
                    .flat_map(|r| r.result.declarations)
                    .collect();
                (meta.result.declarations, closure, e.holes)
            }
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        let mut found = Vec::new();
        for (e, r) in recorded {
            let target = match (r.spec == spec, &r.target_hash) {
                (true, _) => Some(r.target.clone()),
                (false, Some(hash)) => declarations
                    .iter()
                    .find(|d| d.kind == "theorem" && !d.generated && d.type_hash == *hash)
                    .filter(|d| {
                        let changed = &changed_since[&r.spec];
                        let all = declarations.iter().chain(&closure);
                        let (_, reaching) = uses(changed, all);
                        !changed.contains(&d.name)
                            && !reaching.contains(&d.name)
                            && refutable(&declarations, &closure, &holes, &d.name).is_ok()
                    })
                    .map(|d| d.name.clone()),
                (false, None) => None,
            };
            let Some(target) = target else { continue };
            let gone = self.catalog.find_entry(&e).await?.is_none();
            found.push((
                target,
                Refutation {
                    gone,
                    ..self.refutation(&e, r)
                },
            ));
        }
        Ok(found)
    }

    /// The refutations of the targets of the spec the run `entry` is on
    /// starts from (see [`refutations_against`](Self::refutations_against)),
    /// in order.
    pub async fn refutations_of_run(&self, entry: &str) -> Result<Vec<Refutation>, Error> {
        let root = self.run_root(entry).await?;
        let mut found: Vec<Refutation> = (self.refutations_against(&root).await?)
            .into_iter()
            .map(|(_, r)| r)
            .collect();
        found.sort_by(|a, b| {
            (&a.spec, &a.target, &a.snapshot_id).cmp(&(&b.spec, &b.target, &b.snapshot_id))
        });
        Ok(found)
    }

    /// The spec the run `entry` is on starts from: `entry`, or what its
    /// chain of replacements leads back to, which may be deleted since.
    async fn run_root(&self, entry: &str) -> Result<String, Error> {
        let mut at = entry.to_string();
        // Links point at older entries, so the chain ends.
        while let Some(meta) = self.catalog.find_entry(&at).await? {
            match meta.replaces {
                Some(r) => at = r.snapshot_id,
                None => break,
            }
        }
        Ok(at)
    }

    /// The snapshot ids of the heads of the run from `entry`: the entries
    /// that nothing replaces, reached from `entry` through what replaces it,
    /// oldest first.
    pub async fn heads_from(&self, entry: &str) -> Result<Vec<String>, Error> {
        let run = Run::read(&self.catalog, &[entry.to_string()]).await?;
        if run.entry(entry).is_none() {
            // Not kept: the catalog says so.
            self.catalog.entry(entry).await?;
        }
        Ok(run.heads_from(entry).iter().map(|e| self.id(e)).collect())
    }

    /// The snapshot modules a replacement of `replacing` that builds on
    /// `needs` (its own module, `own`, among them) swaps; see [`Swapped`].
    pub async fn swapped(
        &self,
        replacing: &Replacing,
        needs: &[String],
        own: &str,
    ) -> Result<Swapped, Error> {
        self.predicted_swap(
            replacing,
            needs,
            own,
            &HashMap::new(),
            &mut Records::default(),
        )
        .await
    }

    /// [`swapped`](Self::swapped) for a replacement that builds on modules
    /// not made yet: each module `originals` maps is taken to declare what
    /// the module it maps to does. The records it reads are kept in
    /// `records`.
    pub(crate) async fn predicted_swap(
        &self,
        replacing: &Replacing,
        needs: &[String],
        own: &str,
        originals: &HashMap<String, String>,
        records: &mut Records,
    ) -> Result<Swapped, Error> {
        let (now, before): (HashSet<&String>, HashSet<&String>) =
            (needs.iter().collect(), replacing.needs.iter().collect());
        let lost: Vec<&String> = replacing
            .needs
            .iter()
            .filter(|m| !now.contains(m))
            .collect();
        let gained: Vec<&String> = needs
            .iter()
            .filter(|m| *m != own && !before.contains(m))
            .collect();
        let read = |m: &String| originals.get(m).unwrap_or(m).clone();
        let reads: Vec<String> = lost.iter().chain(&gained).map(|m| read(m)).collect();
        records.load(&self.catalog, &reads).await?;
        let mut swapped = Swapped::default();
        for (side, modules) in [(&mut swapped.lost, lost), (&mut swapped.gained, gained)] {
            for m in modules {
                let read = read(m);
                let record = records.held(&read).ok_or_else(|| unknown_module(&read))?;
                side.push((m.clone(), record.result.declarations.clone()));
            }
        }
        Ok(swapped)
    }

    /// The signatures the search exports of `modules` show, by name.
    pub async fn exports_of(&self, modules: &[String]) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for m in modules {
            if let Ok(Some(exported)) = self.catalog.decls(m).await {
                out.extend(exported.decls.into_iter().map(|d| (d.name, d.signature)));
            }
        }
        out
    }

    /// Fills `was` and `now` of each change to a type or an assumption: the
    /// signature in the replaced module or the lost snapshot it is from, by
    /// their search exports, and in `now`, the new side's signatures by name.
    pub async fn sign(
        &self,
        changes: &mut [DeclarationChange],
        replacing: &Replacing,
        swapped: &Swapped,
        now: &HashMap<String, String>,
    ) {
        let signed = |c: &DeclarationChange| matches!(c.change, Change::Type | Change::Assumption);
        if !changes.iter().any(signed) {
            return;
        }
        let mut old = vec![replacing.module.clone()];
        old.extend(swapped.lost.iter().map(|(m, _)| m.clone()));
        let was = self.exports_of(&old).await;
        for c in changes.iter_mut().filter(|c| signed(c)) {
            c.was = was.get(&c.name).cloned();
            c.now = now.get(&c.name).cloned();
        }
    }

    /// The records of the modules `header` imports.
    pub async fn parents(&self, header: &str) -> Result<Vec<ModuleMeta>, Error> {
        let mut parents = Vec::new();
        for import in snapshot::imports(header).map_err(internal)? {
            parents.push(self.catalog.module(&import).await?);
        }
        Ok(parents)
    }

    pub async fn get(&self, entry: &str) -> Result<SnapshotDetail, Error> {
        let entry = self.parse(entry)?;
        let entry_meta = self.catalog.entry(&entry).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        let exports = self.exports(&entry_meta.module).await?;
        let snapshot = self.snapshot(entry, entry_meta, &meta);
        Ok(SnapshotDetail {
            snapshot,
            code: meta.code,
            check: meta.result,
            exports,
        })
    }

    /// Module `m`'s exports, each with the description search's leader has
    /// recorded for it, if any.
    pub(crate) async fn exports(&self, m: &str) -> Result<Vec<Export>, Error> {
        let Some(decls) = self.catalog.decls(m).await? else {
            return Ok(Vec::new());
        };
        let mut described: HashMap<String, serde_json::Value> =
            self.catalog.described(m).await?.into_iter().collect();
        let public = decls.decls.into_iter().filter(|d| !d.private);
        let exports = public.map(|d| Export {
            description: described
                .remove(&d.name)
                .and_then(|r| serde_json::from_value::<Informal>(r.get("informal")?.clone()).ok())
                .map(Description::from),
            name: d.name,
            kind: d.kind,
            signature: d.signature,
            docstring: d.docstring,
        });
        Ok(exports.collect())
    }

    /// The entries, newest first, that have every label in `labels`, at most
    /// `limit`, after the cursor `after`, and only heads if `heads`; see
    /// [`Catalog::list`].
    pub async fn list(
        &self,
        labels: &[(String, String)],
        limit: usize,
        after: Option<&str>,
        heads: bool,
    ) -> Result<ListSnapshotsResponse, Error> {
        if !(1..=MAX_LIMIT).contains(&limit) || after.is_some_and(|a| !is_cursor(a)) {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("a listing takes a limit from 1 to {MAX_LIMIT} and a cursor from `next`"),
            ));
        }
        if labels.len() > MAX_LABELS {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!(
                    "{} label filters; a listing takes at most {MAX_LABELS}",
                    labels.len()
                ),
            ));
        }
        let (listed, next) = self.catalog.list(labels, limit, after, heads).await?;
        Ok(ListSnapshotsResponse {
            snapshots: self.snapshots(listed).await?,
            next,
        })
    }

    /// The entries above `entry`'s module, parents first; see
    /// [`ListDependentsResponse`].
    pub async fn dependents(
        &self,
        entry: &str,
        labels: &[(String, String)],
    ) -> Result<ListDependentsResponse, Error> {
        let entry = self.parse(entry)?;
        if labels.len() > MAX_LABELS {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("{} label filters; at most {MAX_LABELS}", labels.len()),
            ));
        }
        let root = self.catalog.entry(&entry).await?.module;
        let above = self.catalog.dependents(&root).await?;
        let levels = levels(&above, &root);
        let modules: Vec<String> = above.keys().cloned().collect();
        let mut found = self.catalog.entries_on(&modules, labels).await?;
        // Stable, so each level stays oldest first.
        found.sort_by_key(|(_, e)| levels.get(&e.module).copied().unwrap_or(usize::MAX));
        Ok(ListDependentsResponse {
            snapshots: self.snapshots(found).await?,
        })
    }

    /// The entries `found`, in order, as snapshots, from their modules'
    /// summaries in one read: an entry whose module was deleted since it was
    /// found is left out.
    async fn snapshots(&self, found: Vec<(String, EntryMeta)>) -> Result<Vec<Snapshot>, Error> {
        let mut modules: Vec<String> = found.iter().map(|(_, e)| e.module.clone()).collect();
        modules.sort();
        modules.dedup();
        let summaries = self.catalog.summaries(&modules).await?;
        Ok(found
            .into_iter()
            .filter_map(|(entry, entry_meta)| {
                let summary = summaries.get(&entry_meta.module)?;
                Some(self.snapshot_of(entry, entry_meta, summary))
            })
            .collect())
    }

    /// Edits an entry's labels with a merge patch; see
    /// [`Catalog::set_labels`].
    pub async fn set_labels(
        &self,
        entry: &str,
        labels: &BTreeMap<String, Option<String>>,
    ) -> Result<Snapshot, Error> {
        let entry = self.parse(entry)?;
        let entry_meta = self.catalog.set_labels(&entry, labels).await?;
        let meta = self.catalog.module(&entry_meta.module).await?;
        Ok(self.snapshot(entry, entry_meta, &meta))
    }
}

/// The first name that two of `modules`, by id with their declarations,
/// declare, with those modules in order. Names Lean makes for a declaration,
/// and private names, which carry their module, are left out.
pub(crate) fn clash<'a>(
    modules: impl IntoIterator<Item = (&'a str, &'a [Declaration])>,
) -> Option<(String, Vec<String>)> {
    let mut by_name: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (m, declarations) in modules {
        for d in declarations {
            if d.generated || d.name.starts_with("_private.") {
                continue;
            }
            by_name.entry(&d.name).or_default().insert(m);
        }
    }
    by_name
        .into_iter()
        .find(|(_, ms)| ms.len() > 1)
        .map(|(name, ms)| (name.to_string(), ms.into_iter().map(String::from).collect()))
}
