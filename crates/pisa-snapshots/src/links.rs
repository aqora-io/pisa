//! Status through links: what a target's status is once each theorem it
//! inherits axioms through is read on the head that the theorem's run
//! reaches. A theorem's proof can be swapped for another proof of the same
//! statement (proof irrelevance), and a `sorry` is stuck, so nothing that
//! type-checked against the old proof reduced through it: the target holds
//! once the run is settled, which rebuilds it on that head.
//!
//! The reading is conservative. A theorem resolves only through a run with
//! one head, verified, that keeps the theorem's statement, and whose changes
//! to the module's definitions only fill `sorry`-valued holes: anything that
//! walked through such a definition has `sorry` among its own axioms, so no
//! proof can depend on a value that changed. Otherwise, or on a cycle, a
//! theorem counts with the axioms recorded for it.

use pisa_db::catalog::ModuleRecord;
use pisa_protocol::api::{AuditSwap, AuditTarget, AuditThrough, AuditUnsettled};

use super::*;

/// Where a theorem of module `module` is read through links.
#[derive(Debug, Clone)]
struct Resolution {
    /// The entry on the module whose run reaches the head.
    entry: String,
    head: String,
    head_module: String,
    /// The theorems whose statements the head's changes reach, and those
    /// refuted on the entry, by [`normalised`] name: neither resolves.
    unresolved: HashSet<String>,
    /// The definitions the head changed: `sorry`-valued holes it filled.
    changed: Vec<String>,
}

/// The axioms and assumptions a declaration has through links, and what it
/// read through them.
#[derive(Debug, Clone, Default)]
struct Effective {
    axioms: BTreeSet<String>,
    assumes: BTreeSet<String>,
    through: Vec<(AuditThrough, String)>,
    /// A cycle was cut below it, so it isn't kept for reuse.
    cut: bool,
}

impl Effective {
    fn recorded(d: &Declaration) -> Effective {
        Effective {
            axioms: d.axioms.iter().cloned().collect(),
            assumes: d.assumes.iter().cloned().collect(),
            ..Default::default()
        }
    }

    fn add(&mut self, other: Effective) {
        self.axioms.extend(other.axioms);
        self.assumes.extend(other.assumes);
        for t in other.through {
            if !self.through.contains(&t) {
                self.through.push(t);
            }
        }
        self.cut |= other.cut;
    }
}

/// What the resolution reads: the records, held by the audit's cache, and
/// where each module's theorems resolve.
struct Linker<'a> {
    reader: &'a Reader,
    records: &'a Records,
    resolved: &'a HashMap<String, Option<Resolution>>,
    done: HashMap<(String, String), Effective>,
    visiting: HashSet<(String, String)>,
}

/// The declaration of module record `r` named `name`, or with its
/// [`normalised`] name.
fn declared<'r>(r: &'r ModuleRecord, name: &str) -> Option<&'r Declaration> {
    let decls = &r.result.declarations;
    decls.iter().find(|d| d.name == name).or_else(|| {
        let n = normalised(name);
        decls.iter().find(|d| normalised(&d.name) == n)
    })
}

