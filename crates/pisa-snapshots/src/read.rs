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
        let mut below = Vec::new();
        for m in closure.iter().collect::<BTreeSet<_>>() {
            below.extend(self.catalog.module(m).await?.result.declarations);
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

    /// The refusal of a create that replaces `entry` when another entry
    /// replaces it already: the error, which names the replacers and the
    /// run's heads from there, and the heads. `None` if `entry` is a head.
    pub async fn replaced_by(&self, entry: &str) -> Result<Option<(Error, Vec<String>)>, Error> {
        let replacers = self.catalog.replacers(entry).await?;
        if replacers.is_empty() {
            return Ok(None);
        }
        let heads = self.heads_from(entry).await?;
        let replacers: Vec<String> = replacers.iter().map(|(r, _)| self.id(r)).collect();
        let message = format!(
            "{} is replaced by {}; the run's heads from there are {}. Replace a head, or set fork",
            self.id(entry),
            replacers.join(", "),
            heads.join(", ")
        );
        Ok(Some((Error::new(ErrorKind::Conflict, message), heads)))
    }

    /// The snapshot ids of the heads of the run from `entry`: the entries
    /// that nothing replaces, reached from `entry` through what replaces it,
    /// oldest first.
    pub async fn heads_from(&self, entry: &str) -> Result<Vec<String>, Error> {
        let created = self.catalog.entry(entry).await?.created_micros;
        let mut todo = vec![(entry.to_string(), created)];
        let mut heads = Vec::new();
        while let Some((e, created)) = todo.pop() {
            let replacers = self.catalog.replacers(&e).await?;
            if replacers.is_empty() {
                heads.push((created, e));
            }
            todo.extend(replacers.into_iter().map(|(r, m)| (r, m.created_micros)));
        }
        heads.sort();
        Ok(heads.into_iter().map(|(_, e)| self.id(&e)).collect())
    }

    /// The snapshot modules a replacement of `replacing` that builds on
    /// `needs` (its own module, `own`, among them) swaps; see [`Swapped`].
    pub async fn swapped(
        &self,
        replacing: &Replacing,
        needs: &[String],
        own: &str,
    ) -> Result<Swapped, Error> {
        self.predicted_swap(replacing, needs, own, &HashMap::new(), &mut HashMap::new())
            .await
    }

    /// [`swapped`](Self::swapped) for a replacement that builds on modules
    /// not made yet: each module `originals` maps is taken to declare what
    /// the module it maps to does. The records it reads are kept in `metas`.
    pub(crate) async fn predicted_swap(
        &self,
        replacing: &Replacing,
        needs: &[String],
        own: &str,
        originals: &HashMap<String, String>,
        metas: &mut HashMap<String, ModuleMeta>,
    ) -> Result<Swapped, Error> {
        let lost: Vec<&String> = replacing
            .needs
            .iter()
            .filter(|m| !needs.contains(m))
            .collect();
        let gained = needs
            .iter()
            .filter(|m| *m != own && !replacing.needs.contains(m));
        let mut swapped = Swapped::default();
        for (side, modules) in [
            (&mut swapped.lost, lost),
            (&mut swapped.gained, gained.collect()),
        ] {
            for m in modules {
                let read = originals.get(m).unwrap_or(m);
                let decls = self
                    .module_cached(read, metas)
                    .await?
                    .result
                    .declarations
                    .clone();
                side.push((m.clone(), decls));
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
        let exports = decls.decls.into_iter().map(|d| Export {
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
        let mut modules: HashMap<String, Option<ModuleMeta>> = HashMap::new();
        let mut snapshots = Vec::new();
        for (entry, entry_meta) in listed {
            let id = entry_meta.module.clone();
            if !modules.contains_key(&id) {
                let meta = match self.catalog.module(&id).await {
                    Ok(meta) => Some(meta),
                    // Its entries were deleted since they were listed.
                    Err(e) if e.kind == ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                };
                modules.insert(id.clone(), meta);
            }
            if let Some(meta) = &modules[&id] {
                snapshots.push(self.snapshot(entry, entry_meta, meta));
            }
        }
        Ok(ListSnapshotsResponse { snapshots, next })
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
        let mut metas: HashMap<String, Option<ModuleMeta>> = HashMap::new();
        let mut snapshots = Vec::new();
        for (entry, entry_meta) in found {
            let id = entry_meta.module.clone();
            if !metas.contains_key(&id) {
                let meta = match self.catalog.module(&id).await {
                    Ok(meta) => Some(meta),
                    // Deleted since it was found.
                    Err(e) if e.kind == ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                };
                metas.insert(id.clone(), meta);
            }
            if let Some(meta) = &metas[&id] {
                snapshots.push(self.snapshot(entry, entry_meta, meta));
            }
        }
        Ok(ListDependentsResponse { snapshots })
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
