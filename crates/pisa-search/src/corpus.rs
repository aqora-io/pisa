//! The declaration corpus: the exporter's records plus content hashes, for one
//! environment (`env_id`).
//!
//! On disk a corpus is a directory with `manifest.json`, `decls.ndjson.zst`
//! (sorted by name) and `modules.ndjson.zst` (module docstrings).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One declaration, as exported by `search/exporter/PisaExport.lean`, plus
/// hashes computed at ingest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Decl {
    pub name: String,
    pub kind: String,
    pub module: String,
    pub library: String,
    pub signature: String,
    pub type_pp_full: String,
    #[serde(default)]
    pub type_refs: Vec<String>,
    #[serde(default)]
    pub value_refs: Vec<String>,
    #[serde(default)]
    pub docstring: Option<String>,
    #[serde(default)]
    pub attrs: Vec<String>,
    #[serde(default)]
    pub range: Option<LineRange>,
    #[serde(default)]
    pub deprecated_to: Option<String>,
    #[serde(default)]
    pub uses_sorry: bool,
    #[serde(default)]
    pub informal: bool,
    #[serde(default)]
    pub informal_deps: Vec<String>,
    #[serde(default)]
    pub concl_keys: Vec<String>,
    #[serde(default)]
    pub additive_sibling: Option<String>,
    /// The exporter environment it came from; only read during ingest.
    #[serde(default, skip_serializing)]
    pub env: String,
    /// The import header that loads it, when not the default one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<String>,
    /// Hash of the name, signature and docstring.
    #[serde(default)]
    pub statement_hash: String,
    /// Hash of the statement and its dependencies' statements: an LLM
    /// description stays valid while this is unchanged.
    #[serde(default)]
    pub content_hash: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleDoc {
    pub name: String,
    pub doc: String,
}

#[derive(Deserialize)]
#[serde(tag = "record", rename_all = "lowercase")]
#[allow(clippy::large_enum_variant)] // parsed one line at a time
enum ExportLine {
    Decl(Decl),
    Module(ModuleDoc),
    /// A `to_additive` pair, from `search/exporter/ToAdditive.lean`.
    Additive {
        name: String,
        sibling: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub env_id: String,
    pub decls: usize,
    pub libraries: BTreeMap<String, usize>,
    /// Library root → URL prefix for source links pinned to a commit.
    pub sources: BTreeMap<String, String>,
}

pub struct Corpus {
    pub manifest: Manifest,
    pub decls: Vec<Decl>,
    pub modules: Vec<ModuleDoc>,
}

impl Decl {
    /// Definitions' descriptions depend on their bodies; theorems' don't.
    pub fn is_definition_like(&self) -> bool {
        matches!(self.kind.as_str(), "def" | "abbrev" | "instance" | "opaque")
    }

    /// Declarations an informal description builds on.
    pub fn description_deps(&self) -> impl Iterator<Item = &String> {
        let body = if self.is_definition_like() {
            &self.value_refs[..]
        } else {
            &[]
        };
        self.type_refs.iter().chain(body).chain(&self.informal_deps)
    }
}

fn hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    hex::encode(&h.finalize()[..16])
}

