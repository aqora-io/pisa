//! Reuses LeanSearch v2's informal descriptions of Mathlib (Apache-2.0,
//! `FrenzyMath/lsv2-mathlib-v4.28.0-rc1-jsonl`; attribution in
//! `search/NOTICE`).
//!
//! Their corpus describes Mathlib v4.28.0-rc1. A description is reused for
//! one of our declarations when the names match and the constants visible in
//! their type are all among the constants of ours (following our deprecation
//! redirects). Pretty-printed text isn't compared: it differs across versions.

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::Path;

use anyhow::Context;
use pisa_protocol::search::Informal;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::corpus::Decl;
use crate::informal_store::Entry;
use crate::text::lean_identifiers;

pub const SOURCE: &str = "lsv2-v4.28.0-rc1";
pub const URL: &str = "https://huggingface.co/datasets/FrenzyMath/lsv2-mathlib-v4.28.0-rc1-jsonl/resolve/c5f07cc71d42fa6a54ed16e2e5d3eb6b657367a0/lsv2-mathlib-v4.28.0-rc1.jsonl";
pub const SHA256: &str = "457ab5db0288307d8910215d73c3917149164edb4e3e4bdc1e5daab6f895f634";

#[derive(Deserialize)]
struct Record {
    name: Vec<String>,
    #[serde(rename = "type")]
    ty: String,
    informal_name: Option<String>,
    informal_description: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub records: usize,
    pub matched: usize,
    /// Same name, but their type mentions constants ours doesn't.
    pub type_mismatch: usize,
    /// No declaration of that name in our corpus.
    pub missing: usize,
    /// Our Mathlib declarations still without a description.
    pub mathlib_uncovered: usize,
}

/// Downloads the pinned corpus to `path` unless it is already there, and
/// checks its SHA-256 either way.
pub async fn fetch(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        tracing::info!(url = URL, "downloading the LeanSearch v2 corpus");
        let bytes = reqwest::get(URL).await?.error_for_status()?.bytes().await?;
        std::fs::write(path, &bytes)?;
    }
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hex::encode(hasher.finalize());
    anyhow::ensure!(
        digest == SHA256,
        "{} has sha256 {digest}, expected {SHA256}",
        path.display()
    );
    Ok(())
}

/// Matches the LeanSearch v2 records in `path` onto `decls`.
pub fn import(path: &Path, decls: &[Decl]) -> anyhow::Result<(Vec<Entry>, Report)> {
    let by_name: HashMap<&str, &Decl> = decls.iter().map(|d| (d.name.as_str(), d)).collect();
    let renamed: HashMap<&str, &str> = decls
        .iter()
        .filter_map(|d| Some((d.name.as_str(), d.deprecated_to.as_deref()?)))
        .collect();
    let reader = std::io::BufReader::new(
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?,
    );
    let mut report = Report::default();
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let r: Record = serde_json::from_str(&line)?;
        report.records += 1;
        let name = r.name.join(".");
        let Some(d) = by_name.get(name.as_str()) else {
            report.missing += 1;
            continue;
        };
        let (Some(informal_name), Some(description)) = (r.informal_name, r.informal_description)
        else {
            continue;
        };
        if !compatible(&r.ty, d, &by_name, &renamed) {
            report.type_mismatch += 1;
            continue;
        }
        report.matched += 1;
        entries.push(Entry {
            content_hash: d.content_hash.clone(),
            name: d.name.clone(),
            informal: Informal {
                informal_name,
                statement_nl: description,
                source: SOURCE.into(),
                ..Default::default()
            },
            synthetic_queries: Vec::new(),
            meta: None,
        });
    }
    let covered: HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    report.mathlib_uncovered = decls
        .iter()
        .filter(|d| d.library == "Mathlib" && !covered.contains(d.name.as_str()))
        .count();
    Ok((entries, report))
}

/// Every constant their type shows (that exists in our corpus) must be among
/// ours, possibly under its new name.
fn compatible(
    their_type: &str,
    ours: &Decl,
    by_name: &HashMap<&str, &Decl>,
    renamed: &HashMap<&str, &str>,
) -> bool {
    let our_refs: HashSet<&str> = ours.type_refs.iter().map(String::as_str).collect();
    lean_identifiers(their_type)
        .iter()
        .filter(|id| id.contains('.') && by_name.contains_key(id.as_str()))
        .all(|id| {
            our_refs.contains(id.as_str())
                || renamed
                    .get(id.as_str())
                    .is_some_and(|new| our_refs.contains(new))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::tests::decl;

    #[test]
    fn matches_by_name_and_visible_constants() {
        let mut decls = vec![
            decl("Matrix.trace", "def", "Matrix.trace …", &["Matrix"]),
            decl(
                "Matrix.trace_one",
                "theorem",
                "…",
                &["Matrix.trace", "Matrix.one"],
            ),
            decl("Matrix.trace_zero", "theorem", "…", &["Matrix.trace"]),
            decl("Matrix.one", "def", "…", &[]),
            decl("Matrix.zero", "def", "…", &[]),
        ];
        crate::corpus::compute_hashes(&mut decls);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lsv2.jsonl");
        let line = |name: &str, ty: &str| {
            serde_json::json!({"name": name.split('.').collect::<Vec<_>>(), "type": ty,
                "informal_name": format!("about {name}"), "informal_description": "text"})
            .to_string()
        };
        std::fs::write(
            &path,
            [
                line("Matrix.trace_one", "Matrix.trace Matrix.one = n"),
                // Their version of the statement mentions a constant ours doesn't.
                line("Matrix.trace_zero", "Matrix.trace Matrix.zero = 0"),
                line("Matrix.gone", "True"),
            ]
            .join("\n"),
        )
        .unwrap();
        let (entries, report) = import(&path, &decls).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Matrix.trace_one");
        assert_eq!(entries[0].content_hash, decls[1].content_hash);
        assert_eq!(
            report,
            Report {
                records: 3,
                matched: 1,
                type_mismatch: 1,
                missing: 1,
                mathlib_uncovered: 4
            }
        );
    }
}
