//! Readers' flags on search's descriptions of declarations: each says a
//! description misstates its declaration, e.g. leaves out a hypothesis. A
//! flag is on the description as it was, by its hash, so one written again
//! starts with none.

use std::collections::{HashMap, HashSet};

use crate::backend::dispatch;
use crate::catalog::now_micros;
use crate::db::{Db, DbError};

/// One reader's flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptionFlag {
    pub name: String,
    /// The hash of the description flagged.
    pub description_hash: String,
    /// What the reader says is wrong with it.
    pub reason: String,
    pub created_micros: u64,
}

#[derive(Clone)]
pub struct DescriptionFlags {
    db: Db,
}

impl DescriptionFlags {
    pub fn new(db: Db) -> DescriptionFlags {
        DescriptionFlags { db }
    }

    /// Flags `name`'s description, whose hash is `description_hash`.
    pub async fn add(
        &self,
        name: &str,
        description_hash: &str,
        reason: &str,
    ) -> Result<(), DbError> {
        let created = i64::try_from(now_micros()).unwrap_or(i64::MAX);
        dispatch!(
            &self.db,
            add_description_flag(name, description_hash, reason, created)
        )
    }

    /// How many flags each of `keys`, a name and a description hash, has;
    /// those with none are left out.
    pub async fn counts(
        &self,
        keys: &[(String, String)],
    ) -> Result<HashMap<(String, String), u32>, DbError> {
        let wanted: HashSet<&(String, String)> = keys.iter().collect();
        let mut names: Vec<String> = keys.iter().map(|(name, _)| name.clone()).collect();
        names.sort();
        names.dedup();
        let rows = dispatch!(&self.db, description_flag_counts(&names))?;
        Ok(rows
            .into_iter()
            .map(|(name, hash, n)| ((name, hash), u32::try_from(n).unwrap_or(u32::MAX)))
            .filter(|(key, _)| wanted.contains(key))
            .collect())
    }

    /// Every flag, oldest first.
    pub async fn all(&self) -> Result<Vec<DescriptionFlag>, DbError> {
        let rows = dispatch!(&self.db, all_description_flags())?;
        Ok(rows
            .into_iter()
            .map(
                |(name, description_hash, reason, created)| DescriptionFlag {
                    name,
                    description_hash,
                    reason,
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
    /// that is set; with a name of the test's own, as tests share it.
    async fn flags() -> Vec<(DescriptionFlags, String)> {
        let mut dbs = vec![Db::memory().await];
        if let Ok(url) = std::env::var("PISA_TEST_DATABASE_URL") {
            dbs.push(Db::connect(Some(&url)).await.unwrap());
        }
        dbs.into_iter()
            .map(|db| {
                let name = format!("Real.logb_{}", uuid::Uuid::new_v4().simple());
                (DescriptionFlags::new(db), name)
            })
            .collect()
    }

    #[tokio::test]
    async fn flags_count_per_description() {
        for (f, name) in flags().await {
            let key = |hash: &str| (name.clone(), hash.to_string());
            assert!(f.counts(&[key("h1")]).await.unwrap().is_empty());
            f.add(&name, "h1", "leaves out 1 < b").await.unwrap();
            f.add(&name, "h1", "false at b = 1/2").await.unwrap();
            // A description written since has flags of its own.
            f.add(&name, "h2", "still wrong").await.unwrap();
            let counts = f.counts(&[key("h1"), key("h2"), key("h3")]).await.unwrap();
            assert_eq!(counts.get(&key("h1")), Some(&2));
            assert_eq!(counts.get(&key("h2")), Some(&1));
            assert_eq!(counts.get(&key("h3")), None);

            let mine: Vec<DescriptionFlag> = (f.all().await.unwrap().into_iter())
                .filter(|flag| flag.name == name)
                .collect();
            let reasons: Vec<(&str, &str)> = mine
                .iter()
                .map(|flag| (flag.description_hash.as_str(), flag.reason.as_str()))
                .collect();
            assert_eq!(
                reasons,
                [
                    ("h1", "leaves out 1 < b"),
                    ("h1", "false at b = 1/2"),
                    ("h2", "still wrong")
                ]
            );
            assert!(mine.iter().all(|flag| flag.created_micros > 0));
        }
    }

    #[tokio::test]
    async fn flags_are_shared_across_scopes() {
        use crate::scope::{Scope, within};
        for (f, name) in flags().await {
            let key = (name.clone(), "h1".to_string());
            within(Scope::parse("user=a").unwrap(), async {
                f.add(&name, "h1", "leaves out 1 < b").await.unwrap();
            })
            .await;
            within(Scope::parse("user=b").unwrap(), async {
                let counts = f.counts(std::slice::from_ref(&key)).await.unwrap();
                assert_eq!(counts.get(&key), Some(&1));
            })
            .await;
            let counts = f.counts(std::slice::from_ref(&key)).await.unwrap();
            assert_eq!(counts.get(&key), Some(&1));
        }
    }
}