/// Reads exporter output (plain or `.zst`, one or more files) into a corpus
/// with hashes. A declaration in several exports keeps its first copy.
/// Declarations from an environment in `env_imports` record its header.
pub fn ingest(
    exports: &[&Path],
    env_id: String,
    lake_manifest: Option<&Path>,
    env_imports: &BTreeMap<String, String>,
) -> anyhow::Result<Corpus> {
    let mut decls = Vec::new();
    let mut modules = Vec::new();
    let mut pairs = Vec::new();
    for export in exports {
        for (i, line) in open(export)?.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let parsed = serde_json::from_str(&line)
                .with_context(|| format!("{} line {}", export.display(), i + 1))?;
            match parsed {
                ExportLine::Decl(d) => decls.push(d),
                ExportLine::Module(m) => modules.push(m),
                ExportLine::Additive { name, sibling } => pairs.push((name, sibling)),
            }
        }
    }
    decls.retain(|d| !is_syntax(d));
    decls.sort_by(|a, b| a.name.cmp(&b.name));
    decls.dedup_by(|a, b| a.name == b.name);
    for d in &mut decls {
        d.imports = env_imports.get(&d.env).cloned();
        d.env.clear();
    }
    let index: HashMap<String, usize> = decls
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.clone(), i))
        .collect();
    for (a, b) in pairs {
        if let (Some(&i), Some(&j)) = (index.get(&a), index.get(&b)) {
            decls[i].additive_sibling = Some(b);
            decls[j].additive_sibling = Some(a);
        }
    }
    compute_hashes(&mut decls);
    let mut libraries = BTreeMap::new();
    for d in &decls {
        *libraries.entry(d.library.clone()).or_insert(0) += 1;
    }
    let mut sources = match lake_manifest {
        Some(path) => source_prefixes(&std::fs::read_to_string(path)?)?,
        None => BTreeMap::new(),
    };
    // Lean core's libraries live in the lean4 repository at the toolchain's tag.
    if let Some(tag) = env_id.rsplit_once('-').map(|(tag, _)| tag) {
        for lib in ["Init", "Std", "Lean", "Lake"] {
            sources.insert(
                lib.into(),
                format!("https://github.com/leanprover/lean4/blob/{tag}/src/"),
            );
        }
    }
    Ok(Corpus {
        manifest: Manifest {
            env_id,
            decls: decls.len(),
            libraries,
            sources,
        },
        decls,
        modules,
    })
}

/// Notation, parser and macro machinery: declarations of these types are how
/// syntax is implemented, not mathematics anyone would cite.
const SYNTAX_TYPES: &[&str] = &[
    "Lean.ParserDescr",
    "Lean.TrailingParserDescr",
    "Lean.Parser.Parser",
    "Lean.Parser.TrailingParser",
    "Lean.Macro",
    "Lean.PrettyPrinter.Unexpander",
    "Lean.PrettyPrinter.Delaborator.Delab",
    "Lean.Elab.Term.TermElab",
    "Lean.Elab.Command.CommandElab",
    "Lean.Elab.Tactic.Tactic",
];

/// Auxiliary declarations Lean generates that the exporter's `isAutoDecl`
/// doesn't catch (`ctorIdx`, `sizeOf_spec`, `injEq`, ...).
const GENERATED: &[&str] = &[
    "ctorIdx",
    "ctorElim",
    "ctorElimType",
    "ofNat_ctorIdx",
    "sizeOf_spec",
    "injEq",
    "inj",
    "congr_simp",
    "noConfusion",
    "noConfusionType",
    "eq_def",
    "below",
    "ibelow",
    "brecOn",
    "binductionOn",
    "recOn",
    "casesOn",
];

pub fn is_generated(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name);
    GENERATED.contains(&last)
        || last
            .strip_prefix("eq_")
            .is_some_and(|n| n.parse::<u32>().is_ok())
}

/// Metaprogramming and implementation internals (`Mathlib.Tactic.…`,
/// `Nat.Internal.…`, `Lean.…`), which mathematical searches don't want.
pub fn is_tooling(name: &str) -> bool {
    let mut parts: Vec<&str> = name.split('.').collect();
    parts.pop();
    matches!(parts.first(), Some(&("Lean" | "Lake")))
        || parts
            .iter()
            .any(|p| matches!(*p, "Tactic" | "Meta" | "Internal"))
}

pub fn is_syntax(d: &Decl) -> bool {
    SYNTAX_TYPES.contains(&d.type_pp_full.as_str())
}

pub fn compute_hashes(decls: &mut [Decl]) {
    fn as_is(doc: &str) -> Cow<'_, str> {
        Cow::Borrowed(doc)
    }
    let (statements, content) = hashes(decls, as_is);
    for ((d, s), c) in decls.iter_mut().zip(statements).zip(content) {
        d.statement_hash = s;
        d.content_hash = c;
    }
}

