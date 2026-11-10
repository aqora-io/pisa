//! The audit of snapshots by their labels, and against a run's spec
//! snapshots (`/snapshots/audit`).

use super::*;

/// The assumptions a spec permits (see [`AuditSnapshotsResponse::clean`]):
/// those declared in a spec snapshot's module or in a module it builds on.
#[derive(Debug, Default)]
pub(super) struct Permitted {
    /// Their names.
    pub(super) names: HashSet<String>,
    /// The spec modules' own private ones, without their module: a head's
    /// module declares them as its own.
    pub(super) own_private: HashSet<String>,
}

impl Permitted {
    /// Adds the assumption `name`, declared in a spec module if `of_spec`.
    pub(super) fn add(&mut self, name: &str, of_spec: bool) {
        if of_spec && name.starts_with("_private.") {
            self.own_private.insert(normalised(name).into_owned());
        }
        self.names.insert(name.to_string());
    }

    /// Whether it permits `assumption` on a head whose module is `head`: by
    /// name, or as a private one of the head's own module that is a spec
    /// module's own. Another module's private name isn't the spec's.
    pub(super) fn permits(&self, assumption: &str, head: &str) -> bool {
        let own = format!("_private.{}.", snapshot::module(head));
        self.names.contains(assumption)
            || (assumption.starts_with(&own) && self.own_private.contains(&*normalised(assumption)))
    }
}

