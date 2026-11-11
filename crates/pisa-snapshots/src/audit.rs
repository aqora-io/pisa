//! The audit of snapshots by their labels, and against a run's spec
//! snapshots (`/snapshots/audit`).

use super::*;

/// The assumptions a spec permits (see [`AuditSnapshotsResponse::clean`]):
/// those declared in a spec snapshot's module or in a module it builds on.
#[derive(Debug, Default)]
pub(crate) struct Permitted {
    /// Their names.
    pub names: HashSet<String>,
    /// The spec modules' own private ones, without their module: a head's
    /// module declares them as its own.
    pub own_private: HashSet<String>,
}

impl Permitted {
    /// Adds the assumption `name`, declared in a spec module if `of_spec`.
    pub(crate) fn add(&mut self, name: &str, of_spec: bool) {
        if of_spec && name.starts_with("_private.") {
            self.own_private.insert(normalised(name).into_owned());
        }
        self.names.insert(name.to_string());
    }

    /// Whether it permits `assumption` on a head whose module is `head`: by
    /// name, or as a private one of the head's own module that is a spec
    /// module's own. Another module's private name isn't the spec's.
    pub(crate) fn permits(&self, assumption: &str, head: &str) -> bool {
        let own = format!("_private.{}.", snapshot::module(head));
        self.names.contains(assumption)
            || (assumption.starts_with(&own) && self.own_private.contains(&*normalised(assumption)))
    }
}

/// An audit against a spec, with the pairs it compared: what comparator
/// judges of the same run.
pub struct AuditedRun {
    pub audit: AuditSnapshotsResponse,
    /// One per comparison, in the same order.
    pub pairs: Vec<RunPair>,
}

/// A spec snapshot and a head it reaches, as entries and their modules.
#[derive(Debug, Clone)]
pub struct RunPair {
    pub spec: String,
    pub head: String,
    pub spec_module: String,
    pub head_module: String,
    /// The spec's holes.
    pub holes: Vec<String>,
}

