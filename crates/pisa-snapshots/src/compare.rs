//! Comparing a replacement's declarations with those of the module it
//! replaces, and what that needs: the pure part of a create's and an audit's
//! judgement.

use super::*;

/// The entry a create replaces, as it was found before the build.
pub struct Replacing {
    /// The replaced snapshot, as the request names it.
    pub replaced: Replaces,
    /// The replaced entry, which the new entry's link names.
    pub entry: String,
    /// The replaced entry's module.
    pub module: String,
    /// The declarations of the replaced entry's module.
    pub declarations: Vec<Declaration>,
    /// The snapshot modules it needs, other than its own.
    pub needs: Vec<String>,
    /// The replaced entry's holes, which the replacement may change without
    /// `allow`, and inherits.
    pub holes: Vec<String>,
}

impl Replacing {
    /// The link the new entry records: the replaced entry, and what the
    /// replacement may change.
    pub fn link(&self) -> Replaces {
        Replaces {
            snapshot_id: self.entry.clone(),
            allow: self.replaced.allow.clone(),
        }
    }
}

/// The snapshot modules a replacement swaps: those the replaced module builds
/// on and the replacement doesn't (`lost`), and those the replacement builds
/// on and the replaced module doesn't (`gained`), each with its declarations.
#[derive(Default)]
pub struct Swapped {
    pub lost: Vec<(String, Vec<Declaration>)>,
    pub gained: Vec<(String, Vec<Declaration>)>,
}

/// Compares a replacement's declarations, `now`, with those of the module it
/// replaces: what it filled and changed, or why it can't replace it.
///
/// The declarations are compared by their hashes, and a hash names the
/// constants a type or value uses without saying what they are. When the
/// replacement builds on every snapshot the replaced module does, each of
/// those constants is the same, as Lean refuses an import that declares a
/// name again and Mathlib's are fixed by the version. Otherwise the
/// comparison follows what the replaced declarations use into the snapshots
/// it lost (see [`walk`]).
pub fn compare(
    replacing: &Replacing,
    now: &[Declaration],
    swapped: &Swapped,
) -> Result<Replacement, Vec<DeclarationChange>> {
    let allowed = |name: &str| {
        allows(replacing.replaced.allow.as_ref(), name) || named(&replacing.holes, name)
    };
    let reached = walk(replacing, now, swapped)?;
    let mut refused = Vec::new();
    let mut changed_definitions: Vec<String> = Vec::new();
    // What Lean made from an inductive type that `allow` names.
    let mut derived: Vec<String> = Vec::new();
    for c in statement_changes(&replacing.declarations, now)
        .into_iter()
        .chain(reached)
    {
        if c.change == Change::Value && allowed(&c.name) {
            changed_definitions.push(c.name);
        } else if let Some(p) = parent(replacing, swapped, &c).filter(|p| allowed(p)) {
            if !changed_definitions.contains(&p) {
                changed_definitions.push(p);
            }
            derived.push(c.name);
        } else {
            refused.push(c);
        }
    }
    refused.extend(added_assumptions(replacing, now, swapped));
    if !refused.is_empty() {
        return Err(grouped(refused, |c| parent(replacing, swapped, c)));
    }
    // What changed under an allowed type is listed after it, for review.
    for d in &derived {
        if !changed_definitions.contains(d) {
            changed_definitions.push(d.clone());
        }
    }
    let affected = if changed_definitions.is_empty() {
        Vec::new()
    } else {
        let gained = swapped.gained.iter().flat_map(|(_, d)| d);
        let names: HashSet<&str> = now.iter().map(|d| d.name.as_str()).collect();
        let used = changed_definitions.iter().cloned().collect();
        let (_, theorems) = uses(&used, now.iter().chain(gained));
        theorems
            .into_iter()
            .filter(|t| names.contains(t.as_str()))
            .collect()
    };
    let uses_sorry = |d: &Declaration| d.axioms.iter().any(|a| a == "sorryAx");
    let filled = replacing
        .declarations
        .iter()
        .filter(|d| {
            let name = normalised(&d.name);
            uses_sorry(d)
                && now
                    .iter()
                    .any(|n| normalised(&n.name) == name && !uses_sorry(n))
        })
        .map(|d| d.name.clone())
        .collect();
    Ok(Replacement {
        filled,
        changed_definitions,
        affected,
    })
}

/// Whether `allow` names `name`, both as [`normalised`]: a definition, which
/// may then change its value, or an inductive type, whose constructors and
/// what Lean makes from them may then change.
pub(crate) fn allows(allow: Option<&Allow>, name: &str) -> bool {
    allow.is_some_and(|a| named(&a.definitions, name))
}

/// Whether `names` names `name`, by [`normalised`] name.
pub(crate) fn named(names: &[String], name: &str) -> bool {
    let name = normalised(name);
    names.iter().any(|n| normalised(n) == name)
}

