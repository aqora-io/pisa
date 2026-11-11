//! Run names: code may `import PisaRun.<name>`, which imports the head of
//! the run named `name`. [`Reader::resolve_runs`] puts the head's module in its
//! place before anything runs, so stored code always imports modules.

use super::*;

/// The heads of `run` from `root`, each with its module, oldest first.
fn heads_on(run: &Run, root: &str) -> Vec<(String, String)> {
    (run.heads_from(root).into_iter())
        .filter_map(|h| Some((h.clone(), run.entry(&h)?.module.clone())))
        .collect()
}

/// How many modules `heads` are on.
fn modules_of(heads: &[(String, String)]) -> usize {
    heads.iter().map(|(_, m)| m).collect::<HashSet<_>>().len()
}

/// Code with its imports by run name resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The code, importing each named run's head in its place, on the same
    /// line.
    pub code: String,
    /// What each name resolved to, in the order the header names them.
    pub runs: Vec<RunImport>,
}

/// `code` with each run it imports by name that `runs` has resolved as it
/// says; the others stay as they are.
pub fn resolve_with(code: &str, runs: &[RunImport]) -> String {
    let map: BTreeMap<String, String> = runs
        .iter()
        .filter_map(|r| Some((r.name.clone(), snapshot::parse_module(&r.module).ok()??)))
        .collect();
    snapshot::resolve_imports(code, &map)
}

impl Reader {
    /// `code` with each run it imports by name resolved to its head's module,
    /// with what each name resolved to. A name `pins` has resolves as it says,
    /// as a session's do. Code that imports no run by name comes back as it
    /// is, without reading the catalog.
    pub async fn resolve_runs(&self, code: &str, pins: &[RunImport]) -> Result<Resolved, Error> {
        let header = split_header(code).header;
        let names =
            snapshot::run_imports(&header).map_err(|e| Error::new(ErrorKind::BadRequest, e))?;
        if names.is_empty() {
            return Ok(Resolved {
                code: code.to_string(),
                runs: Vec::new(),
            });
        }
        let pinned = |name: &String| pins.iter().find(|p| p.name == *name);
        let unpinned: Vec<String> = names
            .iter()
            .filter(|n| pinned(n).is_none())
            .cloned()
            .collect();
        let heads = self.run_heads(&unpinned).await?;
        let mut runs = Vec::new();
        let mut map = BTreeMap::new();
        for name in &names {
            let run = match pinned(name) {
                Some(pin) => pin.clone(),
                None => {
                    let (entry, module) = &heads[name];
                    RunImport {
                        name: name.clone(),
                        module: snapshot::module(module),
                        snapshot_id: self.id(entry),
                        moved_to: None,
                    }
                }
            };
            let id = snapshot::parse_module(&run.module)
                .ok()
                .flatten()
                .ok_or_else(|| internal(format!("{} is not a snapshot module", run.module)))?;
            map.insert(name.clone(), id);
            runs.push(run);
        }
        Ok(Resolved {
            code: snapshot::resolve_imports(code, &map),
            runs,
        })
    }

