//! The audit's review list (`/snapshots/audit?against=…&review=true`): what a
//! reviewer reads to know what a run's spec says on each head it reaches.
//!
//! From the spec module's declarations, the walk follows, on the head, each
//! declaration's type and the values of definitions and inductive types, and
//! an inductive type's constructors, through the head and the snapshots it
//! builds on, by their records. It doesn't follow theorems' proofs, and
//! axioms are leaves, so a candidate filled into a theorem's proof adds only
//! its statement. The comparison already holds the head to the spec's
//! declarations but for holes and what the audit allows, so on the head the
//! walk also reaches what a hole's new value uses. Signatures and values come
//! from the builds' declaration exports, which also name the library
//! constants that pisa's records leave out.

use std::collections::{HashMap, HashSet, VecDeque};

use pisa_protocol::api::{AuditComparison, AuditReview, Error, ReviewDeclaration, ReviewOrigin};
use pisa_protocol::verify::{Declaration, normalised};

use pisa_db::catalog::{ModuleRecord, SnapshotDecl};

use crate::{Reader, Records};

/// Whether the walk follows a declaration's value: a definition's, and an
/// inductive type's, whose "value" is its constructors and recursor.
fn follows_value(kind: &str) -> bool {
    matches!(kind, "def" | "opaque" | "inductive")
}

/// The names reached from `start` through `records` (name → declaration),
/// each once, in the order reached. Names not among `records`, such as the
/// library's, aren't followed, nor are what Lean generates.
pub fn closure(start: &[String], records: &HashMap<String, Declaration>) -> Vec<String> {
    let mut constructors: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, d) in records {
        if let (Some(parent), "constructor") = (&d.parent, d.kind.as_str()) {
            constructors.entry(parent).or_default().push(name);
        }
    }
    for names in constructors.values_mut() {
        names.sort();
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut queue: VecDeque<String> = start.iter().cloned().collect();
    while let Some(name) = queue.pop_front() {
        let Some(d) = records.get(&name) else {
            continue;
        };
        if d.generated || !seen.insert(name.clone()) {
            continue;
        }
        queue.extend(d.type_refs.iter().cloned());
        if follows_value(&d.kind) {
            queue.extend(d.value_refs.iter().cloned());
        }
        if d.kind == "inductive" {
            let made = constructors.get(name.as_str()).into_iter().flatten();
            queue.extend(made.map(|c| c.to_string()));
        }
        out.push(name);
    }
    out
}

