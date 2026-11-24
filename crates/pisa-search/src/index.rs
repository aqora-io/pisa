//! Index bundles: everything `load` puts into Qdrant for one `env_id`, built
//! from a corpus and any informal descriptions.
//!
//! A bundle directory holds `manifest.json` (with the lexical fields'
//! average lengths, see `encode`), `decls.ndjson.zst` (what hits show, plus
//! type constants, dependency ids and PageRank) and `vectors.bin`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Context;
use pisa_protocol::search::Informal;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::corpus::{self, Corpus, Decl, LineRange};
use crate::text::split_name;

pub const FORMAT_VERSION: u32 = 5;

/// What a hit or `/declarations` shows for one declaration.
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
    /// For a theorem, the constants its conclusion is built from, in the order
    /// of its discrimination-tree path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub concl: Vec<String>,
    /// A definition's value; see [`crate::corpus::Decl::value`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
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
        concl: conclusion_constants(&d.concl_keys),
        value: d.value.clone(),
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

/// The constants among a conclusion's discrimination-tree keys, each once:
/// no stars, literals, projections (`Prod.1`) or other placeholders.
pub fn conclusion_constants(keys: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        let constant = k.chars().next().is_some_and(char::is_alphabetic)
            && !k
                .rsplit('.')
                .next()
                .is_some_and(|last| last.chars().all(|c| c.is_ascii_digit()));
        if constant && !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
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
        let vectors = dir.join("vectors.bin");
        let dense = if vectors.exists() {
            Some(crate::dense::Dense::read(&vectors)?)
        } else {
            None
        };
        Ok(Bundle {
            manifest,
            decls,
            dense,
        })
    }
}

#[cfg(test)]
mod tests {
    /// A conclusion's keys as constants: no stars, literals, projections
    /// or other placeholders, each once.
    #[test]
    fn conclusion_keys_keep_their_constants() {
        let keys: Vec<String> = [
            "Eq",
            "*",
            "Matrix.trace",
            "HMul.hMul",
            "*",
            "1",
            "\"s\"",
            "Code.1",
            "◾",
            "→",
            "Matrix.trace",
            "Real.sqrt",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            conclusion_constants(&keys),
            ["Eq", "Matrix.trace", "HMul.hMul", "Real.sqrt"]
        );
    }

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