impl Reader {
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
        self.count(&entries, &mut Records::default()).await
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
        Ok(self.audit_run(against, allow).await?.audit)
    }

    /// [`audit_against`](Self::audit_against), with the pairs it compared.
    pub async fn audit_run(
        &self,
        against: &[String],
        allow: &[String],
    ) -> Result<AuditedRun, Error> {
        let mut wanted: Vec<String> = Vec::new();
        for id in against {
            let entry = self.parse(id)?;
            if !wanted.contains(&entry) {
                wanted.push(entry);
            }
        }
        // The whole run, from one read of the catalog.
        let run = Run::read(&self.catalog, &wanted).await?;
        let mut specs: Vec<(String, EntryMeta)> = Vec::new();
        for entry in wanted {
            let entry_meta = match run.entry(&entry) {
                Some(meta) => meta.clone(),
                None => self.catalog.entry(&entry).await?,
            };
            specs.push((entry, entry_meta));
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
                let older = run.entry(&replaces.snapshot_id).cloned();
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
                let replacers = run.replacers(&e);
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

        let mut records = Records::default();
        let spec_modules: Vec<&String> = specs.iter().map(|(_, m)| &m.module).collect();
        let modules = spec_modules.iter().copied();
        records
            .load(&self.catalog, modules.chain(heads.iter().map(|(_, m)| m)))
            .await?;
        // What the spec permits: the axioms its modules, and what they build
        // on, declare.
        let mut permitted = Permitted::default();
        records
            .load_closures(&self.catalog, spec_modules.iter().copied())
            .await?;
        for (_, spec_meta) in &specs {
            if records.held(&spec_meta.module).is_none() {
                continue;
            }
            let closure = records.closure(&self.catalog, &spec_meta.module).await?;
            records.load(&self.catalog, &closure).await?;
            for m in &closure {
                let Some(meta) = records.held(m) else {
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
        let mut pairs = Vec::new();
        // Snapshots whose records the kernel hasn't re-checked, in order.
        let mut unverified: Vec<String> = Vec::new();
        // The links' allows are what their creates asked for, by whoever
        // made them: the comparison takes only what the leader allows, the
        // spec's holes and the audit's `allow`.
        for ((spec, spec_meta), ends) in specs.iter().zip(&reached) {
            let allowed: Vec<String> = spec_meta.holes.iter().chain(allow).cloned().collect();
            for head in ends {
                let head_module = module_of[head];
                let compared = self
                    .rederive(&spec_meta.module, head_module, &allowed, &mut records)
                    .await?;
                let (changes, replacement) = match compared {
                    Some(Ok(r)) => (Vec::new(), Some(r)),
                    Some(Err(changes)) => (changes, None),
                    None => (Vec::new(), None),
                };
                let mut reasons: Vec<String> = changes.iter().map(ToString::to_string).collect();
                for (e, m) in [(spec, &spec_meta.module), (head, head_module)] {
                    if records.held(m).is_some_and(|r| !r.verified) {
                        let id = self.id(e);
                        reasons.push(format!("{id} isn't verified: POST /snapshots/{id}/verify"));
                        if !unverified.contains(&id) {
                            unverified.push(id);
                        }
                    }
                }
                comparisons.push(AuditComparison {
                    spec: self.id(spec),
                    head: self.id(head),
                    reasons,
                    changes,
                    replacement,
                });
                pairs.push(RunPair {
                    spec: spec.clone(),
                    head: head.clone(),
                    spec_module: spec_meta.module.clone(),
                    head_module: head_module.clone(),
                    holes: spec_meta.holes.clone(),
                });
            }
        }
        let (mut targets, mut accepted) =
            self.targets(&specs, &reached, &heads, &records, &permitted, &comparisons);
        // A target that a snapshot refutes, on its spec or on a spec that
        // states it the same and that its spec amends, isn't accepted.
        let spec_entries: Vec<String> = specs.iter().map(|(e, _)| e.clone()).collect();
        let mut refuting: Vec<(String, String, Refutation)> = Vec::new();
        for spec in &spec_entries {
            for (target, r) in self.refutations_against(spec).await? {
                refuting.push((self.id(spec), target, r));
            }
        }
        let mut refutations: Vec<Refutation> = refuting.iter().map(|(_, _, r)| r.clone()).collect();
        refutations.sort_by(|a, b| {
            (&a.spec, &a.target, &a.snapshot_id).cmp(&(&b.spec, &b.target, &b.snapshot_id))
        });
        refutations.dedup();
        for t in &mut targets {
            t.refuted_by = (refuting.iter())
                .filter(|(spec, target, _)| *spec == t.spec && *target == t.name)
                .map(|(_, _, r)| r.snapshot_id.clone())
                .collect();
            if !t.refuted_by.is_empty() {
                t.accepted = false;
                t.contradicted = matches!(t.status, ProofStatus::Proved | ProofStatus::Conditional);
                accepted = false;
            }
        }
        let unsettled = self
            .link(&mut targets, &specs, &heads, &run, allow, &mut records)
            .await?;
        // A spec copied from a snapshot that is gone, as a retiring rebuild
        // leaves it, can't be shown to state what the original did.
        // Every head keeps its spec's statements, whether or not the spec has
        // targets, such as one of assumptions only.
        let kept = comparisons.iter().all(|c| c.replacement.is_some());
        let mut superseded = Vec::new();
        for spec in &spec_entries {
            if let Some(by) = self.superseding(spec).await? {
                superseded.push(Superseded {
                    spec: self.id(spec),
                    by: self.id(&by),
                });
            }
        }
        let clean = forks.is_empty()
            && accepted
            && kept
            && !cut
            && refutations.is_empty()
            && superseded.is_empty();

        // Each head stands for its module.
        let mut entries: BTreeMap<String, String> = BTreeMap::new();
        for (e, m) in &heads {
            entries.entry(m.clone()).or_insert_with(|| e.clone());
        }
        let mut audit = self.count(&entries, &mut records).await?;
        for a in &mut audit.assumptions {
            let on_a_head = heads.iter().any(|(_, m)| permitted.permits(&a.name, m));
            a.in_spec = Some(on_a_head);
        }
        audit.clean = Some(clean);
        audit.heads = heads.iter().map(|(e, _)| self.id(e)).collect();
        audit.unverified = unverified;
        audit.forks = forks;
        audit.targets = targets;
        audit.links = links;
        audit.comparisons = comparisons;
        audit.refutations = refutations;
        audit.unsettled = unsettled;
        let mut amendments: Vec<Amendment> = (self.catalog.amendments_of(&spec_entries).await?)
            .into_iter()
            .map(|(e, a)| self.amendment(&e, a))
            .collect();
        amendments.sort_by(|a, b| (&a.amends, &a.snapshot_id).cmp(&(&b.amends, &b.snapshot_id)));
        audit.amendments = amendments;
        audit.superseded = superseded;
        Ok(AuditedRun { audit, pairs })
    }

    /// Compares head module `head` with spec module `spec` from their
    /// records, as a create that replaced the spec with the head's code, under
    /// an `allow` of `allowed`, would: what it changed, or why it can't
    /// replace it. `None` if either isn't verified, or is gone: their records
    /// are then the build's report, which the code judged could have
    /// influenced. Verified, their imports are too.
    pub(crate) async fn rederive(
        &self,
        spec: &String,
        head: &str,
        allowed: &[String],
        records: &mut Records,
    ) -> Result<Option<Result<Replacement, Vec<DeclarationChange>>>, Error> {
        let head = head.to_string();
        records.load(&self.catalog, [spec, &head]).await?;
        let (Some(spec_meta), Some(head_meta)) =
            (records.held(spec).cloned(), records.held(&head).cloned())
        else {
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
            needs: records
                .closure(&self.catalog, spec)
                .await?
                .into_iter()
                .filter(|m| m != spec)
                .collect(),
            holes: Vec::new(),
        };
        let needs = records.closure(&self.catalog, &head).await?;
        let swapped = match self
            .predicted_swap(&replacing, &needs, &head, &HashMap::new(), records)
            .await
        {
            Ok(swapped) => swapped,
            Err(e) if e.kind == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut compared = compare(&replacing, &head_meta.result.declarations, &swapped);
        if let Err(changes) = &mut compared {
            let mut new = vec![head.clone()];
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
    /// and `records` the module records.
    pub(crate) fn targets(
        &self,
        specs: &[(String, EntryMeta)],
        reached: &[Vec<String>],
        heads: &[(String, String)],
        records: &Records,
        permitted: &Permitted,
        comparisons: &[AuditComparison],
    ) -> (Vec<AuditTarget>, bool) {
        let module_of: HashMap<&String, &String> = heads.iter().map(|(e, m)| (e, m)).collect();
        let mut targets = Vec::new();
        let mut all_accepted = true;
        for ((spec, spec_meta), ends) in specs.iter().zip(reached) {
            // A record deleted during the audit can't be judged, so isn't
            // accepted; nor below.
            let Some(meta) = records.held(&spec_meta.module) else {
                all_accepted = false;
                continue;
            };
            let declared = meta.result.declarations.iter();
            for d in declared.filter(|d| !d.generated && d.kind != "axiom") {
                let name = normalised(&d.name);
                for head in ends {
                    let Some(head_meta) = records.held(module_of[head]) else {
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
                        refuted_by: Vec::new(),
                        linked_status: None,
                        through: Vec::new(),
                        contradicted: false,
                    });
                }
            }
        }
        (targets, all_accepted)
    }

    /// Entry `e`'s link, as an audit lists it, if it let definitions change:
    /// under an `allow`, or filling holes.
    pub(crate) fn audit_link(&self, e: &str, entry_meta: &EntryMeta) -> Option<AuditLink> {
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
    /// they use, and what isn't proved. `records` holds the module records
    /// read so far, and gains those read here.
    pub(crate) async fn count(
        &self,
        entries: &BTreeMap<String, String>,
        records: &mut Records,
    ) -> Result<AuditSnapshotsResponse, Error> {
        let mut audit = AuditSnapshotsResponse::default();
        let mut used_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        records.load(&self.catalog, entries.keys()).await?;
        for (m, entry) in entries {
            // Its entries were deleted since they were found.
            let Some(meta) = records.held(m) else {
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
                    let allowed = &self.allowed_axioms;
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
        }
        // Each assumption is declared among the entries' modules or what
        // they build on.
        let kept: Vec<String> = entries
            .keys()
            .filter(|m| records.held(m).is_some())
            .cloned()
            .collect();
        let closure = match used_by.is_empty() {
            true => BTreeSet::new(),
            false => records.below(&self.catalog, &kept).await?,
        };
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
            if let Some(m) = self.declaring(&assumption.name, &closure, records).await? {
                assumption.verified = records.held(&m).is_some_and(|r| r.verified);
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

    /// The module among `modules` that declares the axiom `name`, loading
    /// the records `records` doesn't hold yet.
    pub(crate) async fn declaring(
        &self,
        name: &str,
        modules: &BTreeSet<String>,
        records: &mut Records,
    ) -> Result<Option<String>, Error> {
        records.load(&self.catalog, modules).await?;
        for m in modules {
            let Some(meta) = records.held(m) else {
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