impl Linker<'_> {
    /// Declaration `name` of module `m` through links; `None` if its record
    /// is gone.
    fn effective(&mut self, m: &str, name: &str) -> Option<Effective> {
        let key = (m.to_string(), name.to_string());
        if let Some(e) = self.done.get(&key) {
            return Some(e.clone());
        }
        let d = declared(self.records.held(m)?, name)?.clone();
        let Some(own) = &d.own_axioms else {
            return Some(Effective::recorded(&d));
        };
        // Two proofs can't justify each other.
        if self.visiting.contains(&key) {
            let mut e = Effective::recorded(&d);
            e.cut = true;
            return Some(e);
        }
        let mut e = Effective {
            axioms: own.iter().cloned().collect(),
            assumes: d.assumes.iter().cloned().collect(),
            ..Default::default()
        };
        self.visiting.insert(key.clone());
        for i in &d.inherits {
            e.add(self.inherited(&i.module, &i.name));
        }
        self.visiting.remove(&key);
        if !e.cut {
            self.done.insert(key, e.clone());
        }
        Some(e)
    }

    /// Theorem `name` of the module named `module`, which a declaration
    /// inherits through: read on the head of its run where it resolves, else
    /// in its own module.
    fn inherited(&mut self, module: &str, name: &str) -> Effective {
        let worst = || Effective {
            axioms: ["sorryAx".to_string()].into(),
            ..Default::default()
        };
        let Ok(Some(m)) = snapshot::parse_module(module) else {
            return worst();
        };
        let Some(c) = self
            .records
            .held(&m)
            .and_then(|r| declared(r, name))
            .cloned()
        else {
            return worst();
        };
        if self.visiting.contains(&(m.clone(), c.name.clone())) {
            let mut e = Effective::recorded(&c);
            e.cut = true;
            return e;
        }
        let resolution = self.resolved.get(&m).cloned().flatten();
        let on_head = resolution.as_ref().and_then(|r| {
            if r.unresolved.contains(normalised(&c.name).as_ref()) {
                return None;
            }
            let head = declared(self.records.held(&r.head_module)?, &c.name)?;
            (head.kind == "theorem" && head.type_hash == c.type_hash)
                .then(|| (r.clone(), head.name.clone()))
        });
        match on_head {
            Some((r, head_name)) => {
                let Some(mut e) = self.effective(&r.head_module, &head_name) else {
                    return Effective::recorded(&c);
                };
                let through = AuditThrough {
                    name: c.name.clone(),
                    snapshot_id: self.reader.id(&r.entry),
                    head: self.reader.id(&r.head),
                };
                e.through.insert(0, (through, r.head_module.clone()));
                e
            }
            None => self
                .effective(&m, &c.name)
                .unwrap_or_else(|| Effective::recorded(&c)),
        }
    }
}

impl Reader {
    /// Where the theorems of module `m` resolve: the one head module that the
    /// runs of its entries reach, preferring entries of `preferred`, if it
    /// is verified, keeps `m`'s statements and closure, and changes only
    /// `sorry`-valued holes of `m`.
    async fn resolve(
        &self,
        m: &str,
        entries: &[(String, EntryMeta)],
        run: &Run,
        preferred: &HashSet<String>,
        records: &mut Records,
    ) -> Result<Option<Resolution>, Error> {
        let mut reached: Vec<(String, String, String)> = Vec::new();
        for (e, _) in entries.iter().filter(|(_, meta)| meta.module == m) {
            if run.replacers(e).is_empty() {
                continue;
            }
            for head in run.heads_from(e) {
                if let Some(meta) = run.entry(&head)
                    && meta.module != m
                {
                    reached.push((e.clone(), head, meta.module.clone()));
                }
            }
        }
        let modules = |r: &[(String, String, String)]| -> BTreeSet<String> {
            r.iter().map(|(_, _, hm)| hm.clone()).collect()
        };
        if modules(&reached).len() > 1 {
            reached.retain(|(e, _, _)| preferred.contains(e));
        }
        if modules(&reached).len() != 1 {
            return Ok(None);
        }
        let (entry, head, head_module) = reached.swap_remove(0);
        let m = m.to_string();
        records.load(&self.catalog, [&m, &head_module]).await?;
        let verified = |x: &str| records.held(x).is_some_and(|r| r.verified);
        if !verified(&m) || !verified(&head_module) {
            return Ok(None);
        }
        let holes = run
            .entry(&entry)
            .map(|meta| meta.holes.clone())
            .unwrap_or_default();
        let Some(Ok(replacement)) = self.rederive(&m, &head_module, &holes, records).await? else {
            return Ok(None);
        };
        let Some(record) = records.held(&m) else {
            return Ok(None);
        };
        let sorried = |n: &String| {
            declared(record, n).is_some_and(|d| d.axioms.iter().any(|a| a == "sorryAx"))
        };
        if !replacement.changed_definitions.iter().all(sorried) {
            return Ok(None);
        }
        let below: HashSet<String> = records
            .closure(&self.catalog, &m)
            .await?
            .into_iter()
            .filter(|x| *x != m)
            .collect();
        let head_closure: HashSet<String> = records
            .closure(&self.catalog, &head_module)
            .await?
            .into_iter()
            .collect();
        if !below.is_subset(&head_closure) {
            return Ok(None);
        }
        let mut unresolved: HashSet<String> = (replacement.affected.iter())
            .map(|n| normalised(n).into_owned())
            .collect();
        for (_, r) in self
            .catalog
            .refutations_of(std::slice::from_ref(&entry))
            .await?
        {
            if r.spec == entry {
                unresolved.insert(normalised(&r.target).into_owned());
            }
        }
        Ok(Some(Resolution {
            entry,
            head,
            head_module,
            unresolved,
            changed: replacement.changed_definitions,
        }))
    }

