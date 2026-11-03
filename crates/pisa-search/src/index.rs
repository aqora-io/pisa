//! Index bundles: everything the server needs for one `env_id`, built from a
//! corpus and any informal descriptions.
//!
//! A bundle directory holds `manifest.json`, `decls.ndjson.zst` (what hits
//! show, plus dependency ids and PageRank), and `tantivy/` with one document
//! per declaration:
//!
//! | Field | Content | Analyzer |
//! | --- | --- | --- |
//! | `name` | name split on `.`, `_`, camelCase, plus the whole name | words |
//! | `name_ngram` | lowercased full name | trigrams |
//! | `informal` | informal name and aliases | prose (stemmed) |
//! | `statement` | informal statement and LaTeX statement | prose |
//! | `signature` | the signature | words |
//! | `doc` | docstring | prose |
//! | `consts` | constants in the type | raw |

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Context;
use pisa_protocol::search::Informal;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, SchemaBuilder,
    TextFieldIndexing, TextOptions,
};
use tantivy::{Index, IndexWriter, TantivyDocument};

use crate::corpus::{self, Corpus, Decl, LineRange};
use crate::text::{latex_to_unicode, split_name, tokens};

pub const FORMAT_VERSION: u32 = 4;

/// What a hit or `/v1/decl` shows for one declaration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredDecl {
    pub name: String,
    pub kind: String,
    pub module: String,
    pub library: String,
    pub signature: String,
    pub type_full: String,
    /// The constants its type mentions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_refs: Vec<String>,
    #[serde(default)]
    pub docstring: Option<String>,
    #[serde(default)]
    pub range: Option<LineRange>,
    #[serde(default)]
    pub deprecated_to: Option<String>,
    #[serde(default)]
    pub additive_sibling: Option<String>,
    #[serde(default)]
    pub uses_sorry: bool,
    /// The import header that loads it, when not the default one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<String>,
    /// A Physlib informal result (stated, not proved).
    #[serde(default)]
    pub informal_result: bool,
    #[serde(default)]
    pub informal: Option<Informal>,
    #[serde(default)]
    pub source_url: Option<String>,
    /// Ids of declarations it uses.
    #[serde(default)]
    pub deps: Vec<u32>,
    /// PageRank over "uses" edges, scaled so the mean is 1.
    pub pagerank: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BundleManifest {
    pub format: u32,
    pub env_id: String,
    pub decls: usize,
    pub libraries: BTreeMap<String, usize>,
    pub informal: usize,
    /// The embedding model, when the bundle has a dense lane.
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// The average number of terms in each lexical field (see `encode`).
    #[serde(default)]
    pub field_stats: BTreeMap<String, f32>,
    /// The kinds of declaration there are, e.g. `theorem`.
    #[serde(default)]
    pub kinds: Vec<String>,
}

#[derive(Clone, Copy)]
pub struct Fields {
    pub id: Field,
    pub name: Field,
    pub name_ngram: Field,
    pub informal: Field,
    pub statement: Field,
    pub signature: Field,
    pub doc: Field,
    pub consts: Field,
}

pub fn schema() -> (Schema, Fields) {
    let (b, fields) = schema_builder();
    (b.build(), fields)
}

/// The schema of the snapshot index: the bundle's, whose fields keep their
/// ids, plus `module`, a fast field with the slot of each declaration's module.
pub fn snapshot_schema() -> (Schema, Fields, Field) {
    let (mut b, fields) = schema_builder();
    let module = b.add_u64_field("module", INDEXED | FAST);
    (b.build(), fields, module)
}

fn schema_builder() -> (SchemaBuilder, Fields) {
    let mut b = Schema::builder();
    let text = |tokenizer: &str| {
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(tokenizer)
                .set_index_option(IndexRecordOption::WithFreqs),
        )
    };
    let fields = Fields {
        id: b.add_u64_field("id", STORED | INDEXED),
        // Stemmed, so `eigenvalues` matches `eigenValue`.
        name: b.add_text_field("name", text("pisa_prose")),
        name_ngram: b.add_text_field("name_ngram", text("pisa_ngram")),
        informal: b.add_text_field("informal", text("pisa_prose")),
        statement: b.add_text_field("statement", text("pisa_prose")),
        signature: b.add_text_field("signature", text("pisa_words")),
        doc: b.add_text_field("doc", text("pisa_prose")),
        consts: b.add_text_field("consts", STRING),
    };
    (b, fields)
}

pub fn register_tokenizers(index: &Index) {
    let t = index.tokenizers();
    t.register("pisa_words", crate::encode::words());
    t.register("pisa_prose", crate::encode::prose());
    t.register("pisa_ngram", crate::encode::ngram());
}