/// The review list of each of the audit's `comparisons`, a spec snapshot and
/// a head it reaches, with the spec's holes and `allow` as the definitions
/// shown by value.
pub async fn review(
    snapshots: &Reader,
    comparisons: &[AuditComparison],
    allow: &[String],
) -> Result<Vec<AuditReview>, Error> {
    let catalog = snapshots.catalog();
    let mut metas = Records::default();
    let mut exports: HashMap<String, HashMap<String, SnapshotDecl>> = HashMap::new();
    // Why a module's export couldn't be made again, for those whose couldn't.
    let mut failed: HashMap<String, String> = HashMap::new();
    let mut snapshot_ids: HashMap<String, Option<String>> = HashMap::new();
    let mut out = Vec::new();
    for compared in comparisons {
        let spec = catalog.entry(&snapshots.parse(&compared.spec)?).await?;
        let head = catalog.entry(&snapshots.parse(&compared.head)?).await?;
        metas.load(catalog, [&spec.module, &head.module]).await?;
        let spec_meta = metas.kept(catalog, &spec.module).await?.clone();
        let head_meta = metas.kept(catalog, &head.module).await?.clone();
        // What the head builds on, itself last.
        let below = metas.closure(catalog, &head.module).await?;
        let mut modules = vec![spec.module.clone()];
        modules.extend(below.iter().cloned());
        metas.load(catalog, &modules).await?;
        for m in &modules {
            metas.kept(catalog, m).await?;
        }
        let unread: Vec<String> = modules
            .iter()
            .filter(|m| !exports.contains_key(*m))
            .cloned()
            .collect();
        let mut read = catalog.decls_of(&unread).await?;
        for m in unread {
            let read = read.remove(&m);
            if let Some(f) = read.as_ref().and_then(|d| d.failed.as_ref()) {
                failed.insert(m.clone(), f.error.clone());
            }
            let decls = read.map(|d| d.decls).unwrap_or_default();
            let by_name = decls.into_iter().map(|d| (d.name.clone(), d)).collect();
            exports.insert(m, by_name);
        }
        // Each declaration of the head and what it builds on, by name, with
        // its module.
        let mut records: HashMap<String, Declaration> = HashMap::new();
        let mut module_of: HashMap<String, &str> = HashMap::new();
        for m in &below {
            let Some(meta) = metas.held(m) else {
                continue;
            };
            for d in &meta.result.declarations {
                records.insert(d.name.clone(), d.clone());
                module_of.insert(d.name.clone(), m);
            }
        }
        // The spec's declarations, as the head names them: a private one's
        // name carries its module.
        let spec_names: HashSet<String> = spec_meta
            .result
            .declarations
            .iter()
            .filter(|d| !d.generated)
            .map(|d| normalised(&d.name).into_owned())
            .collect();
        let mut start: Vec<String> = head_meta
            .result
            .declarations
            .iter()
            .filter(|d| spec_names.contains(normalised(&d.name).as_ref()))
            .map(|d| d.name.clone())
            .collect();
        start.sort_by_key(|n| spec_order(&spec_meta, n));
        let holes: HashSet<String> = spec
            .holes
            .iter()
            .chain(allow)
            .map(|h| normalised(h).into_owned())
            .collect();
        let reached = closure(&start, &records);
        // What the holes' values reach: the definitions the head adds there,
        // its own or those of a snapshot the spec doesn't build on, are shown
        // by value too, as a hole's value can hide behind one. A hole may be
        // a definition of a snapshot the spec builds on, so each is found
        // wherever the head declares it.
        let mut hole_names: Vec<String> = records
            .keys()
            .filter(|n| holes.contains(normalised(n).as_ref()))
            .cloned()
            .collect();
        hole_names.sort();
        let from_holes: HashSet<String> = closure(&hole_names, &records).into_iter().collect();
        let spec_below = metas.below(catalog, [&spec.module]).await?;
        // The snapshot that stands for each module below the head that the
        // walk reaches.
        let mut unnamed: Vec<String> = reached
            .iter()
            .map(|name| module_of[name])
            .filter(|m| *m != head.module && !snapshot_ids.contains_key(*m))
            .map(str::to_string)
            .collect();
        unnamed.sort();
        unnamed.dedup();
        if !unnamed.is_empty() {
            // Oldest first, so each module's first is its oldest.
            for (e, entry_meta) in catalog.entries_on(&unnamed, &[]).await? {
                snapshot_ids
                    .entry(entry_meta.module)
                    .or_insert_with(|| Some(snapshots.id(&e)));
            }
            for m in unnamed {
                snapshot_ids.entry(m).or_insert(None);
            }
        }
        let mut declarations = Vec::new();
        let mut library = Vec::new();
        for name in &reached {
            let d = &records[name];
            let m = module_of[name];
            let exported = exports.get(m).and_then(|e| e.get(name));
            // What Lean derives of a declaration, such as a recursor or
            // `casesOn`, which the export leaves out.
            let module_exported = exports.get(m).is_some_and(|e| !e.is_empty());
            if d.parent.is_some() && module_exported && exported.is_none() {
                continue;
            }
            let origin = if m != head.module {
                ReviewOrigin::Snapshot
            } else if spec_names.contains(normalised(name).as_ref()) {
                ReviewOrigin::Spec
            } else {
                ReviewOrigin::Head
            };
            let snapshot_id = match origin {
                ReviewOrigin::Snapshot => snapshot_ids[m].clone(),
                _ => None,
            };
            let hole = holes.contains(normalised(name).as_ref());
            let added = origin == ReviewOrigin::Head
                || (origin == ReviewOrigin::Snapshot && !spec_below.contains(m));
            let by_value = hole || (added && from_holes.contains(name) && follows_value(&d.kind));
            if let Some(e) = exported {
                let mut uses: Vec<&String> = e.type_refs.iter().collect();
                if follows_value(&d.kind) {
                    uses.extend(&e.value_refs);
                }
                for u in uses {
                    if !records.contains_key(u) && !u.starts_with('_') && !library.contains(u) {
                        library.push(u.clone());
                    }
                }
            }
            declarations.push(ReviewDeclaration {
                name: name.clone(),
                kind: d.kind.clone(),
                origin,
                snapshot_id,
                signature: exported
                    .and_then(|e| e.printed_signature())
                    .map(String::from),
                type_full: exported.and_then(|e| e.printed_type()).map(String::from),
                hole,
                value: exported.filter(|_| by_value).and_then(|e| e.value.clone()),
                unprinted: match exported {
                    Some(e) if e.error.is_some() => e
                        .error
                        .as_ref()
                        .map(|error| format!("it didn't print: {error}")),
                    _ if !(by_value && follows_value(&d.kind)) => None,
                    Some(e) if e.value.is_some() => None,
                    Some(e) => e
                        .error
                        .as_ref()
                        .map(|error| format!("it didn't print: {error}")),
                    None if !module_exported => Some(
                        "its module has no export: built before exports, or its export \
                         didn't fit"
                            .into(),
                    ),
                    None => Some(match failed.get(m) {
                        Some(error) => format!(
                            "its module's export doesn't have it, and couldn't be made again: \
                             {error}"
                        ),
                        None => "its module's export doesn't have it: built before private \
                                 declarations were exported"
                            .into(),
                    }),
                },
                holes: match by_value {
                    true => d
                        .value_refs
                        .iter()
                        .filter(|r| *r != name && holes.contains(normalised(r).as_ref()))
                        .cloned()
                        .collect(),
                    false => Vec::new(),
                },
            });
        }
        // The spec's, then the head's, then those below by module, each in
        // the order the walk reached them.
        let position: HashMap<&str, usize> = below
            .iter()
            .enumerate()
            .map(|(i, m)| (m.as_str(), i))
            .collect();
        let rank = |d: &ReviewDeclaration| match d.origin {
            ReviewOrigin::Spec => (0, 0),
            ReviewOrigin::Head => (1, 0),
            ReviewOrigin::Snapshot => (2, position.get(module_of[&d.name]).copied().unwrap_or(0)),
        };
        declarations.sort_by_key(rank);
        library.sort();
        out.push(AuditReview {
            spec: compared.spec.clone(),
            head: compared.head.clone(),
            declarations,
            library,
        });
    }
    Ok(out)
}

