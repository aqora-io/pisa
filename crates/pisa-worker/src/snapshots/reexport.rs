//! Exporting module-system snapshots again whose export a build made before
//! [`EXPORT_FORMAT`](crate::catalog::EXPORT_FORMAT), which left out their
//! non-public declarations, so that the review list sees a hole's value
//! behind one.

use super::*;

/// The lease a pass is made under, so that one worker at a time makes it.
pub const REEXPORT_LEASE: &str = "snapshot-reexport";

/// How long the lease lasts unless renewed: before each module.
const LEASE_TTL: Duration = Duration::from_secs(600);

/// How many modules a page of the pass reads.
const PAGE: u32 = 50;

/// How long the export of a module may take once its imports are loaded.
const EXPORT_TIME: Duration = Duration::from_secs(300);

impl Snapshots {
    /// Exports again each module-system module whose stored export is older
    /// than [`EXPORT_FORMAT`](crate::catalog::EXPORT_FORMAT), one at a time,
    /// under [`REEXPORT_LEASE`] held as `owner`, which it ends when done: it
    /// stops when another holds it.
    /// A module whose export fails is logged and left to the next pass.
    /// Returns how many it stored.
    pub async fn reexport(&self, pool: &Arc<Pool>, owner: &str) -> Result<usize, Error> {
        let mut stored = 0;
        let mut after: Option<String> = None;
        loop {
            let page = self
                .catalog
                .stale_exports_after(after.as_deref(), PAGE)
                .await?;
            let Some((last, _)) = page.last() else {
                break;
            };
            after = Some(last.clone());
            for (m, build) in page {
                if self
                    .catalog
                    .take_lease(REEXPORT_LEASE, owner, LEASE_TTL)
                    .await?
                    .is_none()
                {
                    return Ok(stored);
                }
                match self.reexport_one(pool, &m, &build).await {
                    Ok(true) => stored += 1,
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(module = %m, "exporting the module again failed: {e}")
                    }
                }
            }
        }
        if after.is_some() {
            // Ended, so that the next pass needn't wait for it.
            self.catalog
                .take_lease(REEXPORT_LEASE, owner, Duration::ZERO)
                .await?;
        }
        Ok(stored)
    }

    /// Exports module `m` again, in a slot whose process is retired first,
    /// as the export loads the module's closure; stores the export if the
    /// module's own build is still `build`, and says whether it did.
    async fn reexport_one(&self, pool: &Arc<Pool>, m: &str, build: &str) -> Result<bool, Error> {
        self.ensure_one(m).await?;
        let meta = self.catalog.module(m).await?;
        if meta.modules.last().map(|own| own.build.as_str()) != Some(build) {
            return Ok(false);
        }
        let mut slot = pool.acquire("").await;
        slot.retire_and_wait("evict").await;
        let timeout = slot.import_timeout() + EXPORT_TIME;
        let export = slot.export(&snapshot::module(m), timeout).await;
        drop(slot);
        let export = export.map_err(|e| Error::new(ErrorKind::Crash, e))?;
        let decls = parse_export(export).ok_or_else(|| internal("unreadable export"))?;
        let decls = SnapshotDecls::new(&meta, decls);
        self.catalog.replace_decls(m, build, &decls).await
    }
}
