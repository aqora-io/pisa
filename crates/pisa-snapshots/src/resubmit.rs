//! A resubmit's plan: the heads of an amended spec's run, each compared with
//! the spec that amends it as an audit against that spec would compare them.

use pisa_protocol::api::{ResubmitItem, ResubmitSnapshotsResponse};

use super::*;

impl Reader {
    /// The plan of carrying the run of spec `from` over to spec `to`, which
    /// must amend it: each head of `from`'s run (not `from` itself), whether
    /// it keeps `to`'s statements, and the key its create takes. `from` may
    /// be deleted.
    pub async fn resubmit_plan(
        &self,
        from: &str,
        to: &str,
    ) -> Result<ResubmitSnapshotsResponse, Error> {
        let (from_e, to_e) = (self.parse(from)?, self.parse(to)?);
        let amends = self
            .catalog
            .amendments_of(std::slice::from_ref(&to_e))
            .await?;
        if !amends.iter().any(|(e, a)| *e == to_e && a.amends == from_e) {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("{to} doesn't amend {from}: create it with amends.snapshot_id {from}"),
            ));
        }
        let to_meta = self.catalog.entry(&to_e).await?;
        let run = Run::read(&self.catalog, std::slice::from_ref(&from_e)).await?;
        let mut records = Records::default();
        let mut items = Vec::new();
        for head in run.heads_from(&from_e) {
            let Some(head_meta) = run.entry(&head).filter(|_| head != from_e) else {
                continue;
            };
            let head_module = head_meta.module.clone();
            let compared = self
                .rederive(&to_meta.module, &head_module, &to_meta.holes, &mut records)
                .await?;
            let mut item = ResubmitItem {
                snapshot_id: self.id(&head),
                module: snapshot::module(&head_module),
                key: format!("resubmit:{from_e}:{to_e}:{head}"),
                passes: false,
                reasons: Vec::new(),
                changes: Vec::new(),
                replacement: None,
                new_snapshot_id: None,
                check: None,
            };
            match compared {
                Some(Ok(replacement)) => {
                    item.passes = true;
                    item.replacement = Some(replacement);
                }
                Some(Err(changes)) => {
                    item.reasons = changes.iter().map(ToString::to_string).collect();
                    item.changes = changes;
                }
                None => {
                    for (e, m) in [(&to_e, &to_meta.module), (&head, &head_module)] {
                        if records.held(m).is_none_or(|r| !r.verified) {
                            let id = self.id(e);
                            item.reasons.push(format!(
                                "{id} isn't verified: POST /snapshots/{id}/verify, then resubmit"
                            ));
                        }
                    }
                }
            }
            items.push(item);
        }
        Ok(ResubmitSnapshotsResponse {
            ok: items.iter().all(|i| i.passes),
            items,
        })
    }
}
