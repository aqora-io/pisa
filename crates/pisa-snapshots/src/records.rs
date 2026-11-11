//! The module records a read of the catalog uses, read in batches.

use std::collections::{BTreeSet, HashMap, HashSet};

use pisa_db::catalog::{Catalog, ModuleRecord, closure_order, unknown_module};
use pisa_protocol::api::Error;

/// The module records a read uses, each read from the catalog once and many
/// at a time, with the imports of what they build on. A read over a
/// thousand modules is then a few queries, and a module's closure is ordered
/// from imports shared among them rather than read with each record.
#[derive(Default)]
pub(crate) struct Records {
    /// By module; `None` for one that is gone.
    records: HashMap<String, Option<ModuleRecord>>,
    /// The imports of each module whose closure is loaded, in the header's
    /// order.
    imports: HashMap<String, Vec<String>>,
    /// The modules whose whole closure `imports` holds.
    closed: HashSet<String>,
}

impl Records {
    /// Reads the records of `ms` that it doesn't hold yet, in one batch.
    pub(crate) async fn load<'a>(
        &mut self,
        catalog: &Catalog,
        ms: impl IntoIterator<Item = &'a String>,
    ) -> Result<(), Error> {
        let missing: HashSet<&String> = ms
            .into_iter()
            .filter(|m| !self.records.contains_key(*m))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let missing: Vec<String> = missing.into_iter().cloned().collect();
        let mut found = catalog.records(&missing).await?;
        for m in missing {
            let record = found.remove(&m);
            self.records.insert(m, record);
        }
        Ok(())
    }

    /// Module `m`'s record, read if it isn't held; `None` if it is gone.
    pub(crate) async fn get(
        &mut self,
        catalog: &Catalog,
        m: &str,
    ) -> Result<Option<&ModuleRecord>, Error> {
        if !self.records.contains_key(m) {
            self.load(catalog, [&m.to_string()]).await?;
        }
        Ok(self.held(m))
    }

    /// [`get`](Self::get) for a module that must be kept: `not_found` if it
    /// is gone.
    pub(crate) async fn kept(
        &mut self,
        catalog: &Catalog,
        m: &str,
    ) -> Result<&ModuleRecord, Error> {
        self.get(catalog, m).await?.ok_or_else(|| unknown_module(m))
    }

    /// Module `m`'s record, if it is read and kept.
    pub(crate) fn held(&self, m: &str) -> Option<&ModuleRecord> {
        self.records.get(m).and_then(Option::as_ref)
    }

    /// Reads the imports of what each of `ms` builds on, for those whose
    /// closure it doesn't hold yet, in one query.
    pub(crate) async fn load_closures<'a>(
        &mut self,
        catalog: &Catalog,
        ms: impl IntoIterator<Item = &'a String>,
    ) -> Result<(), Error> {
        let open: HashSet<&String> = ms
            .into_iter()
            .filter(|m| !self.closed.contains(*m))
            .collect();
        if open.is_empty() {
            return Ok(());
        }
        let open: Vec<String> = open.into_iter().cloned().collect();
        let graph = catalog.import_graph(&open).await?;
        // What a module builds on is in the graph too, so each module it
        // reaches is closed.
        for (m, parents) in &graph {
            self.closed.insert(m.clone());
            self.closed.extend(parents.iter().cloned());
        }
        self.closed.extend(open);
        self.imports.extend(graph);
        Ok(())
    }

    /// What module `m` builds on, each after what it imports, `m` last.
    pub(crate) async fn closure(
        &mut self,
        catalog: &Catalog,
        m: &str,
    ) -> Result<Vec<String>, Error> {
        self.load_closures(catalog, [&m.to_string()]).await?;
        Ok(closure_order(m, &self.imports))
    }

    /// The modules `ms` build on, `ms` among them.
    pub(crate) async fn below<'a>(
        &mut self,
        catalog: &Catalog,
        ms: impl IntoIterator<Item = &'a String>,
    ) -> Result<BTreeSet<String>, Error> {
        let ms: Vec<String> = ms.into_iter().cloned().collect();
        self.load_closures(catalog, &ms).await?;
        let mut seen = BTreeSet::new();
        let mut stack = ms;
        while let Some(m) = stack.pop() {
            if seen.insert(m.clone()) {
                stack.extend(self.imports.get(&m).into_iter().flatten().cloned());
            }
        }
        Ok(seen)
    }
}

#[cfg(test)]
impl FromIterator<(String, ModuleRecord)> for Records {
    fn from_iter<I: IntoIterator<Item = (String, ModuleRecord)>>(records: I) -> Self {
        Records {
            records: records.into_iter().map(|(m, r)| (m, Some(r))).collect(),
            ..Default::default()
        }
    }
}