    /// The head of each run `names` names, as its entry and module, in one
    /// read of the runs. A name no run has, a run with no entry left, and a
    /// run whose heads are on several modules (a fork) are refused.
    async fn run_heads(
        &self,
        names: &[String],
    ) -> Result<HashMap<String, (String, String)>, Error> {
        if names.is_empty() {
            return Ok(HashMap::new());
        }
        let roots = self.catalog.run_roots(names).await?;
        if let Some(missing) = names.iter().find(|n| !roots.contains_key(*n)) {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!(
                    "no run is named {missing}: a run is named by the `name` of the create that \
                     starts it"
                ),
            ));
        }
        let ids: Vec<String> = roots.values().cloned().collect();
        let run = Run::read(&self.catalog, &ids).await?;
        let mut found = HashMap::new();
        for name in names {
            let heads = heads_on(&run, &roots[name]);
            match modules_of(&heads) {
                0 => {
                    return Err(Error::new(
                        ErrorKind::NotFound,
                        format!("the run named {name} has no snapshot left"),
                    ));
                }
                1 => {
                    // The newest head, of those on the one module.
                    let head = heads.last().cloned().expect("a head");
                    found.insert(name.clone(), head);
                }
                n => {
                    let ids: Vec<String> = heads.iter().map(|(h, _)| self.id(h)).collect();
                    return Err(Error::new(
                        ErrorKind::Conflict,
                        format!(
                            "{}.{name} has heads on {n} modules, a fork: {}. Drop all but one \
                             with DELETE /snapshots/{{id}}?with_replacers=true on the snapshots \
                             the run's audit lists in forks, or import one by its module",
                            snapshot::RUNS,
                            ids.join(", ")
                        ),
                    ));
                }
            }
        }
        Ok(found)
    }

    /// The run named `name`: its root, its heads, and, when they are on one
    /// module, what the name resolves to. A name no run has, or whose run
    /// has no entry left, is `not_found`.
    pub async fn named_run(&self, name: &str) -> Result<NamedRun, Error> {
        let roots = self.catalog.run_roots(&[name.to_string()]).await?;
        let Some(root) = roots.get(name) else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("no run is named {name}"),
            ));
        };
        let run = Run::read(&self.catalog, std::slice::from_ref(root)).await?;
        let heads = heads_on(&run, root);
        if heads.is_empty() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("the run named {name} has no snapshot left"),
            ));
        }
        let resolved = (modules_of(&heads) == 1).then(|| heads.last()).flatten();
        Ok(NamedRun {
            name: name.to_string(),
            root: self.id(root),
            heads: heads.iter().map(|(h, _)| self.id(h)).collect(),
            module: resolved.map(|(_, m)| snapshot::module(m)),
            snapshot_id: resolved.map(|(h, _)| self.id(h)),
        })
    }

    /// Names `name` the run snapshot `id` is in, and answers the run.
    pub async fn name_run(&self, name: &str, id: &str) -> Result<NamedRun, Error> {
        snapshot::check_run_name(name).map_err(|m| Error::new(ErrorKind::BadRequest, m))?;
        let entry = self.parse(id)?;
        self.catalog.rename_run(&entry, name).await?;
        self.named_run(name).await
    }

    /// Names for the modules of an audit's `pairs` that say which run each
    /// is: when a pair's spec is of a named run, `PisaRun.<name>` for its
    /// head's module and `PisaSpec.<name>` for the spec's, unless the spec
    /// is its own head. A spec whose heads are on several modules, a fork,
    /// leaves them named by their hash, and so does every module the pairs
    /// import. Each name is one module's.
    pub async fn readable_names(
        &self,
        pairs: &[RunPair],
    ) -> Result<BTreeMap<String, String>, Error> {
        let specs: Vec<String> = pairs.iter().map(|p| p.spec.clone()).collect();
        let runs = self.catalog.run_names_of(&specs).await?;
        let mut names: BTreeMap<String, String> = BTreeMap::new();
        let mut give = |module: &String, name: String| {
            if !names.contains_key(module) && !names.values().any(|n| *n == name) {
                names.insert(module.clone(), name);
            }
        };
        for p in pairs {
            let Some(run) = runs.get(&p.spec) else {
                continue;
            };
            let heads: HashSet<&String> = (pairs.iter())
                .filter(|q| q.spec == p.spec)
                .map(|q| &q.head_module)
                .collect();
            if heads.len() == 1 {
                give(&p.head_module, format!("{}.{run}", snapshot::RUNS));
            }
        }
        for p in pairs {
            if let Some(run) = runs.get(&p.spec) {
                give(&p.spec_module, format!("{}.{run}", snapshot::SPECS));
            }
        }
        Ok(names)
    }

    /// `runs`, which code keeps resolving as they say, as a session's and a
    /// document's do, each with `moved_to` set to its run's head when the
    /// head is now on another module. A run that can't be read now, forked
    /// or gone, is left as it is.
    pub async fn moved(&self, runs: Vec<RunImport>) -> Vec<RunImport> {
        let mut out = Vec::new();
        for mut run in runs {
            run.moved_to = None;
            if let Ok(heads) = self.run_heads(std::slice::from_ref(&run.name)).await {
                let (entry, module) = &heads[&run.name];
                if snapshot::module(module) != run.module {
                    run.moved_to = Some(self.id(entry));
                }
            }
            out.push(run);
        }
        out
    }

    /// Refuses `runs` pinned to a module no snapshot (in the request's scope)
    /// is on now, as when the head the name resolved to was deleted since,
    /// naming the import, rather than as an unknown module.
    pub async fn pins_kept(&self, runs: &[RunImport]) -> Result<(), Error> {
        let id = |run: &RunImport| snapshot::parse_module(&run.module).ok().flatten();
        let ids: Vec<String> = runs.iter().filter_map(id).collect();
        if ids.is_empty() {
            return Ok(());
        }
        let on: HashSet<String> = (self.catalog.entries_on(&ids, &[]).await?.into_iter())
            .map(|(_, meta)| meta.module)
            .collect();
        match runs
            .iter()
            .find(|r| id(r).is_some_and(|id| !on.contains(&id)))
        {
            Some(run) => Err(Error::new(
                ErrorKind::NotFound,
                format!(
                    "{}.{} resolved to {} when this started, and no snapshot is on that \
                     module now: it was deleted. Import the name again where it resolves \
                     afresh to take the run's head",
                    snapshot::RUNS,
                    run.name,
                    run.module
                ),
            )),
            None => Ok(()),
        }
    }

    /// The first of the snapshot modules `ids` that the request's scope can't
    /// reach, as no entry in the scope is on it; none without a scope. A
    /// module's closure comes with it, so only what code imports directly
    /// needs asking.
    pub async fn unreachable(&self, ids: &[String]) -> Result<Option<String>, Error> {
        if ids.is_empty() || pisa_db::scope::current().is_empty() {
            return Ok(None);
        }
        let on: HashSet<String> = (self.catalog.entries_on(ids, &[]).await?.into_iter())
            .map(|(_, meta)| meta.module)
            .collect();
        Ok(ids.iter().find(|id| !on.contains(*id)).cloned())
    }

    /// Sets each of `snapshots`' `run` to the name of the run it is in, if
    /// that run has one.
    pub async fn name_runs(&self, snapshots: &mut [Snapshot]) -> Result<(), Error> {
        let entries: Vec<String> = snapshots
            .iter()
            .filter_map(|s| SnapshotId::parse(&s.id).map(|id| id.entry))
            .collect();
        if entries.is_empty() {
            return Ok(());
        }
        let names = self.catalog.run_names_of(&entries).await?;
        for s in snapshots {
            if let Some(id) = SnapshotId::parse(&s.id) {
                s.run = names.get(&id.entry).cloned();
            }
        }
        Ok(())
    }
}