/// Content hashes as [`compute_hashes`] gives them, but with every docstring's
/// whitespace collapsed. Lean releases reformat docstrings (v4.36 trims
/// trailing whitespace), which changes content hashes without changing what a
/// description says; see [`crate::carry`].
pub fn hashes_ignoring_doc_whitespace(decls: &[Decl]) -> Vec<String> {
    fn collapsed(doc: &str) -> Cow<'_, str> {
        Cow::Owned(doc.split_whitespace().collect::<Vec<_>>().join(" "))
    }
    hashes(decls, collapsed).1
}

/// Statement and content hashes, with docstrings passed through `doc`.
fn hashes(decls: &[Decl], doc: impl Fn(&str) -> Cow<'_, str>) -> (Vec<String>, Vec<String>) {
    let statements: Vec<String> = decls
        .iter()
        .map(|d| {
            hash(&[
                &d.name,
                &d.signature,
                &doc(d.docstring.as_deref().unwrap_or("")),
            ])
        })
        .collect();
    let by_name: HashMap<&str, &str> = decls
        .iter()
        .zip(&statements)
        .map(|(d, s)| (d.name.as_str(), s.as_str()))
        .collect();
    let content = decls
        .iter()
        .zip(&statements)
        .map(|(d, s)| {
            let mut deps: Vec<&str> = d
                .description_deps()
                .filter_map(|n| by_name.get(n.as_str()).copied())
                .collect();
            deps.sort_unstable();
            deps.dedup();
            let mut parts = vec![s.as_str()];
            parts.extend(deps);
            hash(&parts)
        })
        .collect();
    (statements, content)
}

/// URL prefixes per library, from `lake-manifest.json`, e.g. `Mathlib` →
/// `https://github.com/leanprover-community/mathlib4/blob/<rev>/`.
pub fn source_prefixes(lake_manifest: &str) -> anyhow::Result<BTreeMap<String, String>> {
    #[derive(Deserialize)]
    struct LakeManifest {
        packages: Vec<Package>,
    }
    #[derive(Deserialize)]
    struct Package {
        name: String,
        url: Option<String>,
        rev: Option<String>,
    }
    let manifest: LakeManifest = serde_json::from_str(lake_manifest)?;
    let mut out = BTreeMap::new();
    for p in manifest.packages {
        let (Some(url), Some(rev)) = (p.url, p.rev) else {
            continue;
        };
        let base = url.trim_end_matches(".git");
        if !base.starts_with("https://github.com/") {
            continue;
        }
        // A package's libraries; Physlib's repository holds three.
        let roots = match p.name.as_str() {
            "mathlib" => vec!["Mathlib"],
            "batteries" => vec!["Batteries"],
            "Physlib" => vec!["Physlib", "QuantumInfo", "PhyslibAlpha"],
            other => vec![other],
        };
        for root in roots {
            out.insert(root.to_string(), format!("{base}/blob/{rev}/"));
        }
    }
    Ok(out)
}

/// A link to a declaration's lines in its pinned source.
pub fn source_url(sources: &BTreeMap<String, String>, d: &Decl) -> Option<String> {
    let prefix = sources.get(&d.library)?;
    let path = d.module.replace('.', "/");
    let lines = d
        .range
        .map(|r| format!("#L{}-L{}", r.start, r.end))
        .unwrap_or_default();
    Some(format!("{prefix}{path}.lean{lines}"))
}

fn open(path: &Path) -> anyhow::Result<Box<dyn BufRead>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let inner: Box<dyn Read> = if path.extension().is_some_and(|e| e == "zst") {
        Box::new(zstd::Decoder::new(file)?)
    } else {
        Box::new(file)
    };
    Ok(Box::new(BufReader::new(inner)))
}