/// The text indexed for a name: its parts, the whole name, and the last component.
pub fn name_text(name: &str) -> String {
    let mut words = split_name(name);
    words.push(name.to_lowercase());
    if let Some(last) = name.rsplit('.').next() {
        words.push(last.to_lowercase());
    }
    words.join(" ")
}

/// Builds a bundle from a corpus, descriptions keyed by declaration name, and
/// optionally (model, document vectors keyed by declaration name).
pub fn build(
    corpus: &Corpus,
    informal: &HashMap<String, Informal>,
    vectors: Option<(&str, &HashMap<String, Vec<f32>>)>,
    out: &Path,
) -> anyhow::Result<BundleManifest> {
    std::fs::create_dir_all(out)?;
    let decls = &corpus.decls;
    let ids: HashMap<&str, u32> = decls
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.as_str(), i as u32))
        .collect();
    let deps: Vec<Vec<u32>> = decls
        .par_iter()
        .map(|d| {
            let mut v: Vec<u32> = d
                .type_refs
                .iter()
                .chain(&d.value_refs)
                .filter_map(|r| ids.get(r.as_str()).copied())
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect();
    let pagerank = pagerank(&deps, 30, 0.85);
    let stored: Vec<StoredDecl> = decls
        .iter()
        .zip(deps)
        .zip(&pagerank)
        .map(|((d, deps), pr)| stored_decl(d, informal.get(&d.name), deps, *pr, corpus))
        .collect();

    let (schema, fields) = schema();
    let dir = out.join("tantivy");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let index = Index::create_in_dir(&dir, schema)?;
    register_tokenizers(&index);
    let mut writer: IndexWriter = index.writer(1_000_000_000)?;
    for (i, (d, s)) in decls.iter().zip(&stored).enumerate() {
        writer.add_document(document(&fields, i as u64, s, &d.type_refs))?;
    }
    writer.commit()?;
    writer.wait_merging_threads()?;

    let file = std::fs::File::create(out.join("decls.ndjson.zst"))?;
    let mut enc = zstd::Encoder::new(file, 9)?.auto_finish();
    for s in &stored {
        serde_json::to_writer(&mut enc, s)?;
        std::io::Write::write_all(&mut enc, b"\n")?;
    }
    drop(enc);
    let path = out.join("vectors.bin");
    let embedding_model = match vectors {
        Some((model, vectors)) => {
            let entries: Vec<(u32, Vec<i8>)> = decls
                .iter()
                .enumerate()
                .filter_map(|(i, d)| {
                    Some((i as u32, crate::dense::quantize(vectors.get(&d.name)?)))
                })
                .collect();
            crate::dense::Dense::new(model.to_string(), entries).write(&path)?;
            Some(model.to_string())
        }
        None => {
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            None
        }
    };
    let manifest = BundleManifest {
        format: FORMAT_VERSION,
        env_id: corpus.manifest.env_id.clone(),
        decls: decls.len(),
        libraries: corpus.manifest.libraries.clone(),
        informal: stored.iter().filter(|s| s.informal.is_some()).count(),
        embedding_model,
        field_stats: crate::encode::field_stats(&stored),
        kinds: stored
            .iter()
            .map(|s| s.kind.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
    };
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

fn stored_decl(
    d: &Decl,
    informal: Option<&Informal>,
    deps: Vec<u32>,
    pagerank: f32,
    corpus: &Corpus,
) -> StoredDecl {
    // Physlib's informal results carry their human text as the docstring.
    let informal = informal.cloned().or_else(|| {
        d.informal.then(|| Informal {
            informal_name: d.name.rsplit('.').next().unwrap_or(&d.name).to_string(),
            statement_nl: d.docstring.clone().unwrap_or_default(),
            source: "physlib-informal".into(),
            ..Default::default()
        })
    });
    StoredDecl {
        name: d.name.clone(),
        kind: d.kind.clone(),
        module: d.module.clone(),
        library: d.library.clone(),
        signature: d.signature.clone(),
        type_full: d.type_pp_full.clone(),
        type_refs: d.type_refs.clone(),
        docstring: d.docstring.clone(),
        range: d.range,
        deprecated_to: d.deprecated_to.clone(),
        additive_sibling: d.additive_sibling.clone(),
        uses_sorry: d.uses_sorry,
        informal_result: d.informal,
        imports: d.imports.clone(),
        informal,
        source_url: corpus::source_url(&corpus.manifest.sources, d),
        deps,
        pagerank,
    }
}

/// The document of declaration `s`, whose type mentions `type_refs`.
pub fn document(f: &Fields, id: u64, s: &StoredDecl, type_refs: &[String]) -> TantivyDocument {
    let mut doc = TantivyDocument::default();
    doc.add_u64(f.id, id);
    doc.add_text(f.name, name_text(&s.name));
    doc.add_text(f.name_ngram, s.name.to_lowercase());
    doc.add_text(f.signature, tokens(&s.signature).join(" "));
    let mut statement = String::new();
    if let Some(i) = &s.informal {
        doc.add_text(f.informal, tokens(&i.informal_name).join(" "));
        for alias in &i.aliases {
            doc.add_text(f.informal, tokens(alias).join(" "));
        }
        statement.push(' ');
        statement.push_str(&tokens(&i.statement_nl).join(" "));
        if let Some(latex) = &i.statement_latex {
            statement.push(' ');
            statement.push_str(&tokens(&latex_to_unicode(latex)).join(" "));
        }
    }
    doc.add_text(f.statement, statement);
    if let Some(docstring) = &s.docstring {
        doc.add_text(f.doc, tokens(docstring).join(" "));
    }
    for c in type_refs {
        doc.add_text(f.consts, c);
    }
    doc
}

/// PageRank where each declaration passes rank to what it uses, so widely
/// used lemmas rank high. Scaled so the mean is 1.
pub fn pagerank(deps: &[Vec<u32>], iterations: usize, damping: f32) -> Vec<f32> {
    let n = deps.len();
    if n == 0 {
        return Vec::new();
    }
    // Incoming edges: used[v] lists the users of v.
    let mut users: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (u, ds) in deps.iter().enumerate() {
        for &v in ds {
            users[v as usize].push(u as u32);
        }
    }
    let mut rank = vec![1.0f32 / n as f32; n];
    for _ in 0..iterations {
        let dangling: f32 = rank
            .iter()
            .zip(deps)
            .filter(|(_, d)| d.is_empty())
            .map(|(r, _)| r)
            .sum();
        let base = (1.0 - damping) / n as f32 + damping * dangling / n as f32;
        rank = (0..n)
            .into_par_iter()
            .map(|v| {
                base + damping
                    * users[v]
                        .iter()
                        .map(|&u| rank[u as usize] / deps[u as usize].len() as f32)
                        .sum::<f32>()
            })
            .collect();
    }
    rank.iter().map(|r| r * n as f32).collect()
}

pub struct Bundle {
    pub manifest: BundleManifest,
    pub decls: Vec<StoredDecl>,
    pub index: Index,
    pub fields: Fields,
    pub dense: Option<crate::dense::Dense>,
}

impl Bundle {
    pub fn open(dir: &Path) -> anyhow::Result<Bundle> {
        let manifest: BundleManifest = serde_json::from_slice(
            &std::fs::read(dir.join("manifest.json")).context("reading the bundle manifest")?,
        )?;
        anyhow::ensure!(
            manifest.format == FORMAT_VERSION,
            "bundle format {} is not {FORMAT_VERSION}; rebuild it",
            manifest.format
        );
        let file = std::fs::File::open(dir.join("decls.ndjson.zst"))?;
        let decls = std::io::BufRead::lines(std::io::BufReader::new(zstd::Decoder::new(file)?))
            .map(|l| Ok(serde_json::from_str(&l?)?))
            .collect::<anyhow::Result<Vec<StoredDecl>>>()?;
        let index = Index::open_in_dir(dir.join("tantivy"))?;
        register_tokenizers(&index);
        let (_, fields) = schema();
        let vectors = dir.join("vectors.bin");
        let dense = if vectors.exists() {
            Some(crate::dense::Dense::read(&vectors)?)
        } else {
            None
        };
        Ok(Bundle {
            manifest,
            decls,
            index,
            fields,
            dense,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagerank_favours_widely_used_declarations() {
        // 0 and 1 both use 2; 2 uses 3.
        let deps = vec![vec![2], vec![2], vec![3], vec![]];
        let pr = pagerank(&deps, 50, 0.85);
        assert!(pr[2] > pr[0] && pr[3] > pr[0], "{pr:?}");
        let mean: f32 = pr.iter().sum::<f32>() / pr.len() as f32;
        assert!((mean - 1.0).abs() < 1e-3, "{mean}");
    }

    #[test]
    fn name_text_has_parts_whole_and_last() {
        assert_eq!(
            name_text("Real.sqrt_mul_self"),
            "real sqrt mul self real.sqrt_mul_self sqrt_mul_self"
        );
    }
}
