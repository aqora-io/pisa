//! Run names: code may `import PisaRun.<name>`, which imports the head of
//! the run named `name`. [`Reader::resolve_runs`] puts the head's module in its
//! place before anything runs, so stored code always imports modules.

use super::*;

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
            let heads: Vec<(String, String)> = run
                .heads_from(&roots[name])
                .into_iter()
                .filter_map(|h| Some((h.clone(), run.entry(&h)?.module.clone())))
                .collect();
            let mut modules: Vec<&String> = heads.iter().map(|(_, m)| m).collect();
            modules.sort();
            modules.dedup();
            match modules.len() {
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

    /// Names for `modules` (ids) that say which run each is: for a named
    /// run, `PisaRun.<name>` for the module its name resolves to, its head,
    /// and `PisaSpec.<name>` for its root's, when that one has no name yet.
    /// A module of no named run, or one a fork leaves without a head, has
    /// none. Each name is one module's.
    pub async fn readable_names(
        &self,
        modules: &[String],
    ) -> Result<BTreeMap<String, String>, Error> {
        let on = self.catalog.entries_on(modules, &[]).await?;
        let entries: Vec<String> = on.iter().map(|(e, _)| e.clone()).collect();
        let mut runs: Vec<String> = self
            .catalog
            .run_names_of(&entries)
            .await?
            .into_values()
            .collect();
        runs.sort();
        runs.dedup();
        let wanted: HashSet<&String> = modules.iter().collect();
        let mut names: BTreeMap<String, String> = BTreeMap::new();
        let mut give = |module: String, name: String| {
            if wanted.contains(&module)
                && !names.contains_key(&module)
                && !names.values().any(|n| *n == name)
            {
                names.insert(module, name);
            }
        };
        for run in &runs {
            if let Ok(heads) = self.run_heads(std::slice::from_ref(run)).await {
                let (_, module) = &heads[run];
                give(module.clone(), format!("{}.{run}", snapshot::RUNS));
            }
        }
        let roots = self.catalog.run_roots(&runs).await?;
        for run in &runs {
            let Some(root) = roots.get(run) else { continue };
            if let Ok(root) = self.catalog.entry(root).await {
                give(root.module, format!("{}.{run}", snapshot::SPECS));
            }
        }
        Ok(names)
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