    /// Sets each target's status through links, where it differs, and
    /// returns the heads that a settle would rebuild, with what it would
    /// swap and what keeps it from rebuilding one. `heads` are the audit's,
    /// with their modules; `run` is the audit's read of its runs, whose
    /// entries are preferred where a module's runs disagree; `allow` is the
    /// audit's.
    pub(crate) async fn link(
        &self,
        targets: &mut [AuditTarget],
        specs: &[(String, EntryMeta)],
        heads: &[(String, String)],
        run: &Run,
        allow: &[String],
        records: &mut Records,
    ) -> Result<Vec<AuditUnsettled>, Error> {
        let module_of: HashMap<String, String> = (heads.iter())
            .map(|(e, m)| (self.id(e), m.clone()))
            .collect();
        // The modules inherited through, round by round, each round's
        // entries and runs read at once.
        let inherits = |records: &Records, ms: &[String]| -> BTreeSet<String> {
            let decls = ms
                .iter()
                .filter_map(|m| records.held(m))
                .flat_map(|r| &r.result.declarations);
            decls
                .flat_map(|d| &d.inherits)
                .filter_map(|i| snapshot::parse_module(&i.module).ok().flatten())
                .collect()
        };
        let head_modules: Vec<String> = heads.iter().map(|(_, m)| m.clone()).collect();
        let mut frontier = inherits(records, &head_modules);
        let mut resolved: HashMap<String, Option<Resolution>> = HashMap::new();
        let preferred: HashSet<String> = run.ids().cloned().collect();
        while !frontier.is_empty() {
            let ms: Vec<String> = frontier
                .into_iter()
                .filter(|m| !resolved.contains_key(m))
                .collect();
            if ms.is_empty() {
                break;
            }
            records.load(&self.catalog, &ms).await?;
            let entries = self.catalog.entries_on(&ms, &[]).await?;
            let ids: Vec<String> = entries.iter().map(|(e, _)| e.clone()).collect();
            let runs = Run::read(&self.catalog, &ids).await?;
            let mut next = Vec::new();
            for m in &ms {
                let r = self
                    .resolve(m, &entries, &runs, &preferred, records)
                    .await?;
                next.push(m.clone());
                if let Some(r) = &r {
                    next.push(r.head_module.clone());
                }
                resolved.insert(m.clone(), r);
            }
            frontier = inherits(records, &next);
        }
        if resolved.values().all(Option::is_none) {
            return Ok(Vec::new());
        }

        let mut linker = Linker {
            reader: self,
            records: &*records,
            resolved: &resolved,
            done: HashMap::new(),
            visiting: HashSet::new(),
        };
        // Each head's swaps, by its id, in order.
        let mut unsettled: BTreeMap<String, Vec<AuditThrough>> = BTreeMap::new();
        for t in targets.iter_mut() {
            let Some(m) = module_of.get(&t.head) else {
                continue;
            };
            let Some(head) = linker.records.held(m).filter(|r| r.verified) else {
                continue;
            };
            let Some(d) = declared(head, &t.name) else {
                continue;
            };
            if d.own_axioms.is_none() || d.inherits.is_empty() {
                continue;
            }
            let name = d.name.clone();
            let Some(e) = linker.effective(m, &name) else {
                continue;
            };
            let axioms: Vec<String> = e.axioms.into_iter().collect();
            let assumes: Vec<String> = e.assumes.into_iter().collect();
            let status = ProofStatus::of(&axioms, &assumes, &self.allowed_axioms);
            if status == t.status {
                continue;
            }
            t.linked_status = Some(status);
            t.through = e.through.iter().map(|(t, _)| t.clone()).collect();
            let swaps = unsettled.entry(t.head.clone()).or_default();
            for (through, _) in e.through {
                if !swaps.contains(&through) {
                    swaps.push(through);
                }
            }
        }
        let spec_modules: Vec<&String> = specs.iter().map(|(_, meta)| &meta.module).collect();
        let mut out = Vec::new();
        for (head, through) in unsettled {
            let mut swaps: Vec<AuditSwap> = Vec::new();
            for t in &through {
                let swap = AuditSwap {
                    snapshot_id: t.snapshot_id.clone(),
                    head: t.head.clone(),
                };
                if !swaps.contains(&swap) {
                    swaps.push(swap);
                }
            }
            let m = module_of[&head].clone();
            let blockers = self
                .blockers(&m, &swaps, &resolved, &spec_modules, heads, allow, records)
                .await?;
            out.push(AuditUnsettled {
                head,
                swaps,
                blockers,
            });
        }
        Ok(out)
    }

