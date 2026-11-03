//! Sparse vectors for Qdrant's lexical, structural and name lanes.
//!
//! The lexical vector reproduces the old tantivy index's BM25: one token per
//! (field, term), with the document side holding BM25's term-frequency part
//! and the query side each field's boost. Qdrant's `idf` modifier supplies
//! the rest, so a query scores `Σ boost · idf · tf part`, as tantivy's
//! boolean query of boosted term queries did.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::tokenizer::{
    Language, LowerCaser, NgramTokenizer, RemoveLongFilter, Stemmer, TextAnalyzer,
    WhitespaceTokenizer,
};

use crate::index::{StoredDecl, name_text};
use crate::text::{latex_to_unicode, tokens};

/// BM25's term-frequency saturation and length normalization, as tantivy's.
const K1: f32 = 1.2;
const B: f32 = 0.75;

/// The fields of the lexical vector.
pub const FIELDS: [&str; 5] = ["name", "informal", "statement", "signature", "doc"];

/// Words: whitespace-split, lowercased.
pub fn words() -> TextAnalyzer {
    TextAnalyzer::builder(WhitespaceTokenizer::default())
        .filter(RemoveLongFilter::limit(80))
        .filter(LowerCaser)
        .build()
}

/// Prose: words, stemmed, so `eigenvalues` matches `eigenValue`.
pub fn prose() -> TextAnalyzer {
    TextAnalyzer::builder(WhitespaceTokenizer::default())
        .filter(RemoveLongFilter::limit(80))
        .filter(LowerCaser)
        .filter(Stemmer::new(Language::English))
        .build()
}

/// Trigrams of a lowercased name.
pub fn ngram() -> TextAnalyzer {
    TextAnalyzer::builder(NgramTokenizer::new(3, 3, false).expect("valid ngram sizes"))
        .filter(LowerCaser)
        .build()
}

/// Each field's analyzer: names, descriptions and docstrings as prose,
/// signatures as words.
fn analyzer(field: &str) -> TextAnalyzer {
    if field == "signature" {
        words()
    } else {
        prose()
    }
}

fn analyze(mut a: TextAnalyzer, text: &str) -> Vec<String> {
    let mut stream = a.token_stream(text);
    let mut out = Vec::new();
    while stream.advance() {
        out.push(stream.token().text.clone());
    }
    out
}

/// A sparse vector, as Qdrant takes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sparse {
    pub indices: Vec<u32>,
    pub values: Vec<f32>,
}

