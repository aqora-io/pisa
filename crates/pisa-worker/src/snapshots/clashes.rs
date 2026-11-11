//! Modules verified before creates refused a header whose imports declare one
//! name twice: what Lean makes of such a module depends on the order of its
//! imports, so it loses `verified`, with what builds on it, once per version.

use super::*;

/// The lease the check is made under, so that one worker makes it.
const CLASH_LEASE: &str = "snapshot-clash-check";

/// How long the lease lasts unless renewed: before each page.
const LEASE_TTL: Duration = Duration::from_secs(600);

/// How many modules a page of the check reads.
const PAGE: u32 = 500;

impl Snapshots {
    /// Clears `verified` on each module whose imports declare one name
    /// twice, and on what builds on it, with the clash as the reason, once
    /// for this version: under its lease held as `owner`, and only until it
    /// has been done. A module cleared already is looked at too, so a pass
    /// stopped half way clears what builds on it. Returns how many modules
    /// it cleared.
    pub async fn unverify_old_clashes(&self, owner: &str) -> Result<usize, Error> {
        if self.catalog.clashes_checked().await? {
            return Ok(0);
        }
        let mut cleared = 0;
        let mut after: Option<String> = None;
        loop {
            if self
                .catalog
                .take_lease(CLASH_LEASE, owner, LEASE_TTL)
                .await?
                .is_none()
            {
                return Ok(cleared);
            }
            let page = self.catalog.modules_after(after.as_deref(), PAGE).await?;
            let Some((last, _)) = page.last() else {
                break;
            };
            after = Some(last.clone());
            let ids: Vec<String> = page.iter().map(|(m, _)| m.clone()).collect();
            let records = self.catalog.records(&ids).await?;
            for (m, build) in page {
                let Some(record) = records.get(&m).filter(|r| r.imports.len() > 1) else {
                    continue;
                };
                if let Some(e) = self.reader.clashes(&record.imports).await? {
                    let gone = self.catalog.unverify(&m, &build, &e.message).await?;
                    tracing::warn!(module = %m, cleared = gone.len(), "a verified module's imports clash");
                    cleared += gone.len();
                }
            }
        }
        self.catalog.mark_clashes_checked().await?;
        self.catalog
            .take_lease(CLASH_LEASE, owner, Duration::ZERO)
            .await?;
        Ok(cleared)
    }
}
