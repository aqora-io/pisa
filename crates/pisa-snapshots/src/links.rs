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
use pisa_protocol::api::{AuditSwap, AuditTarget, AuditThrough, AuditUnlinked, AuditUnsettled};

use super::*;

/// Where a theorem of module `module` is read through links.
#[derive(Debug, Clone)]
struct Resolution {
    /// The entry on the module whose run reaches the head.
    entry: String,
    head: String,
    head_module: String,
    /// The theorems whose statements the head's changes reach, and those
    /// refuted on the entry, by [`normalised`] name, with why: neither
    /// resolves.
    unresolved: HashMap<String, String>,
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
    /// The theorems it inherits through that weren't read through links.
    unread: Vec<AuditUnlinked>,
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
        for u in other.unread {
            if !self
                .unread
                .iter()
                .any(|x| x.name == u.name && x.module == u.module)
            {
                self.unread.push(u);
            }
        }
        self.cut |= other.cut;
    }

    /// Not reading theorem `name` of `module` through links, for `reason`.
    fn unread(mut self, name: &str, module: &str, reason: String) -> Effective {
        self.unread.push(AuditUnlinked {
            name: name.to_string(),
            module: Some(module.to_string()),
            reason,
        });
        self
    }
}

/// What the resolution reads: the records, held by the audit's cache, and
/// where each module's theorems resolve.
struct Linker<'a> {
    reader: &'a Reader,
    records: &'a Records,
    resolved: &'a HashMap<String, Result<Resolution, String>>,
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
            let why = format!(
                "{} was described before what a declaration inherits was recorded: verify it \
                 again",
                snapshot::module(m)
            );
            return Some(Effective::recorded(&d).unread(name, &snapshot::module(m), why));
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
        let worst = || {
            let e = Effective {
                axioms: ["sorryAx".to_string()].into(),
                ..Default::default()
            };
            e.unread(name, module, "its module's record is gone".into())
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
            return e.unread(name, module, "two proofs can't justify each other".into());
        }
        let on_head = match self.resolved.get(&m) {
            Some(Ok(r)) => match r.unresolved.get(normalised(&c.name).as_ref()) {
                Some(why) => Err(why.clone()),
                None => {
                    let head = (self.records.held(&r.head_module))
                        .and_then(|rec| declared(rec, &c.name))
                        .filter(|head| head.kind == "theorem" && head.type_hash == c.type_hash);
                    match head {
                        Some(head) => Ok((r.clone(), head.name.clone())),
                        None => Err(format!(
                            "{} doesn't state it the same",
                            self.reader.id(&r.head)
                        )),
                    }
                }
            },
            Some(Err(why)) => Err(why.clone()),
            None => Err("its run wasn't read".into()),
        };
        match on_head {
            Ok((r, head_name)) => {
                let Some(mut e) = self.effective(&r.head_module, &head_name) else {
                    let why = format!("the record of {} is gone", self.reader.id(&r.head));
                    return Effective::recorded(&c).unread(name, module, why);
                };
                let through = AuditThrough {
                    name: c.name.clone(),
                    snapshot_id: self.reader.id(&r.entry),
                    head: self.reader.id(&r.head),
                };
                e.through.insert(0, (through, r.head_module.clone()));
                e
            }
            Err(why) => self
                .effective(&m, &c.name)
                .unwrap_or_else(|| Effective::recorded(&c))
                .unread(name, module, why),
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
    ) -> Result<Result<Resolution, String>, Error> {
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
        if reached.is_empty() {
            return Ok(Err("nothing replaces its snapshot yet".into()));
        }
        let forked = |r: &[(String, String, String)]| {
            let heads: BTreeSet<String> = r.iter().map(|(_, h, _)| self.id(h)).collect();
            let heads: Vec<String> = heads.into_iter().collect();
            format!("its run reaches more than one head: {}", heads.join(", "))
        };
        if modules(&reached).len() > 1 {
            let all = forked(&reached);
            reached.retain(|(e, _, _)| preferred.contains(e));
            if modules(&reached).len() != 1 {
                return Ok(Err(all));
            }
        }
        let (entry, head, head_module) = reached.swap_remove(0);
        let m = m.to_string();
        records.load(&self.catalog, [&m, &head_module]).await?;
        let verified = |x: &str| records.held(x).is_some_and(|r| r.verified);
        if !verified(&m) {
            return Ok(Err(format!("{} isn't verified", self.id(&entry))));
        }
        if !verified(&head_module) {
            return Ok(Err(format!("{} isn't verified", self.id(&head))));
        }
        let holes = run
            .entry(&entry)
            .map(|meta| meta.holes.clone())
            .unwrap_or_default();
        let replacement = match self.rederive(&m, &head_module, &holes, records).await? {
            Some(Ok(replacement)) => replacement,
            Some(Err(_)) => {
                let why = format!("{} doesn't keep its module's statements", self.id(&head));
                return Ok(Err(why));
            }
            None => return Ok(Err("a record on the way is gone or not verified".into())),
        };
        let Some(record) = records.held(&m) else {
            return Ok(Err("its module's record is gone".into()));
        };
        let sorried = |n: &&String| {
            declared(record, n).is_some_and(|d| d.axioms.iter().any(|a| a == "sorryAx"))
        };
        let valued: Vec<&String> = (replacement.changed_definitions.iter())
            .filter(|n| !sorried(n))
            .collect();
        if !valued.is_empty() {
            let valued: Vec<&str> = valued.iter().map(|n| n.as_str()).collect();
            return Ok(Err(format!(
                "{} changes {}, whose value wasn't sorry",
                self.id(&head),
                valued.join(", ")
            )));
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
            let why = format!(
                "{} doesn't build on all its module builds on",
                self.id(&head)
            );
            return Ok(Err(why));
        }
        let affected = format!("{}'s changes reach its statement", self.id(&head));
        let mut unresolved: HashMap<String, String> = (replacement.affected.iter())
            .map(|n| (normalised(n).into_owned(), affected.clone()))
            .collect();
        for (_, r) in self
            .catalog
            .refutations_of(std::slice::from_ref(&entry))
            .await?
        {
            if r.spec == entry {
                let why = format!("it is refuted on {}", self.id(&entry));
                unresolved.insert(normalised(&r.target).into_owned(), why);
            }
        }
        Ok(Ok(Resolution {
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
        let mut resolved: HashMap<String, Result<Resolution, String>> = HashMap::new();
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
                if let Ok(r) = &r {
                    next.push(r.head_module.clone());
                }
                resolved.insert(m.clone(), r);
            }
            frontier = inherits(records, &next);
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
            if t.status == ProofStatus::Proved {
                continue;
            }
            let Some(head) = linker.records.held(m) else {
                continue;
            };
            let Some(d) = declared(head, &t.name) else {
                continue;
            };
            // A head that can't be read says why, for each theorem it
            // inherits through, or, from before that was recorded, for itself.
            if !head.verified {
                let why = format!("{} isn't verified", t.head);
                t.unlinked = (d.inherits.iter())
                    .map(|i| AuditUnlinked {
                        name: i.name.clone(),
                        module: Some(i.module.clone()),
                        reason: why.clone(),
                    })
                    .collect();
                continue;
            }
            if d.own_axioms.is_none() {
                t.unlinked = vec![AuditUnlinked {
                    name: d.name.clone(),
                    module: None,
                    reason: format!(
                        "{} was described before what a declaration inherits was recorded: \
                         verify it again",
                        t.head
                    ),
                }];
                continue;
            }
            if d.inherits.is_empty() {
                continue;
            }
            let name = d.name.clone();
            let Some(e) = linker.effective(m, &name) else {
                continue;
            };
            let axioms: Vec<String> = e.axioms.into_iter().collect();
            let assumes: Vec<String> = e.assumes.into_iter().collect();
            let status = ProofStatus::of(&axioms, &assumes, &self.allowed_axioms);
            if status != ProofStatus::Proved {
                t.unlinked = e.unread;
            }
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
        resolved: &HashMap<String, Result<Resolution, String>>,
        spec_modules: &[&String],
        heads: &[(String, String)],
        allow: &[String],
        records: &mut Records,
    ) -> Result<Vec<String>, Error> {
        let mut blockers = Vec::new();
        let used: Vec<(&String, &Resolution)> = (resolved.iter())
            .filter_map(|(old, r)| r.as_ref().ok().map(|r| (old, r)))
            .filter(|(_, r)| swaps.iter().any(|s| s.head == self.id(&r.head)))
            .collect();
        let audited: HashSet<&String> = heads.iter().map(|(e, _)| e).collect();
        for (_, r) in &used {
            let nested = (records.held(&r.head_module).into_iter())
                .flat_map(|rec| &rec.result.declarations)
                .flat_map(|d| &d.inherits)
                .filter_map(|i| snapshot::parse_module(&i.module).ok().flatten())
                .any(|x| resolved.get(&x).is_some_and(Result::is_ok));
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