impl Sparse {
    fn from_map(map: BTreeMap<u32, f32>) -> Sparse {
        let (indices, values) = map.into_iter().unzip();
        Sparse { indices, values }
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// The index of a token of `kind` (a field, `const` or `tri`).
pub fn token(kind: &str, term: &str) -> u32 {
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(b":");
    h.update(term.as_bytes());
    u32::from_be_bytes(h.finalize()[..4].try_into().expect("4 bytes"))
}

/// The text of each lexical field of a declaration, as the tantivy index had it.
pub fn field_texts(d: &StoredDecl) -> [(&'static str, String); 5] {
    let mut informal = String::new();
    let mut statement = String::new();
    if let Some(i) = &d.informal {
        informal.push_str(&tokens(&i.informal_name).join(" "));
        for alias in &i.aliases {
            informal.push(' ');
            informal.push_str(&tokens(alias).join(" "));
        }
        statement.push_str(&tokens(&i.statement_nl).join(" "));
        if let Some(latex) = &i.statement_latex {
            statement.push(' ');
            statement.push_str(&tokens(&latex_to_unicode(latex)).join(" "));
        }
    }
    let doc = d
        .docstring
        .as_deref()
        .map(|doc| tokens(doc).join(" "))
        .unwrap_or_default();
    [
        ("name", name_text(&d.name)),
        ("informal", informal),
        ("statement", statement),
        ("signature", tokens(&d.signature).join(" ")),
        ("doc", doc),
    ]
}

/// Each field's terms in `d`.
fn field_terms(d: &StoredDecl) -> Vec<(&'static str, Vec<String>)> {
    field_texts(d)
        .into_iter()
        .map(|(field, text)| (field, analyze(analyzer(field), &text)))
        .collect()
}

/// The average number of terms in each lexical field over `decls`, counting
/// declarations without the field, as BM25 does.
pub fn field_stats(decls: &[StoredDecl]) -> BTreeMap<String, f32> {
    let mut totals: HashMap<&str, u64> = HashMap::new();
    for d in decls {
        for (field, terms) in field_terms(d) {
            *totals.entry(field).or_default() += terms.len() as u64;
        }
    }
    let n = decls.len().max(1) as f32;
    FIELDS
        .iter()
        .map(|f| {
            (
                f.to_string(),
                totals.get(f).copied().unwrap_or(0) as f32 / n,
            )
        })
        .collect()
}

/// A declaration's lexical vector: BM25's term-frequency part for each
/// (field, term), with the averages `stats`.
pub fn lexical_doc(d: &StoredDecl, stats: &BTreeMap<String, f32>) -> Sparse {
    let mut map = BTreeMap::new();
    for (field, terms) in field_terms(d) {
        let len = terms.len() as f32;
        let avg = stats.get(field).copied().unwrap_or(1.0).max(f32::EPSILON);
        let mut tf: HashMap<&str, f32> = HashMap::new();
        for t in &terms {
            *tf.entry(t).or_default() += 1.0;
        }
        for (term, tf) in tf {
            let value = tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avg));
            map.insert(token(field, term), value);
        }
    }
    Sparse::from_map(map)
}

/// A query's lexical vector: for each `(tokens, field, boost)`, the field's
/// terms of the tokens with the boost; a term several clauses share sums them.
pub fn lexical_query(plan: &[(&[String], &str, f32)]) -> Sparse {
    let mut map: BTreeMap<u32, f32> = BTreeMap::new();
    for (toks, field, boost) in plan {
        if toks.is_empty() {
            continue;
        }
        let mut terms = analyze(analyzer(field), &toks.join(" "));
        terms.sort();
        terms.dedup();
        for t in terms {
            *map.entry(token(field, &t)).or_default() += boost;
        }
    }
    Sparse::from_map(map)
}

/// The constants a declaration's type mentions.
pub fn consts_doc(type_refs: &[String]) -> Sparse {
    let map = type_refs.iter().map(|c| (token("const", c), 1.0)).collect();
    Sparse::from_map(map)
}

/// Constants with weights, as a query.
pub fn consts_query(consts: &[(String, f32)]) -> Sparse {
    let mut map: BTreeMap<u32, f32> = BTreeMap::new();
    for (c, w) in consts {
        let v = map.entry(token("const", c)).or_default();
        *v = v.max(*w);
    }
    Sparse::from_map(map)
}

/// The trigrams of a name, lowercased.
pub fn trigrams(name: &str) -> Sparse {
    let map = analyze(ngram(), &name.to_lowercase())
        .into_iter()
        .map(|t| (token("tri", &t), 1.0))
        .collect();
    Sparse::from_map(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str, signature: &str, doc: Option<&str>) -> StoredDecl {
        StoredDecl {
            name: name.into(),
            signature: signature.into(),
            docstring: doc.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn lexical_doc_values_are_bm25_tf_parts() {
        let d = decl(
            "Matrix.trace",
            "Matrix.trace A",
            Some("trace trace of a matrix"),
        );
        let stats: BTreeMap<String, f32> = FIELDS.iter().map(|f| (f.to_string(), 4.0)).collect();
        let v = lexical_doc(&d, &stats);
        // "trace" twice in the docstring.
        let len = field_terms(&d)[4].1.len() as f32;
        let i = v
            .indices
            .iter()
            .position(|&i| i == token("doc", "trace"))
            .unwrap();
        let want = 2.0 * (K1 + 1.0) / (2.0 + K1 * (1.0 - B + B * len / 4.0));
        assert!((v.values[i] - want).abs() < 1e-6, "{}", v.values[i]);
        // The same term in another field is another token.
        assert!(v.indices.contains(&token("name", "trace")));
        assert!(v.indices.is_sorted());
    }

    #[test]
    fn lexical_queries_stem_prose_and_sum_shared_terms() {
        let prose_tokens = vec!["eigenvalues".to_string()];
        let lean_tokens = vec!["eigenvalues".to_string()];
        let q = lexical_query(&[
            (&prose_tokens, "name", 1.0),
            (&lean_tokens, "name", 1.5),
            (&prose_tokens, "signature", 1.0),
        ]);
        let get = |field: &str, term: &str| {
            let i = q.indices.iter().position(|&i| i == token(field, term));
            i.map(|i| q.values[i])
        };
        assert_eq!(get("name", "eigenvalu"), Some(2.5));
        // Signatures aren't stemmed.
        assert_eq!(get("signature", "eigenvalues"), Some(1.0));
    }

    #[test]
    fn field_stats_average_over_every_declaration() {
        let stats = field_stats(&[decl("a", "a b c", None), decl("b", "b", Some("x y"))]);
        assert_eq!(stats["signature"], 2.0);
        assert_eq!(stats["doc"], 1.0);
        assert_eq!(stats["informal"], 0.0);
    }

    #[test]
    fn trigrams_are_case_insensitive() {
        assert_eq!(trigrams("Real.sqrt"), trigrams("real.SQRT"));
        assert!(trigrams("Eq").is_empty());
        assert_eq!(
            consts_query(&[("Nat".into(), 0.5), ("Nat".into(), 1.0)]).values,
            [1.0]
        );
    }
}