/// What a replacement's `allow` may name, the definitions (declarations with
/// a value) and inductive types of the replaced module and of the snapshots
/// it no longer builds on.
pub fn allowable<'a>(
    replacing: &'a Replacing,
    swapped: &'a Swapped,
) -> impl Iterator<Item = &'a Declaration> {
    let lost = swapped.lost.iter().flat_map(|(_, d)| d);
    replacing
        .declarations
        .iter()
        .chain(lost)
        .filter(|d| changeable(d))
}

/// Whether `allow` or `holes` may name `d`: a definition (a declaration with a
/// value) or an inductive type.
pub fn changeable(d: &Declaration) -> bool {
    d.value_hash.is_some() || d.kind == "inductive"
}

/// The first of `wanted` that isn't among `names`, by [`normalised`] name,
/// with a hint if its private form is.
pub(crate) fn misnamed<'a>(
    wanted: &'a [String],
    names: &HashSet<Cow<'_, str>>,
) -> Option<(&'a str, String)> {
    let name = wanted.iter().find(|n| !names.contains(&normalised(n)))?;
    let private = format!("_private.{}", normalised(name));
    let hint = if names.contains(private.as_str()) {
        format!(" (for a private definition, write {private})")
    } else {
        String::new()
    };
    Some((name, hint))
}

/// The error for a replacement whose `allow` names something other than a
/// definition it may change (see [`allowable`]), by [`normalised`] name.
pub fn allow_error(replacing: &Replacing, swapped: &Swapped) -> Option<Error> {
    let allow = replacing.replaced.allow.as_ref()?;
    let names: HashSet<_> = allowable(replacing, swapped)
        .map(|d| normalised(&d.name))
        .collect();
    let (name, hint) = misnamed(&allow.definitions, &names)?;
    Some(Error::new(
        ErrorKind::BadRequest,
        format!(
            "allow names {name}, which isn't a definition or inductive type of the replaced \
             snapshot or of a snapshot the code no longer builds on{hint}"
        ),
    ))
}

/// Whether Lean made `name` rather than the code, such as the axiom
/// `native_decide` declares: an internal name.
pub(crate) fn lean_made(name: &str) -> bool {
    let name = normalised(name);
    let user = name.strip_prefix("_private.").unwrap_or(&name);
    user.split('.').any(|part| part.starts_with('_'))
}

/// The assumptions `decls` declare: their `axiom`s other than Lean's, by
/// name across modules, with their type hash and their name as declared.
pub(crate) fn declared_assumptions<'a>(
    decls: impl IntoIterator<Item = &'a Declaration>,
) -> HashMap<String, (&'a str, &'a str)> {
    decls
        .into_iter()
        .filter(|d| d.kind == "axiom" && !lean_made(&d.name))
        .map(|d| {
            let key = normalised(&d.name).into_owned();
            (key, (d.type_hash.as_str(), d.name.as_str()))
        })
        .collect()
}

/// A replacement keeps the replaced module's assumptions but adds none: each
/// assumption that it, or a snapshot it builds on and the replaced module
/// doesn't, declares must be declared by the replaced module or a snapshot
/// it no longer builds on, with the same statement. The snapshots both build
/// on are the same. Otherwise a `sorry` could be "filled" by assuming it.
///
/// Names are matched across modules (see [`normalised`]), but a private name
/// is a module's own: one of a snapshot the code newly builds on is matched
/// only with those of the snapshots it no longer builds on, never with the
/// replaced module's own, which a private axiom of the same name and another
/// statement could otherwise pass for.
pub(crate) fn added_assumptions(
    replacing: &Replacing,
    now: &[Declaration],
    swapped: &Swapped,
) -> Vec<DeclarationChange> {
    let own_before = declared_assumptions(&replacing.declarations);
    let lost_before = declared_assumptions(swapped.lost.iter().flat_map(|(_, d)| d));
    let gained_now = declared_assumptions(swapped.gained.iter().flat_map(|(_, d)| d));
    let own_now = declared_assumptions(now);
    let private = |name: &str| name.starts_with("_private.");
    let mut changes: Vec<DeclarationChange> = Vec::new();
    for (own, (key, (statement, name))) in own_now
        .into_iter()
        .map(|a| (true, a))
        .chain(gained_now.into_iter().map(|a| (false, a)))
    {
        let mine = (own || !private(&key))
            .then(|| own_before.get(&key))
            .flatten();
        let change = match (mine, lost_before.get(&key)) {
            // The replaced module's own are compared as its statements.
            (Some(_), _) => None,
            (None, Some((was, _))) if *was == statement => None,
            (None, Some(_)) => Some(Change::Assumption),
            (None, None) => Some(Change::AddedAssumption),
        };
        if let Some(change) = change {
            // As the code names it; a private one with its module.
            let name = if own { key } else { name.to_string() };
            changes.push(DeclarationChange::new(name, change));
        }
    }
    changes.sort_by_key(ToString::to_string);
    changes.dedup();
    changes
}