fn write_ndjson<T: Serialize>(path: &Path, items: &[T]) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut out = zstd::Encoder::new(file, 9)?.auto_finish();
    for item in items {
        serde_json::to_writer(&mut out, item)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

fn read_ndjson<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<Vec<T>> {
    open(path)?
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}

impl Corpus {
    pub fn write(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        write_ndjson(&dir.join("decls.ndjson.zst"), &self.decls)?;
        write_ndjson(&dir.join("modules.ndjson.zst"), &self.modules)?;
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&self.manifest)?,
        )?;
        Ok(())
    }

    pub fn read(dir: &Path) -> anyhow::Result<Corpus> {
        let manifest = serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
        Ok(Corpus {
            manifest,
            decls: read_ndjson(&dir.join("decls.ndjson.zst"))?,
            modules: read_ndjson(&dir.join("modules.ndjson.zst"))?,
        })
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub added: usize,
    pub removed: usize,
    /// Content hash changed: descriptions need regenerating.
    pub changed: usize,
    pub unchanged: usize,
}

/// What changed between two corpora, by name and content hash.
pub fn diff(old: &[Decl], new: &[Decl]) -> Diff {
    let old: HashMap<&str, &str> = old
        .iter()
        .map(|d| (d.name.as_str(), d.content_hash.as_str()))
        .collect();
    let mut out = Diff::default();
    let mut seen = 0;
    for d in new {
        match old.get(d.name.as_str()) {
            None => out.added += 1,
            Some(h) => {
                seen += 1;
                if *h == d.content_hash {
                    out.unchanged += 1;
                } else {
                    out.changed += 1;
                }
            }
        }
    }
    out.removed = old.len() - seen;
    out
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn decl(name: &str, kind: &str, signature: &str, type_refs: &[&str]) -> Decl {
        Decl {
            name: name.into(),
            kind: kind.into(),
            module: "Mathlib.Test".into(),
            library: "Mathlib".into(),
            signature: signature.into(),
            type_pp_full: signature.into(),
            type_refs: type_refs.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn content_hash_follows_dependency_statements_not_their_wording() {
        let mut v = vec![
            decl("A", "def", "A : Type", &[]),
            decl("b", "theorem", "b : A", &["A"]),
        ];
        compute_hashes(&mut v);
        let before = v[1].content_hash.clone();

        let mut changed = v.clone();
        changed[0].signature = "A : Type 1".into();
        compute_hashes(&mut changed);
        assert_ne!(
            changed[1].content_hash, before,
            "a dependency's statement changed"
        );

        let mut same = v.clone();
        same[1].attrs.push("simp".into());
        compute_hashes(&mut same);
        assert_eq!(same[1].content_hash, before);

        let d = diff(&v, &changed);
        assert_eq!(
            d,
            Diff {
                added: 0,
                removed: 0,
                changed: 2,
                unchanged: 0
            }
        );
    }

    #[test]
    fn hashes_are_stable_because_the_store_is_keyed_by_them() {
        let mut v = vec![
            Decl {
                docstring: Some("An A. ".into()),
                ..decl("A", "def", "A : Type", &[])
            },
            decl("b", "theorem", "b : A", &["A"]),
        ];
        compute_hashes(&mut v);
        let got: Vec<_> = v
            .iter()
            .map(|d| (d.statement_hash.as_str(), d.content_hash.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "19716f794bdd204196b1a1fc93af58ce",
                    "0920c5feb6d5e5b3a9e25580e3c8ec25"
                ),
                (
                    "254e9dbb8c5a6d9be7d811ff0dd8745c",
                    "3da744ec981f6503fe7820eea6dd09b4"
                ),
            ]
        );
    }

    #[test]
    fn ingest_reads_export_lines_and_writes_a_corpus() {
        let dir = tempfile::tempdir().unwrap();
        let export = dir.path().join("e.ndjson");
        std::fs::write(
            &export,
            concat!(
                r#"{"record":"decl","env":"x","name":"b","kind":"theorem","module":"Physlib.M","library":"Physlib","signature":"b : True","type_pp_full":"True","range":{"start":3,"end":4},"uses_sorry":true}"#,
                "\n",
                r#"{"record":"module","env":"x","name":"Physlib.M","doc":"About M."}"#,
                "\n",
                r#"{"record":"decl","env":"x","name":"a","kind":"def","module":"Mathlib.A","library":"Mathlib","signature":"a : Nat","type_pp_full":"Nat"}"#,
                "\n",
                r#"{"record":"additive","name":"a","sibling":"b"}"#,
                "\n"
            ),
        )
        .unwrap();
        let lake = r#"{"packages":[{"name":"mathlib","url":"https://github.com/leanprover-community/mathlib4.git","rev":"abc"},
                       {"name":"Physlib","url":"https://github.com/leanprover-community/physlib.git","rev":"def"}]}"#;
        let lake_path = dir.path().join("lake-manifest.json");
        std::fs::write(&lake_path, lake).unwrap();
        let corpus = ingest(
            &[export.as_path()],
            "v4.35.0-x".into(),
            Some(&lake_path),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            corpus
                .decls
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(corpus.decls[1].uses_sorry);
        assert_eq!(corpus.decls[0].additive_sibling.as_deref(), Some("b"));
        assert_eq!(corpus.decls[1].additive_sibling.as_deref(), Some("a"));
        assert_eq!(corpus.manifest.libraries["Physlib"], 1);
        assert_eq!(
            source_url(&corpus.manifest.sources, &corpus.decls[1]).unwrap(),
            "https://github.com/leanprover-community/physlib/blob/def/Physlib/M.lean#L3-L4"
        );
        assert_eq!(
            corpus.manifest.sources["PhyslibAlpha"],
            "https://github.com/leanprover-community/physlib/blob/def/"
        );

        let out = dir.path().join("corpus");
        corpus.write(&out).unwrap();
        let back = Corpus::read(&out).unwrap();
        assert_eq!(back.decls, corpus.decls);
        assert_eq!(back.modules[0].doc, "About M.");
    }

    #[test]
    fn second_environment_adds_its_own_declarations_with_their_header() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.ndjson");
        std::fs::write(
            &main,
            r#"{"record":"decl","env":"main","name":"a","kind":"def","module":"Mathlib.A","library":"Mathlib","signature":"a : Nat","type_pp_full":"Nat"}"#,
        )
        .unwrap();
        let qi = dir.path().join("qi.ndjson");
        std::fs::write(
            &qi,
            concat!(
                r#"{"record":"decl","env":"qi","name":"a","kind":"def","module":"QuantumInfo.A","library":"QuantumInfo","signature":"a : Int","type_pp_full":"Int"}"#,
                "\n",
                r#"{"record":"decl","env":"qi","name":"MState","kind":"structure","module":"QuantumInfo.MState","library":"QuantumInfo","signature":"MState : Type","type_pp_full":"Type"}"#,
            ),
        )
        .unwrap();
        let headers = BTreeMap::from([(
            "qi".to_string(),
            "import Physlib; import QuantumInfo".to_string(),
        )]);
        let c = ingest(
            &[main.as_path(), qi.as_path()],
            "v4.35.0-x".into(),
            None,
            &headers,
        )
        .unwrap();
        // The first export's copy of a shared name wins.
        assert_eq!(
            (c.decls[0].name.as_str(), c.decls[0].library.as_str()),
            ("MState", "QuantumInfo")
        );
        assert_eq!(
            c.decls[0].imports.as_deref(),
            Some("import Physlib; import QuantumInfo")
        );
        assert_eq!(
            (c.decls[1].name.as_str(), c.decls[1].library.as_str()),
            ("a", "Mathlib")
        );
        assert_eq!(c.decls[1].imports, None);
        let out = dir.path().join("corpus");
        c.write(&out).unwrap();
        assert_eq!(Corpus::read(&out).unwrap().decls, c.decls);
    }
}