/// Where `name` comes in the spec module's declarations, by the name
/// without a private one's module.
fn spec_order(spec: &ModuleRecord, name: &str) -> usize {
    let name = normalised(name);
    let declarations = &spec.result.declarations;
    declarations
        .iter()
        .position(|d| normalised(&d.name) == name)
        .unwrap_or(declarations.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::verify::ProofStatus;

    fn decl(name: &str, kind: &str, type_refs: &[&str], value_refs: &[&str]) -> Declaration {
        Declaration {
            name: name.into(),
            kind: kind.into(),
            axioms: Vec::new(),
            assumes: Vec::new(),
            status: ProofStatus::Proved,
            type_hash: "t".into(),
            value_hash: None,
            type_refs: type_refs.iter().map(|s| s.to_string()).collect(),
            value_refs: value_refs.iter().map(|s| s.to_string()).collect(),
            generated: false,
            parent: None,
            negates: None,
            own_axioms: None,
            inherits: Vec::new(),
            own_status: None,
        }
    }

    fn records(decls: Vec<Declaration>) -> HashMap<String, Declaration> {
        decls.into_iter().map(|d| (d.name.clone(), d)).collect()
    }

    #[test]
    fn the_walk_follows_types_and_definitions_but_not_proofs() {
        let mut eq = decl("cost.eq_1", "theorem", &["cost"], &[]);
        eq.generated = true;
        let mut mk = decl("Gate.cnot", "constructor", &["Gate"], &[]);
        mk.parent = Some("Gate".into());
        let all = records(vec![
            decl("t", "theorem", &["good", "cost"], &["helper"]),
            decl("helper", "theorem", &["cost"], &[]),
            decl("good", "def", &[], &["bound", "Gate"]),
            decl("bound", "def", &[], &[]),
            decl("cost", "def", &[], &["count", "cost.eq_1"]),
            decl("count", "def", &[], &["lemma"]),
            decl("lemma", "axiom", &["Gate"], &[]),
            decl("Gate", "inductive", &[], &[]),
            mk,
            eq,
            decl("unused", "def", &[], &[]),
        ]);
        let reached = closure(&["t".to_string()], &all);
        assert_eq!(
            reached,
            [
                "t",
                "good",
                "cost",
                "bound",
                "Gate",
                "count",
                "Gate.cnot",
                "lemma"
            ]
        );
        // A proof of `t` that uses `helper` doesn't bring it in, nor does a
        // name pisa has no record of, such as a library constant's.
        let only: HashMap<String, Declaration> =
            records(vec![decl("u", "theorem", &["Nat"], &["helper"])]);
        assert_eq!(closure(&["u".to_string()], &only), ["u"]);
    }
}