/// The parent of the declaration a change to what `replacing` declares is
/// about: of the replaced module, or of the lost snapshot it names.
pub(crate) fn parent(
    replacing: &Replacing,
    swapped: &Swapped,
    c: &DeclarationChange,
) -> Option<String> {
    match &c.module {
        None => parent_in(&replacing.declarations, &c.name),
        Some(module) => {
            let (_, decls) = swapped
                .lost
                .iter()
                .find(|(id, _)| snapshot::module(id) == *module)?;
            parent_in(decls, &c.name)
        }
    }
}

/// What `d` uses: its type's refs, and its value's for a definition or an
/// inductive.
pub(crate) fn refs_of(d: &Declaration) -> Vec<&String> {
    let mut refs: Vec<&String> = d.type_refs.iter().collect();
    if d.value_hash.is_some() || d.kind == "inductive" {
        refs.extend(&d.value_refs);
    }
    refs
}

/// Follows what the replaced module's declarations use, through types and
/// values, into the snapshots the replacement no longer builds on, and
/// compares each constant reached there with the one of the same name (see
/// [`normalised`]) that the replacement declares or builds on now. What it
/// can't compare, because a private name is ambiguous, is an error: the
/// replacement is refused.
pub(crate) fn walk(
    replacing: &Replacing,
    now: &[Declaration],
    swapped: &Swapped,
) -> Result<Vec<DeclarationChange>, Vec<DeclarationChange>> {
    if swapped.lost.is_empty() {
        return Ok(Vec::new());
    }
    // Refs name constants as they are, so the old side is found exactly.
    let old: HashMap<&str, (&str, &Declaration)> = swapped
        .lost
        .iter()
        .flat_map(|(m, decls)| {
            decls
                .iter()
                .map(move |d| (d.name.as_str(), (m.as_str(), d)))
        })
        .collect();
    let mut new: HashMap<std::borrow::Cow<str>, Option<&Declaration>> = HashMap::new();
    for d in now.iter().chain(swapped.gained.iter().flat_map(|(_, d)| d)) {
        new.entry(normalised(&d.name))
            .and_modify(|found| *found = None)
            .or_insert(Some(d));
    }
    let mut todo: Vec<&String> = replacing.declarations.iter().flat_map(refs_of).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let (mut reached, mut ambiguous) = (Vec::new(), Vec::new());
    while let Some(r) = todo.pop() {
        if !seen.insert(r) {
            continue;
        }
        // Neither from a lost snapshot: the replaced module's own, which are
        // compared already, or from a snapshot both build on.
        let Some(&(module, c)) = old.get(r.as_str()) else {
            continue;
        };
        let from = |change| DeclarationChange {
            module: Some(snapshot::module(module)),
            ..DeclarationChange::new(c.name.clone(), change)
        };
        let change = match new.get(&normalised(r)) {
            None => Some(Change::Missing),
            Some(None) => {
                ambiguous.push(from(Change::Ambiguous));
                None
            }
            Some(Some(n)) if n.kind != c.kind => Some(Change::Kind),
            Some(Some(n)) if n.type_hash != c.type_hash => Some(Change::Type),
            Some(Some(n)) if n.value_hash != c.value_hash => Some(Change::Value),
            Some(Some(_)) => None,
        };
        if let Some(change) = change {
            reached.push(from(change));
        }
        todo.extend(refs_of(c));
    }
    if ambiguous.is_empty() {
        Ok(reached)
    } else {
        ambiguous.dedup();
        Err(ambiguous)
    }
}

/// What among `items` uses a name in `used`, directly or through each other,
/// by the refs they record: the declarations other than theorems whose type
/// or value does, which then count as used too, and the theorems whose type
/// does. Names are matched as [`normalised`].
pub fn uses<'a, I>(used: &HashSet<String>, items: I) -> (Vec<String>, Vec<String>)
where
    I: IntoIterator<Item = &'a Declaration>,
    I::IntoIter: Clone,
{
    let items = items.into_iter();
    let theorem = |d: &Declaration| d.kind == "theorem";
    let mut names: HashSet<String> = used.iter().map(|u| normalised(u).into_owned()).collect();
    let mut users: Vec<String> = Vec::new();
    let is_used = |r: &String, names: &HashSet<String>| names.contains(normalised(r).as_ref());
    loop {
        let before = users.len();
        for d in items.clone().filter(|d| !theorem(d)) {
            let refs = d.type_refs.iter().chain(&d.value_refs);
            if !names.contains(normalised(&d.name).as_ref())
                && refs.clone().any(|r| is_used(r, &names))
            {
                names.insert(normalised(&d.name).into_owned());
                users.push(d.name.clone());
            }
        }
        if users.len() == before {
            break;
        }
    }
    let theorems = items
        .filter(|d| theorem(d) && d.type_refs.iter().any(|r| is_used(r, &names)))
        .map(|d| d.name.clone())
        .collect();
    (users, theorems)
}

/// The worst status among a module's declarations, `decls`.
pub fn worst_status(decls: &[Declaration]) -> ProofStatus {
    let statuses = decls.iter().map(|d| d.status);
    statuses.max().unwrap_or(ProofStatus::Proved)
}