impl Snapshots {
    /// What the entries with every label in `labels` rest on; see
    /// [`AuditSnapshotsResponse`]. It reads records only.
    pub async fn audit(
        &self,
        labels: &[(String, String)],
    ) -> Result<AuditSnapshotsResponse, Error> {
        if labels.len() > MAX_LABELS {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("{} label filters; at most {MAX_LABELS}", labels.len()),
            ));
        }
        // An entry on each module the labels reach.
        let mut entries: BTreeMap<String, String> = BTreeMap::new();
        let mut after = None;
        loop {
            let (page, next) = self
                .catalog
                .list(labels, MAX_LIMIT, after.as_deref(), false)
                .await?;
            for (e, entry_meta) in page {
                entries.entry(entry_meta.module).or_insert(e);
            }
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        self.count(&entries, &mut HashMap::new()).await
    }

    /// The audit of a run against its spec, the entries `against` names:
    /// the walk from them to the run's heads, each head compared with its
    /// spec as if the definitions `allow` names were allowed to change, and
    /// the counts over the heads; see [`AuditSnapshotsResponse`]. It reads
    /// records only.
    pub async fn audit_against(
        &self,
        against: &[String],
        allow: &[String],
    ) -> Result<AuditSnapshotsResponse, Error> {
        let mut specs: Vec<(String, EntryMeta)> = Vec::new();
        for id in against {
            let entry = self.parse(id)?;
            if !specs.iter().any(|(e, _)| *e == entry) {
                let entry_meta = self.catalog.entry(&entry).await?;
                specs.push((entry, entry_meta));
            }
        }
        let mut links = Vec::new();
        let mut linked = HashSet::new();
        let mut add_link = |e: &str, entry_meta: &EntryMeta| {
            if let Some(link) = self.audit_link(e, entry_meta)
                && linked.insert(e.to_string())
            {
                links.push(link);
            }
        };
        let mut forks = Vec::new();
        // Each head once, as `(entry, module)`, and those each spec reaches.
        let mut heads: Vec<(String, String)> = Vec::new();
        let mut reached: Vec<Vec<String>> = Vec::new();
        // Whether a spec's chain back reaches a snapshot that is gone.
        let mut cut = false;
        for (spec, spec_meta) in &specs {
            // Back through what it replaced, as far as the entries exist.
            let mut back = Some((spec.clone(), spec_meta.clone()));
            let mut seen = HashSet::new();
            while let Some((e, entry_meta)) = back.take() {
                let Some(replaces) = &entry_meta.replaces else {
                    break;
                };
                if !seen.insert(e.clone()) {
                    break;
                }
                add_link(&e, &entry_meta);
                let older = self.catalog.find_entry(&replaces.snapshot_id).await?;
                cut |= older.is_none();
                back = older.map(|m| (replaces.snapshot_id.clone(), m));
            }
            // Forward through what replaces it, to the heads. A head has one
            // way back, as each snapshot replaces at most one.
            let mut ends = Vec::new();
            let mut stack = vec![(spec.clone(), spec_meta.clone())];
            let mut seen = HashSet::new();
            while let Some((e, entry_meta)) = stack.pop() {
                if !seen.insert(e.clone()) {
                    continue;
                }
                if e != *spec {
                    add_link(&e, &entry_meta);
                }
                let replacers = self.catalog.replacers(&e).await?;
                if replacers.is_empty() {
                    if !heads.iter().any(|(h, _)| *h == e) {
                        heads.push((e.clone(), entry_meta.module));
                    }
                    ends.push(e);
                    continue;
                }
                if replacers.len() > 1
                    && !forks
                        .iter()
                        .any(|f: &AuditFork| f.snapshot_id == self.id(&e))
                {
                    forks.push(AuditFork {
                        snapshot_id: self.id(&e),
                        replaced_by: replacers.iter().map(|(r, _)| self.id(r)).collect(),
                    });
                }
                // Oldest first.
                stack.extend(replacers.into_iter().rev());
            }
            reached.push(ends);
        }

        let mut metas: HashMap<String, ModuleMeta> = HashMap::new();
        let modules = specs.iter().map(|(_, m)| &m.module);
        for m in modules.chain(heads.iter().map(|(_, m)| m)) {
            self.cached(m, &mut metas).await?;
        }
        // What the spec permits: the axioms its modules, and what they build
        // on, declare.
        let mut permitted = Permitted::default();
        for (_, spec_meta) in &specs {
            let Some(meta) = metas.get(&spec_meta.module) else {
                continue;
            };
            let closure: Vec<String> = ids(&meta.modules).cloned().collect();
            for m in &closure {
                let Some(meta) = self.cached(m, &mut metas).await? else {
                    continue;
                };
                let axioms = meta
                    .result
                    .declarations
                    .iter()
                    .filter(|d| d.kind == "axiom");
                for d in axioms {
                    permitted.add(&d.name, *m == spec_meta.module);
                }
            }
        }
        let module_of: HashMap<&String, &String> = heads.iter().map(|(e, m)| (e, m)).collect();
        let mut comparisons = Vec::new();
        // The links' allows are what their creates asked for, by whoever
        // made them: the comparison takes only what the leader allows, the
        // spec's holes and the audit's `allow`.
        for ((spec, spec_meta), ends) in specs.iter().zip(&reached) {
            let allowed: Vec<String> = spec_meta.holes.iter().chain(allow).cloned().collect();
            for head in ends {
                let head_module = module_of[head];
                let compared = self
                    .rederive(&spec_meta.module, head_module, &allowed, &mut metas)
                    .await?;
                let (changes, replacement) = match compared {
                    Some(Ok(r)) => (Vec::new(), Some(r)),
                    Some(Err(changes)) => (changes, None),
                    None => (Vec::new(), None),
                };
                comparisons.push(AuditComparison {
                    spec: self.id(spec),
                    head: self.id(head),
                    reasons: changes.iter().map(ToString::to_string).collect(),
                    changes,
                    replacement,
                });
            }
        }
        let (targets, accepted) =
            self.targets(&specs, &reached, &heads, &metas, &permitted, &comparisons);
        // A spec copied from a snapshot that is gone, as a retiring rebuild
        // leaves it, can't be shown to state what the original did.
        // Every head keeps its spec's statements, whether or not the spec has
        // targets, such as one of assumptions only.
        let kept = comparisons.iter().all(|c| c.replacement.is_some());
        let clean = forks.is_empty() && accepted && kept && !cut;

        // Each head stands for its module.
        let mut entries: BTreeMap<String, String> = BTreeMap::new();
        for (e, m) in &heads {
            entries.entry(m.clone()).or_insert_with(|| e.clone());
        }
        let mut audit = self.count(&entries, &mut metas).await?;
        for a in &mut audit.assumptions {
            let on_a_head = heads.iter().any(|(_, m)| permitted.permits(&a.name, m));
            a.in_spec = Some(on_a_head);
        }
        audit.clean = Some(clean);
        audit.heads = heads.iter().map(|(e, _)| self.id(e)).collect();
        audit.forks = forks;
        audit.targets = targets;
        audit.links = links;
        audit.comparisons = comparisons;
        Ok(audit)
    }

    /// Compares head module `head` with spec module `spec` from their
    /// records, as a create that replaced the spec with the head's code, under
    /// an `allow` of `allowed`, would: what it changed, or why it can't
    /// replace it. `None` if either isn't verified, or is gone: their records
    /// are then the build's report, which the code judged could have
    /// influenced. Verified, their imports are too.
    pub(super) async fn rederive(
        &self,
        spec: &String,
        head: &str,
        allowed: &[String],
        metas: &mut HashMap<String, ModuleMeta>,
    ) -> Result<Option<Result<Replacement, Vec<DeclarationChange>>>, Error> {
        let (Some(spec_meta), Some(head_meta)) = (
            self.cached(spec, metas).await?.cloned(),
            self.cached(head, metas).await?.cloned(),
        ) else {
            return Ok(None);
        };
        if !spec_meta.verified || !head_meta.verified {
            return Ok(None);
        }
        let replacing = Replacing {
            replaced: Replaces {
                snapshot_id: spec.clone(),
                allow: (!allowed.is_empty()).then(|| Allow {
                    definitions: allowed.to_vec(),
                }),
            },
            entry: spec.clone(),
            module: spec.clone(),
            declarations: spec_meta.result.declarations,
            needs: ids(&spec_meta.modules)
                .filter(|m| *m != spec)
                .cloned()
                .collect(),
            holes: Vec::new(),
        };
        let needs: Vec<String> = ids(&head_meta.modules).cloned().collect();
        let swapped = match self
            .predicted_swap(&replacing, &needs, head, &HashMap::new(), metas)
            .await
        {
            Ok(swapped) => swapped,
            Err(e) if e.kind == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut compared = compare(&replacing, &head_meta.result.declarations, &swapped);
        if let Err(changes) = &mut compared {
            let mut new = vec![head.to_string()];
            new.extend(swapped.gained.iter().map(|(m, _)| m.clone()));
            let now = self.exports_of(&new).await;
            self.sign(changes, &replacing, &swapped, &now).await;
        }
        Ok(Some(compared))
    }

    /// The targets of an audit against `specs`: each declaration of a spec
    /// snapshot's module, other than its assumptions and what Lean
    /// generated, on each head that spec reaches (`reached`), with whether
    /// every one is accepted there under the assumptions `permitted` (see
    /// [`AuditSnapshotsResponse::clean`]). `heads` holds each head's module,
    /// and `metas` the module records.
    pub(super) fn targets(
        &self,
        specs: &[(String, EntryMeta)],
        reached: &[Vec<String>],
        heads: &[(String, String)],
        metas: &HashMap<String, ModuleMeta>,
        permitted: &Permitted,
        comparisons: &[AuditComparison],
    ) -> (Vec<AuditTarget>, bool) {
        let module_of: HashMap<&String, &String> = heads.iter().map(|(e, m)| (e, m)).collect();
        let mut targets = Vec::new();
        let mut all_accepted = true;
        for ((spec, spec_meta), ends) in specs.iter().zip(reached) {
            // A record deleted during the audit can't be judged, so isn't
            // accepted; nor below.
            let Some(meta) = metas.get(&spec_meta.module) else {
                all_accepted = false;
                continue;
            };
            let declared = meta.result.declarations.iter();
            for d in declared.filter(|d| !d.generated && d.kind != "axiom") {
                let name = normalised(&d.name);
                for head in ends {
                    let Some(head_meta) = metas.get(module_of[head]) else {
                        all_accepted = false;
                        continue;
                    };
                    let mut on_head = head_meta.result.declarations.iter();
                    // A replacement keeps every declaration, so this is a
                    // record that was edited: the run isn't done.
                    let Some(h) = on_head.find(|h| normalised(&h.name) == name) else {
                        all_accepted = false;
                        continue;
                    };
                    // Both records come from the kernel re-check, not from a
                    // build's report, and comparing them shows that the head
                    // keeps the spec's statements: its link alone doesn't.
                    let (spec_id, head_id) = (self.id(spec), self.id(head));
                    let kept = comparisons
                        .iter()
                        .any(|c| c.spec == spec_id && c.head == head_id && c.replacement.is_some());
                    let accepted = meta.verified
                        && head_meta.verified
                        && kept
                        && match h.status {
                            ProofStatus::Proved => true,
                            ProofStatus::Conditional => h
                                .assumes
                                .iter()
                                .all(|a| permitted.permits(a, module_of[head])),
                            ProofStatus::Sorry | ProofStatus::Rejected => false,
                        };
                    all_accepted &= accepted;
                    targets.push(AuditTarget {
                        name: d.name.clone(),
                        spec: spec_id,
                        head: head_id,
                        status: h.status,
                        verified: head_meta.verified,
                        accepted,
                    });
                }
            }
        }
        (targets, all_accepted)
    }

    /// Entry `e`'s link, as an audit lists it, if it let definitions change:
    /// under an `allow`, or filling holes.
    pub(super) fn audit_link(&self, e: &str, entry_meta: &EntryMeta) -> Option<AuditLink> {
        let changed = entry_meta
            .replacement
            .as_ref()
            .is_some_and(|r| !r.changed_definitions.is_empty());
        let replaces = entry_meta
            .replaces
            .as_ref()
            .filter(|r| r.allow.is_some() || changed)?;
        Some(AuditLink {
            snapshot_id: self.id(e),
            replaces: Replaces {
                snapshot_id: self.id(&replaces.snapshot_id),
                allow: replaces.allow.clone(),
            },
            replacement: entry_meta.replacement.clone().unwrap_or_default(),
        })
    }

    /// An audit's counts over the modules of `entries`, each with the entry
    /// that stands for it: their declarations by status, the assumptions
    /// they use, and what isn't proved. `metas` holds the module records
    /// read so far, and gains those read here.
    pub(super) async fn count(
        &self,
        entries: &BTreeMap<String, String>,
        metas: &mut HashMap<String, ModuleMeta>,
    ) -> Result<AuditSnapshotsResponse, Error> {
        let mut audit = AuditSnapshotsResponse::default();
        let mut used_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        // Each assumption is declared among the entries' modules or what
        // they build on.
        let mut closure: BTreeSet<String> = BTreeSet::new();
        for (m, entry) in entries {
            // Its entries were deleted since they were found.
            let Some(meta) = self.cached(m, metas).await? else {
                continue;
            };
            audit.summary.unverified_modules += u64::from(!meta.verified);
            for d in meta.result.declarations.iter().filter(|d| !d.generated) {
                // An assumption itself.
                if d.kind == "axiom" && d.assumes.contains(&d.name) {
                    continue;
                }
                let status = d.status;
                let summary = &mut audit.summary;
                *match status {
                    ProofStatus::Proved => &mut summary.proved,
                    ProofStatus::Conditional => &mut summary.conditional,
                    ProofStatus::Sorry => &mut summary.sorry,
                    ProofStatus::Rejected => &mut summary.rejected,
                } += 1;
                for a in &d.assumes {
                    used_by.entry(a.clone()).or_default().push(d.name.clone());
                }
                if status >= ProofStatus::Sorry {
                    let allowed = &self.policy.allowed_axioms;
                    audit.not_proved.push(AuditUnproved {
                        name: d.name.clone(),
                        snapshot_id: self.id(entry),
                        status,
                        axioms: d
                            .axioms
                            .iter()
                            .filter(|a| !allowed.contains(a))
                            .cloned()
                            .collect(),
                    });
                }
            }
            closure.extend(ids(&meta.modules).cloned());
        }
        for (name, mut users) in used_by {
            users.sort();
            users.dedup();
            let mut assumption = AuditAssumption {
                name,
                signature: None,
                snapshot_id: None,
                verified: false,
                used_by: users,
                in_spec: None,
            };
            if let Some(m) = self.declaring(&assumption.name, &closure, metas).await? {
                assumption.verified = metas[&m].verified;
                let exported = self
                    .catalog
                    .decls(&m)
                    .await?
                    .map(|d| d.decls)
                    .unwrap_or_default();
                assumption.signature = exported
                    .into_iter()
                    .find(|d| d.name == assumption.name)
                    .map(|d| d.signature);
                assumption.snapshot_id = match entries.get(&m) {
                    Some(e) => Some(self.id(e)),
                    None => {
                        let on = self
                            .catalog
                            .entries_on(std::slice::from_ref(&m), &[])
                            .await?;
                        on.into_iter().next().map(|(e, _)| self.id(&e))
                    }
                };
            }
            audit.assumptions.push(assumption);
        }
        Ok(audit)
    }

    /// Module `m`'s record, from `metas` or read into it; `None` if it is
    /// gone.
    pub(super) async fn cached<'a>(
        &self,
        m: &str,
        metas: &'a mut HashMap<String, ModuleMeta>,
    ) -> Result<Option<&'a ModuleMeta>, Error> {
        if !metas.contains_key(m) {
            match self.catalog.module(m).await {
                Ok(meta) => metas.insert(m.to_string(), meta),
                Err(e) if e.kind == ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
        }
        Ok(metas.get(m))
    }

    /// [`cached`](Self::cached) for a module that must be kept: `not_found`
    /// if it is gone, as [`Catalog::module`] has it.
    pub(super) async fn module_cached<'a>(
        &self,
        m: &str,
        metas: &'a mut HashMap<String, ModuleMeta>,
    ) -> Result<&'a ModuleMeta, Error> {
        self.cached(m, metas)
            .await?
            .ok_or_else(|| unknown_module(m))
    }

    /// The module among `modules` that declares the axiom `name`, loading
    /// the records `metas` doesn't hold yet.
    pub(super) async fn declaring(
        &self,
        name: &str,
        modules: &BTreeSet<String>,
        metas: &mut HashMap<String, ModuleMeta>,
    ) -> Result<Option<String>, Error> {
        for m in modules {
            let Some(meta) = self.cached(m, metas).await? else {
                continue;
            };
            let declares = |d: &Declaration| d.kind == "axiom" && d.name == name;
            if meta.result.declarations.iter().any(declares) {
                return Ok(Some(m.clone()));
            }
        }
        Ok(None)
    }
}