    /// What keeps a settle from rebuilding head module `m` with `swaps`:
    /// a swap onto a head the audit doesn't settle that needs settling too, a
    /// name the swapped heads add that `m` declares, a private name of a
    /// swapped module in `m`'s statements, and a filled hole that a spec's
    /// statements use and the audit doesn't `allow`.
    #[allow(clippy::too_many_arguments)]
    async fn blockers(
        &self,
        m: &str,
        swaps: &[AuditSwap],
        resolved: &HashMap<String, Option<Resolution>>,
        spec_modules: &[&String],
        heads: &[(String, String)],
        allow: &[String],
        records: &mut Records,
    ) -> Result<Vec<String>, Error> {
        let mut blockers = Vec::new();
        let used: Vec<(&String, &Resolution)> = (resolved.iter())
            .filter_map(|(old, r)| r.as_ref().map(|r| (old, r)))
            .filter(|(_, r)| swaps.iter().any(|s| s.head == self.id(&r.head)))
            .collect();
        let audited: HashSet<&String> = heads.iter().map(|(e, _)| e).collect();
        for (_, r) in &used {
            let nested = (records.held(&r.head_module).into_iter())
                .flat_map(|rec| &rec.result.declarations)
                .flat_map(|d| &d.inherits)
                .filter_map(|i| snapshot::parse_module(&i.module).ok().flatten())
                .any(|x| resolved.get(&x).is_some_and(Option::is_some));
            if nested && !audited.contains(&r.head) {
                blockers.push(format!(
                    "{} builds on snapshots that need settling too: add the spec of its run, \
                     which {} is on, to against",
                    self.id(&r.head),
                    self.id(&r.entry)
                ));
            }
        }
        let Some(own) = records.held(m).cloned() else {
            return Ok(blockers);
        };
        let now: HashSet<String> = records
            .closure(&self.catalog, m)
            .await?
            .into_iter()
            .collect();
        let mut added: Vec<String> = Vec::new();
        for (_, r) in &used {
            for x in records.closure(&self.catalog, &r.head_module).await? {
                if !now.contains(&x) && !added.contains(&x) {
                    added.push(x);
                }
            }
        }
        records.load(&self.catalog, &added).await?;
        let names: HashSet<&str> = own
            .result
            .declarations
            .iter()
            .filter(|d| !d.name.starts_with("_private."))
            .map(|d| d.name.as_str())
            .collect();
        for x in &added {
            let Some(rec) = records.held(x) else { continue };
            for d in rec
                .result
                .declarations
                .iter()
                .filter(|d| names.contains(d.name.as_str()))
            {
                blockers.push(format!(
                    "{} declares {}, as {} does",
                    snapshot::module(x),
                    d.name,
                    snapshot::module(m)
                ));
            }
        }
        for (old, _) in &used {
            let prefix = format!("_private.{}.", snapshot::module(old));
            for d in &own.result.declarations {
                if let Some(p) = d.type_refs.iter().find(|r| r.starts_with(&prefix)) {
                    blockers.push(format!(
                        "the statement of {} uses {p}, a private name of {}, which the settle swaps",
                        d.name,
                        snapshot::module(old)
                    ));
                }
            }
        }
        records
            .load(&self.catalog, spec_modules.iter().copied())
            .await?;
        for (_, r) in &used {
            for hole in r.changed.iter().filter(|h| !allows_name(allow, h)) {
                let stated = (spec_modules.iter())
                    .filter_map(|s| records.held(s))
                    .flat_map(|rec| &rec.result.declarations)
                    .find(|d| {
                        d.type_refs
                            .iter()
                            .any(|t| normalised(t) == normalised(hole))
                    });
                if let Some(d) = stated {
                    blockers.push(format!(
                        "the spec states {} with {hole}, which {} fills: allow it in the audit",
                        d.name,
                        self.id(&r.head)
                    ));
                }
            }
        }
        Ok(blockers)
    }
}

/// Whether the audit's `allow` names `name`, by [`normalised`] name.
fn allows_name(allow: &[String], name: &str) -> bool {
    let name = normalised(name);
    allow.iter().any(|a| normalised(a) == name)
}
