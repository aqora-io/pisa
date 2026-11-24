//! The environments workers have served: each version's Lake project, as a
//! worker of it recorded when it registered. A record outlives its pool, so
//! what a snapshot or a check of that version ran on can still be read, and
//! built again.

use std::collections::BTreeMap;

use pisa_protocol::api::PoolPackage;

use crate::backend::dispatch;
use crate::catalog::now_micros;
use crate::db::{Db, DbError};

/// A version's Lake project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    pub version: String,
    /// The Lean toolchain it pins, e.g. `leanprover/lean4:v4.36.0`.
    pub toolchain: String,
    /// The Lake packages its manifest pins.
    pub packages: Vec<PoolPackage>,
    /// Its files by name (see `pisa_protocol::version::project_files`).
    pub files: BTreeMap<String, String>,
    /// When a worker first recorded it.
    pub created_micros: u64,
}

#[derive(Clone)]
pub struct Environments {
    db: Db,
}

impl Environments {
    pub fn new(db: Db) -> Environments {
        Environments { db }
    }

    /// Records `version`'s project. A version recorded before keeps when it
    /// was, and takes this project: a label that hashes its project names
    /// one, and one set by hand names what its workers serve now.
    pub async fn record(
        &self,
        version: &str,
        toolchain: &str,
        packages: &[PoolPackage],
        files: &BTreeMap<String, String>,
    ) -> Result<(), DbError> {
        let packages = serde_json::to_string(packages).unwrap_or_else(|_| "[]".into());
        let files = serde_json::to_string(files).unwrap_or_else(|_| "{}".into());
        let created = i64::try_from(now_micros()).unwrap_or(i64::MAX);
        dispatch!(
            &self.db,
            record_environment(version, toolchain, &packages, &files, created)
        )
    }

    /// Every environment recorded, by version.
    pub async fn list(&self) -> Result<Vec<Environment>, DbError> {
        let mut rows = dispatch!(&self.db, all_environments())?;
        // Here, as a database's collation orders them its own way.
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows
            .into_iter()
            .map(
                |(version, toolchain, packages, files, created)| Environment {
                    version,
                    toolchain,
                    packages: serde_json::from_str(&packages).unwrap_or_default(),
                    files: serde_json::from_str(&files).unwrap_or_default(),
                    created_micros: u64::try_from(created).unwrap_or(0),
                },
            )
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory SQLite, and the PostgreSQL at `PISA_TEST_DATABASE_URL` if
    /// that is set; with a version of the test's own, as tests share it.
    async fn environments() -> Vec<(Environments, String)> {
        let mut dbs = vec![Db::memory().await];
        if let Ok(url) = std::env::var("PISA_TEST_DATABASE_URL") {
            dbs.push(Db::connect(Some(&url)).await.unwrap());
        }
        dbs.into_iter()
            .map(|db| {
                let version = format!("v4.36.0-{}", uuid::Uuid::new_v4().simple());
                (Environments::new(db), version)
            })
            .collect()
    }

    #[tokio::test]
    async fn an_environment_is_recorded_once_and_keeps_its_latest_project() {
        for (e, version) in environments().await {
            let mine = || async {
                let all = e.list().await.unwrap();
                let versions: Vec<&str> = all.iter().map(|x| x.version.as_str()).collect();
                assert!(versions.is_sorted(), "{versions:?}");
                all.into_iter().find(|x| x.version == version)
            };
            assert_eq!(mine().await, None);
            let cslib = PoolPackage {
                name: "cslib".into(),
                rev: "abc".into(),
                ..PoolPackage::default()
            };
            let files = |root: &str| -> BTreeMap<String, String> {
                [
                    ("lean-toolchain", "leanprover/lean4:v4.36.0\n"),
                    ("P.lean", root),
                ]
                .map(|(name, text)| (name.to_string(), text.to_string()))
                .into()
            };
            let toolchain = "leanprover/lean4:v4.36.0";
            let packages = std::slice::from_ref(&cslib);
            e.record(&version, toolchain, packages, &files("import Cslib\n"))
                .await
                .unwrap();
            let first = mine().await.unwrap();
            assert_eq!(first.toolchain, toolchain);
            assert_eq!(first.packages, packages);
            assert_eq!(first.files, files("import Cslib\n"));
            assert!(first.created_micros > 0);

            // Another worker of the version registers.
            e.record(&version, toolchain, packages, &files("import Mathlib\n"))
                .await
                .unwrap();
            let again = mine().await.unwrap();
            assert_eq!(again.files, files("import Mathlib\n"));
            assert_eq!(again.created_micros, first.created_micros);
        }
    }
}
