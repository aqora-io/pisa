//! A request's scope: labels that bound what the request creates and what it
//! reads. A request sends them as the header `Pisa-Scope: user=42,org=7`.
//! The server stamps them on what the request creates, requires them of
//! everything the request names or reaches, answering `not_found` otherwise,
//! and keys run names and idempotency keys by them. An empty scope, a request
//! without the header, bounds nothing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::labels::{MAX_LABELS, check_label_key};

/// The header a request sends its scope in.
pub const HEADER: &str = "pisa-scope";

/// The longest value of a scope's label, in bytes, as for any label.
const MAX_VALUE: usize = 512;

/// A request's scope: labels, each key once.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Scope(BTreeMap<String, String>);

impl Scope {
    /// The scope of `labels`, checked as the header's are.
    pub fn new(labels: BTreeMap<String, String>) -> Result<Scope, String> {
        if labels.len() > MAX_LABELS {
            return Err(format!(
                "a scope has {} labels; it can have at most {MAX_LABELS}",
                labels.len()
            ));
        }
        for (key, value) in &labels {
            check_label_key(key)?;
            if value.is_empty() || value.len() > MAX_VALUE {
                return Err(format!(
                    "the scope's label {key} must have a value of 1 to {MAX_VALUE} bytes"
                ));
            }
            if value.contains(',') || value.contains('\0') {
                return Err(format!(
                    "the scope's label {key} has a ',' or a NUL character in its value"
                ));
            }
        }
        Ok(Scope(labels))
    }

    /// The scope a `Pisa-Scope` header gives: `key=value` pairs separated by
    /// commas, each key once. A blank header is no scope.
    pub fn parse(header: &str) -> Result<Scope, String> {
        let mut labels = BTreeMap::new();
        for pair in header.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((key, value)) = pair.split_once('=') else {
                return Err(format!(
                    "Pisa-Scope takes key=value pairs separated by commas, not {pair:?}"
                ));
            };
            let (key, value) = (key.trim(), value.trim());
            if labels.insert(key.to_string(), value.to_string()).is_some() {
                return Err(format!("Pisa-Scope names {key} twice"));
            }
        }
        Scope::new(labels)
    }

    /// Whether the scope bounds nothing.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.0
    }

    /// Its labels as pairs, as a listing's filters are.
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// Whether `labels` has every label of the scope.
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.0.iter().all(|(k, v)| labels.get(k) == Some(v))
    }

    /// Adds the scope's labels to `labels`, refusing a label the request sets
    /// to another value than its scope does.
    pub fn merge_into(&self, labels: &mut BTreeMap<String, String>) -> Result<(), String> {
        for (key, value) in &self.0 {
            match labels.get(key) {
                Some(other) if other != value => {
                    return Err(format!(
                        "label {key} is {value} in the request's scope, not {other}"
                    ));
                }
                _ => {
                    labels.insert(key.clone(), value.clone());
                }
            }
        }
        Ok(())
    }

    /// The first key of `keys` that the scope has: a label edit can't touch it.
    pub fn touched<'a>(&self, keys: impl IntoIterator<Item = &'a String>) -> Option<&'a String> {
        keys.into_iter().find(|k| self.0.contains_key(*k))
    }

    /// A text that is the same for equal scopes and differs otherwise: empty
    /// for no scope, else the labels as JSON, in key order.
    pub fn canonical(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        serde_json::to_string(&self.0).unwrap_or_default()
    }

    /// The header a client sends for the scope.
    pub fn header(&self) -> String {
        let pairs: Vec<String> = self.0.iter().map(|(k, v)| format!("{k}={v}")).collect();
        pairs.join(",")
    }

    /// Idempotency key `key` as it is stored for the scope, so that a key is
    /// unique within a scope: as it is without one, else after the scope and
    /// a space, which no key has.
    pub fn key(&self, key: &str) -> String {
        if self.is_empty() {
            key.to_string()
        } else {
            format!("{} {key}", self.canonical())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_header_parses_into_labels_and_back() {
        let scope = Scope::parse(" user=42, org=a=b ,").unwrap();
        assert_eq!(scope.labels(), &labels(&[("user", "42"), ("org", "a=b")]));
        assert_eq!(scope.header(), "org=a=b,user=42");
        assert_eq!(Scope::parse(&scope.header()).unwrap(), scope);
        assert!(Scope::parse("").unwrap().is_empty());
        for bad in ["user", "user=", "us er=1", "user=1,user=2", "=1"] {
            assert!(Scope::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_scope_matches_and_stamps_labels() {
        let scope = Scope::parse("user=42").unwrap();
        assert!(scope.matches(&labels(&[("user", "42"), ("run", "1")])));
        assert!(!scope.matches(&labels(&[("user", "43")])));
        assert!(!scope.matches(&labels(&[])));
        assert!(Scope::default().matches(&labels(&[])));
        let mut created = labels(&[("run", "1")]);
        scope.merge_into(&mut created).unwrap();
        assert_eq!(created, labels(&[("run", "1"), ("user", "42")]));
        let mut other = labels(&[("user", "43")]);
        assert!(scope.merge_into(&mut other).is_err());
        let keys = ["run".to_string(), "user".to_string()];
        assert_eq!(scope.touched(&keys), Some(&keys[1]));
    }

    #[test]
    fn keys_are_unique_within_a_scope() {
        let scope = Scope::parse("user=42").unwrap();
        assert_eq!(Scope::default().key("k"), "k");
        assert_eq!(scope.key("k"), r#"{"user":"42"} k"#);
        assert_ne!(scope.key("k"), Scope::parse("user=43").unwrap().key("k"));
        assert_eq!(Scope::default().canonical(), "");
    }
}
