//! A run as one read of the catalog gives it: every entry of the runs some
//! entries are on, with its link, so that walks along the run need no more
//! queries and see one state of it.

use super::*;

pub(crate) struct Run {
    entries: HashMap<String, EntryMeta>,
    /// What replaces each entry, oldest first.
    replacers: HashMap<String, Vec<String>>,
}

impl Run {
    /// The runs that `entries` are on.
    pub(crate) async fn read(catalog: &Catalog, entries: &[String]) -> Result<Run, Error> {
        let entries = catalog.runs(entries).await?;
        let mut replacers: HashMap<String, Vec<String>> = HashMap::new();
        for (e, meta) in &entries {
            if let Some(replaces) = &meta.replaces {
                replacers
                    .entry(replaces.snapshot_id.clone())
                    .or_default()
                    .push(e.clone());
            }
        }
        for found in replacers.values_mut() {
            found.sort_by_key(|e| (entries[e].created_micros, e.clone()));
        }
        Ok(Run { entries, replacers })
    }

    /// The entries read.
    pub(crate) fn ids(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    /// Entry `e`, if it is kept.
    pub(crate) fn entry(&self, e: &str) -> Option<&EntryMeta> {
        self.entries.get(e)
    }

    /// What replaces `e`, oldest first, each with its entry.
    pub(crate) fn replacers(&self, e: &str) -> Vec<(String, EntryMeta)> {
        let found = self.replacers.get(e).into_iter().flatten();
        found
            .map(|r| (r.clone(), self.entries[r].clone()))
            .collect()
    }

    /// The entries nothing replaces, reached from `e` through what replaces
    /// it, oldest first.
    pub(crate) fn heads_from(&self, e: &str) -> Vec<String> {
        let mut todo = vec![e.to_string()];
        let mut seen = HashSet::new();
        let mut heads = Vec::new();
        while let Some(e) = todo.pop() {
            if !seen.insert(e.clone()) {
                continue;
            }
            match self.replacers.get(&e) {
                Some(replacers) => todo.extend(replacers.iter().cloned()),
                None => {
                    let created = self.entries.get(&e).map_or(0, |m| m.created_micros);
                    heads.push((created, e));
                }
            }
        }
        heads.sort();
        heads.into_iter().map(|(_, e)| e).collect()
    }
}
