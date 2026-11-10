//! Carries stored descriptions over to a rebuilt corpus.
//!
//! A content hash covers a declaration's docstring and its dependencies'
//! docstrings, so when a Lean release reformats docstrings (v4.36 trims
//! trailing whitespace) most hashes change while no description's meaning
//! does. A declaration whose hash ignoring docstring whitespace is the same in
//! both corpora keeps its description under its new content hash.

use std::collections::HashMap;

use crate::corpus::{Decl, hashes_ignoring_doc_whitespace};
use crate::informal_store::Entry;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Described under an old content hash; copied to the new one.
    pub carried: usize,
    /// Its content hash already has a description.
    pub stored: usize,
    /// Unchanged but for docstring whitespace, with no description to carry.
    pub undescribed: usize,
    /// New, or changed beyond docstring whitespace.
    pub changed: usize,
}

/// Entries for `new`'s declarations that have none under their content hash
/// but had one in `old`, unchanged but for docstring whitespace.
pub fn carry(old: &[Decl], new: &[Decl], stored: &HashMap<String, Entry>) -> (Vec<Entry>, Report) {
    let old_keys = hashes_ignoring_doc_whitespace(old);
    let old: HashMap<&str, (&Decl, &String)> = old
        .iter()
        .zip(&old_keys)
        .map(|(d, k)| (d.name.as_str(), (d, k)))
        .collect();
    let mut entries = Vec::new();
    let mut report = Report::default();
    for (d, key) in new.iter().zip(hashes_ignoring_doc_whitespace(new)) {
        if stored.contains_key(&d.content_hash) {
            report.stored += 1;
            continue;
        }
        match old.get(d.name.as_str()) {
            Some((o, k)) if **k == key => match stored.get(&o.content_hash) {
                Some(e) => {
                    entries.push(Entry {
                        content_hash: d.content_hash.clone(),
                        ..e.clone()
                    });
                    report.carried += 1;
                }
                None => report.undescribed += 1,
            },
            _ => report.changed += 1,
        }
    }
    (entries, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::compute_hashes;
    use crate::corpus::tests::decl;
    use pisa_protocol::search::Informal;

    fn described(d: &Decl) -> (String, Entry) {
        let e = Entry {
            content_hash: d.content_hash.clone(),
            name: d.name.clone(),
            informal: Informal {
                informal_name: d.name.clone(),
                statement_nl: format!("about {}", d.name),
                source: "test".into(),
                ..Default::default()
            },
            synthetic_queries: Vec::new(),
            meta: None,
        };
        (d.content_hash.clone(), e)
    }

    #[test]
    fn descriptions_follow_declarations_across_docstring_whitespace() {
        let mut old = vec![
            Decl {
                docstring: Some("An  A.\n".into()),
                ..decl("A", "def", "A : Type", &[])
            },
            decl("b", "theorem", "b : A", &["A"]),
            decl("c", "theorem", "c : A", &["A"]),
            decl("d", "theorem", "d : A", &["A"]),
            decl("x", "theorem", "x : True", &[]),
        ];
        compute_hashes(&mut old);
        let stored: HashMap<_, _> = [&old[1], &old[2], &old[4]]
            .into_iter()
            .map(described)
            .collect();

        let mut new = old.clone();
        new[0].docstring = Some("An A.".into());
        new[2].signature = "c : A → A".into();
        new.push(decl("e", "theorem", "e : A", &["A"]));
        compute_hashes(&mut new);
        assert_ne!(new[1].content_hash, old[1].content_hash);

        let (entries, report) = carry(&old, &new, &stored);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content_hash, new[1].content_hash);
        assert_eq!(entries[0].informal, stored[&old[1].content_hash].informal);
        assert_eq!(
            report,
            Report {
                carried: 1,
                stored: 1,
                undescribed: 2,
                changed: 2,
            }
        );
    }

    #[test]
    fn a_dependency_docstring_that_says_something_else_stops_the_carry() {
        let mut old = vec![
            Decl {
                docstring: Some("An A.".into()),
                ..decl("A", "def", "A : Type", &[])
            },
            decl("b", "theorem", "b : A", &["A"]),
        ];
        compute_hashes(&mut old);
        let stored: HashMap<_, _> = [described(&old[1])].into_iter().collect();
        let mut new = old.clone();
        new[0].docstring = Some("Another A.".into());
        compute_hashes(&mut new);
        let (entries, report) = carry(&old, &new, &stored);
        assert!(entries.is_empty());
        assert_eq!(report.changed, 2);
    }
}
