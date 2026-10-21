//! Answering queries over a bundle: routing, lanes, fusion, priors, rendering.

use std::collections::{HashMap, HashSet};

use pisa_protocol::search::{DeclDetail, Flags, Hit, Query, QueryResult, Resolution, Suggestion};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, Query as TQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Value};
use tantivy::{IndexReader, TantivyDocument, Term};

use crate::index::{Bundle, StoredDecl};
use crate::symbols::Symbols;
use crate::text::{latex_to_unicode, lean_identifiers, levenshtein, tokens};

/// Candidates each lane contributes to fusion.
const LANE_DEPTH: usize = 100;
/// Reciprocal-rank-fusion constant.
const RRF_K: f64 = 60.0;
const MAX_K: usize = 50;

/// Fusion weights and field boosts. Defaults are set from the MathlibQR
/// baseline (see `search/eval/`); `eval` can try others.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Weights {
    pub lexical: f64,
    pub name: f64,
    pub structural: f64,
    pub dense: f64,
    /// Prose against each field.
    pub prose_name: f32,
    pub prose_informal: f32,
    pub prose_statement: f32,
    pub prose_signature: f32,
    pub prose_doc: f32,
    /// Lean text against each field.
    pub lean_signature: f32,
    pub lean_name: f32,
    /// Weight of log PageRank after fusion.
    pub pagerank: f64,
    /// Cap on log PageRank, so prominence reorders close matches but can't
    /// override the text match entirely.
    pub pagerank_cap: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            lexical: 1.0,
            name: 3.0,
            structural: 0.3,
            dense: 1.0,
            prose_name: 1.0,
            prose_informal: 2.0,
            prose_statement: 1.5,
            prose_signature: 1.0,
            prose_doc: 1.2,
            lean_signature: 2.0,
            lean_name: 1.5,
            pagerank: 0.004,
            pagerank_cap: 4.0,
        }
    }
}

/// Which lanes a search may use; all by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lanes {
    pub lexical: bool,
    pub name: bool,
    pub structural: bool,
    pub dense: bool,
}

impl Lanes {
    pub const ALL: Lanes = Lanes {
        lexical: true,
        name: true,
        structural: true,
        dense: true,
    };
}

pub struct Engine {
    pub bundle: Bundle,
    pub weights: Weights,
    reader: IndexReader,
    by_name: HashMap<String, u32>,
    by_lower: HashMap<String, Vec<u32>>,
    by_last: HashMap<String, Vec<u32>>,
    used_by: Vec<Vec<u32>>,
    symbols: Symbols,
    pub dense: Option<crate::dense::Dense>,
}

/// A query split by what it contains.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub prose: Vec<String>,
    pub lean: Vec<String>,
    pub identifiers: Vec<String>,
    pub name: Option<String>,
    /// Text for the dense lane.
    pub dense_text: String,
}

/// Classifies free text: Lean if it has `⊢`, `?x` or `:=`; LaTeX if it has
/// backslash commands or `^{`/`_{`; a name if it is one identifier with a dot
/// or underscore; natural language otherwise.
pub fn classify(q: &str) -> &'static str {
    let t = q.trim();
    if t.contains('⊢')
        || t.contains(":=")
        || t.contains("|-")
        || t.split_whitespace().any(|w| w.starts_with('?'))
    {
        "lean"
    } else if t.contains('\\') || t.contains("^{") || t.contains("_{") {
        "latex"
    } else if !t.contains(char::is_whitespace) && (t.contains('.') || t.contains('_')) {
        "name"
    } else {
        "nl"
    }
}

pub fn parse(q: &Query) -> Parsed {
    let mut p = Parsed::default();
    let mut nl = q.nl.clone();
    let mut latex = q.latex.clone();
    let mut lean: Vec<String> = q.pattern.iter().chain(&q.goal).cloned().collect();
    p.name = q.name.clone();
    if let Some(text) = &q.q {
        match classify(text) {
            "lean" => lean.push(text.clone()),
            "latex" => latex = Some(latex.map_or(text.clone(), |l| format!("{l} {text}"))),
            "name" => p.name = p.name.clone().or(Some(text.clone())),
            _ => nl = Some(nl.map_or(text.clone(), |n| format!("{n} {text}"))),
        }
    }
    let mut dense = Vec::new();
    if let Some(nl) = &nl {
        p.prose.extend(tokens(nl));
        dense.push(nl.clone());
    }
    if let Some(latex) = &latex {
        let unicode = latex_to_unicode(latex);
        // Single-letter variables (`s`, `a`, `x`) match nearly everything.
        p.prose.extend(tokens(&unicode).into_iter().filter(|t| {
            let mut cs = t.chars();
            !matches!((cs.next(), cs.next()), (Some(c), None) if c.is_ascii_alphanumeric())
        }));
        dense.push(latex.clone());
    }
    for l in &lean {
        p.lean.extend(tokens(l));
        p.identifiers.extend(lean_identifiers(l));
        dense.push(l.clone());
    }
    if let Some(name) = &p.name {
        p.lean.extend(tokens(name));
        dense.push(name.clone());
    }
    p.dense_text = dense.join("\n");
    p
}

impl Engine {
    pub fn new(bundle: Bundle, symbols: Symbols) -> anyhow::Result<Engine> {
        let reader = bundle.index.reader()?;
        let mut by_name = HashMap::new();
        let mut by_lower: HashMap<String, Vec<u32>> = HashMap::new();
        let mut by_last: HashMap<String, Vec<u32>> = HashMap::new();
        let mut used_by: Vec<Vec<u32>> = vec![Vec::new(); bundle.decls.len()];
        for (i, d) in bundle.decls.iter().enumerate() {
            let i = i as u32;
            by_name.insert(d.name.clone(), i);
            by_lower.entry(d.name.to_lowercase()).or_default().push(i);
            let last = d.name.rsplit('.').next().unwrap_or(&d.name).to_lowercase();
            by_last.entry(last).or_default().push(i);
            for &dep in &d.deps {
                used_by[dep as usize].push(i);
            }
        }
        for users in &mut used_by {
            users.sort_by(|a, b| {
                let (pa, pb) = (
                    bundle.decls[*a as usize].pagerank,
                    bundle.decls[*b as usize].pagerank,
                );
                pb.total_cmp(&pa).then(a.cmp(b))
            });
            users.truncate(50);
        }
        let symbols = symbols.retain_known(|n| by_name.contains_key(n));
        Ok(Engine {
            bundle,
            weights: Weights::default(),
            reader,
            by_name,
            by_lower,
            by_last,
            used_by,
            symbols,
            dense: None,
        })
    }

    pub fn env_id(&self) -> &str {
        &self.bundle.manifest.env_id
    }

    fn decl(&self, id: u32) -> &StoredDecl {
        &self.bundle.decls[id as usize]
    }

    pub fn id_of(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    pub fn search(&self, q: &Query) -> QueryResult {
        self.search_with(q, Lanes::ALL)
    }

    pub fn search_with(&self, q: &Query, use_lanes: Lanes) -> QueryResult {
        let w = &self.weights;
        let parsed = parse(q);
        let mut lanes: Vec<(&'static str, f64, Vec<u32>)> = Vec::new();

        if use_lanes.lexical && (!parsed.prose.is_empty() || !parsed.lean.is_empty()) {
            lanes.push((
                "lexical",
                w.lexical,
                self.bm25(&parsed.prose, &parsed.lean, LANE_DEPTH),
            ));
        }
        if use_lanes.name
            && let Some(name) = &parsed.name
        {
            lanes.push(("name", w.name, self.name_lane(name)));
        }
        let mut consts: Vec<(String, f32)> = parsed
            .identifiers
            .iter()
            .filter(|i| self.by_name.contains_key(*i))
            .map(|i| (i.clone(), 1.0))
            .collect();
        let all_tokens: Vec<String> = parsed.prose.iter().chain(&parsed.lean).cloned().collect();
        consts.extend(self.symbols.constants(&all_tokens));
        if use_lanes.structural && !consts.is_empty() {
            // Symbols alone are weaker evidence than names written in Lean.
            let weight = if parsed.identifiers.is_empty() {
                w.structural
            } else {
                1.0
            };
            lanes.push(("structural", weight, self.structural(&consts, LANE_DEPTH)));
        }
        if use_lanes.dense
            && let Some(dense) = &self.dense
            && !parsed.dense_text.is_empty()
            && let Some(ids) = dense.search(&parsed.dense_text, LANE_DEPTH)
        {
            lanes.push(("dense", w.dense, ids));
        }

        let mut fused: HashMap<u32, (f64, Vec<&'static str>)> = HashMap::new();
        for (lane, weight, ids) in &lanes {
            for (rank, id) in ids.iter().enumerate() {
                let e = fused.entry(*id).or_default();
                e.0 += weight / (RRF_K + rank as f64 + 1.0);
                if !e.1.contains(lane) {
                    e.1.push(lane);
                }
            }
        }

        // A name that exists exactly is what was asked for.
        if use_lanes.name
            && let Some(id) = parsed.name.as_deref().and_then(|n| self.id_of(n))
        {
            fused.entry(id).or_default().0 += 1.0;
        }

        let prose: HashSet<&str> = parsed.prose.iter().map(String::as_str).collect();
        let wants_def = ["definition", "def", "structure", "class", "type"]
            .iter()
            .any(|w| prose.contains(w));
        let wants_thm = [
            "theorem",
            "lemma",
            "inequality",
            "identity",
            "equality",
            "bound",
        ]
        .iter()
        .any(|w| prose.contains(w));

        // Priors, filters, and folding duplicates into one hit.
        let mut scored: HashMap<u32, (f64, Vec<&'static str>, Option<String>)> = HashMap::new();
        for (id, (mut score, lanes)) in fused {
            let d = self.decl(id);
            if !q.libraries.is_empty() && !q.libraries.contains(&d.library) {
                continue;
            }
            if !q.kinds.is_empty() && !q.kinds.contains(&d.kind) {
                continue;
            }
            if d.uses_sorry && !q.include_sorried {
                continue;
            }
            score += w.pagerank * (1.0 + d.pagerank as f64).ln().min(w.pagerank_cap);
            let def_like = matches!(
                d.kind.as_str(),
                "def" | "abbrev" | "structure" | "class" | "inductive"
            );
            if (wants_def && def_like) || (wants_thm && d.kind == "theorem") {
                score += 0.002;
            }
            // A deprecated alias counts towards its replacement.
            let target = d
                .deprecated_to
                .as_ref()
                .and_then(|t| self.id_of(t))
                .unwrap_or(id);
            let e = scored.entry(target).or_insert((0.0, Vec::new(), None));
            e.0 = e.0.max(score);
            for l in lanes {
                if !e.1.contains(&l) {
                    e.1.push(l);
                }
            }
        }
        // Fold to_additive pairs: keep the better-scored one, name the other.
        let ids: Vec<u32> = scored.keys().copied().collect();
        for id in ids {
            let Some(sib) = self
                .decl(id)
                .additive_sibling
                .as_ref()
                .and_then(|s| self.id_of(s))
            else {
                continue;
            };
            if let (Some(a), Some(b)) =
                (scored.get(&id).map(|e| e.0), scored.get(&sib).map(|e| e.0))
            {
                let (keep, drop) = if a >= b { (id, sib) } else { (sib, id) };
                scored.remove(&drop);
                if let Some(e) = scored.get_mut(&keep) {
                    e.2 = Some(self.decl(drop).name.clone());
                }
            }
        }
        let mut ranked: Vec<(u32, f64, Vec<&'static str>, Option<String>)> = scored
            .into_iter()
            .map(|(id, (s, l, sib))| (id, s, l, sib))
            .collect();
        ranked.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| self.decl(a.0).name.cmp(&self.decl(b.0).name))
        });
        let k = q.k.clamp(1, MAX_K);
        let hits = ranked
            .into_iter()
            .take(k)
            .enumerate()
            .map(|(i, (id, score, lanes, sibling))| self.hit(i + 1, id, score, lanes, sibling))
            .collect();
        QueryResult {
            hits,
            verification: q.verify.then(|| "unavailable".to_string()),
        }
    }

    fn analyzed_terms(&self, field: Field, tokens: &[String]) -> Vec<Term> {
        let Ok(mut analyzer) = self.bundle.index.tokenizer_for_field(field) else {
            return Vec::new();
        };
        let text = tokens.join(" ");
        let mut stream = analyzer.token_stream(&text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(Term::from_field_text(field, &stream.token().text));
        }
        out.sort();
        out.dedup();
        out
    }

    fn top(&self, query: &dyn TQuery, n: usize) -> Vec<u32> {
        let searcher = self.reader.searcher();
        let Ok(top) = searcher.search(query, &TopDocs::with_limit(n).order_by_score()) else {
            return Vec::new();
        };
        top.into_iter()
            .filter_map(|(_, addr)| {
                let doc: TantivyDocument = searcher.doc(addr).ok()?;
                doc.get_first(self.bundle.fields.id)?
                    .as_u64()
                    .map(|v| v as u32)
            })
            .collect()
    }

    /// BM25: prose against names, descriptions, signatures and docstrings;
    /// Lean text against signatures and names only, so a pattern matches
    /// statements rather than prose that mentions the same words.
    pub fn bm25(&self, prose: &[String], lean: &[String], n: usize) -> Vec<u32> {
        let f = self.bundle.fields;
        let w = &self.weights;
        let mut clauses: Vec<(Occur, Box<dyn TQuery>)> = Vec::new();
        let plan = [
            (prose, f.name, w.prose_name),
            (prose, f.informal, w.prose_informal),
            (prose, f.statement, w.prose_statement),
            (prose, f.signature, w.prose_signature),
            (prose, f.doc, w.prose_doc),
            (lean, f.signature, w.lean_signature),
            (lean, f.name, w.lean_name),
        ];
        for (tokens, field, boost) in plan {
            if tokens.is_empty() {
                continue;
            }
            for term in self.analyzed_terms(field, tokens) {
                let q = TermQuery::new(term, IndexRecordOption::WithFreqs);
                clauses.push((Occur::Should, Box::new(BoostQuery::new(Box::new(q), boost))));
            }
        }
        if clauses.is_empty() {
            return Vec::new();
        }
        self.top(&BooleanQuery::new(clauses), n)
    }

    /// Declarations whose types mention the given constants, idf-weighted.
    fn structural(&self, consts: &[(String, f32)], n: usize) -> Vec<u32> {
        let field = self.bundle.fields.consts;
        let clauses: Vec<(Occur, Box<dyn TQuery>)> = consts
            .iter()
            .map(|(c, w)| {
                let q = TermQuery::new(Term::from_field_text(field, c), IndexRecordOption::Basic);
                (
                    Occur::Should,
                    Box::new(BoostQuery::new(Box::new(q), *w)) as Box<dyn TQuery>,
                )
            })
            .collect();
        self.top(&BooleanQuery::new(clauses), n)
    }

    /// Exact name first, then its closest matches.
    fn name_lane(&self, name: &str) -> Vec<u32> {
        let exact = self.id_of(name);
        exact
            .into_iter()
            .chain(
                self.suggest(name, 20)
                    .into_iter()
                    .filter(|id| Some(*id) != exact),
            )
            .collect()
    }

    /// Existing names closest to `name`: same name in other case, same last
    /// component, or shared trigrams; ranked by edit distance, then prominence.
    pub fn suggest(&self, name: &str, n: usize) -> Vec<u32> {
        let lower = name.to_lowercase();
        let last = lower.rsplit('.').next().unwrap_or(&lower).to_string();
        let mut candidates: HashSet<u32> = HashSet::new();
        candidates.extend(self.by_lower.get(&lower).into_iter().flatten());
        candidates.extend(self.by_last.get(&last).into_iter().flatten().take(200));
        let field = self.bundle.fields.name_ngram;
        let grams = self.analyzed_terms(field, std::slice::from_ref(&lower));
        if !grams.is_empty() {
            let clauses: Vec<(Occur, Box<dyn TQuery>)> = grams
                .into_iter()
                .map(|t| {
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(t, IndexRecordOption::Basic)) as Box<dyn TQuery>,
                    )
                })
                .collect();
            candidates.extend(self.top(&BooleanQuery::new(clauses), 100));
        }
        let mut ranked: Vec<(usize, f32, u32)> = candidates
            .into_iter()
            .filter(|&id| self.decl(id).name != name)
            .map(|id| {
                let d = self.decl(id);
                let full = levenshtein(&lower, &d.name.to_lowercase());
                let d_last = d.name.rsplit('.').next().unwrap_or(&d.name).to_lowercase();
                let tail = levenshtein(&last, &d_last);
                (full.min(tail + 1), -d.pagerank, id)
            })
            .collect();
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
        ranked.into_iter().take(n).map(|(_, _, id)| id).collect()
    }

    pub fn resolve(&self, name: &str) -> Resolution {
        let name = name.trim();
        match self.id_of(name) {
            Some(id) => Resolution {
                name: name.to_string(),
                exists: true,
                deprecated_to: self.decl(id).deprecated_to.clone(),
                suggestions: Vec::new(),
            },
            None => Resolution {
                name: name.to_string(),
                exists: false,
                deprecated_to: None,
                suggestions: self
                    .suggest(name, 5)
                    .into_iter()
                    .map(|id| {
                        let d = self.decl(id);
                        Suggestion {
                            name: d.name.clone(),
                            kind: d.kind.clone(),
                            signature: short_signature(d),
                        }
                    })
                    .collect(),
            },
        }
    }

    pub fn detail(&self, name: &str) -> Option<DeclDetail> {
        let id = self.id_of(name)?;
        let d = self.decl(id);
        Some(DeclDetail {
            name: d.name.clone(),
            kind: d.kind.clone(),
            library: d.library.clone(),
            module: d.module.clone(),
            signature: d.signature.clone(),
            type_full: d.type_full.clone(),
            docstring: d.docstring.clone(),
            informal: d.informal.clone(),
            flags: flags(d, None),
            source_url: d.source_url.clone(),
            deps: d.deps.iter().map(|&i| self.decl(i).name.clone()).collect(),
            used_by: self.used_by[id as usize]
                .iter()
                .map(|&i| self.decl(i).name.clone())
                .collect(),
        })
    }

    fn hit(
        &self,
        n: usize,
        id: u32,
        score: f64,
        lanes: Vec<&'static str>,
        sibling: Option<String>,
    ) -> Hit {
        let d = self.decl(id);
        let flags = flags(d, sibling);
        let summary = summary(d);
        Hit {
            name: d.name.clone(),
            kind: d.kind.clone(),
            library: d.library.clone(),
            module: d.module.clone(),
            signature: d.signature.clone(),
            informal_name: d.informal.as_ref().map(|i| i.informal_name.clone()),
            text: render(n, d, summary.as_deref(), &flags),
            summary,
            flags,
            score,
            lanes: lanes.into_iter().map(String::from).collect(),
            source_url: d.source_url.clone(),
        }
    }
}

fn flags(d: &StoredDecl, sibling: Option<String>) -> Flags {
    Flags {
        uses_sorry: d.uses_sorry,
        informal: d.informal_result,
        deprecated_to: d.deprecated_to.clone(),
        additive_sibling: sibling.or_else(|| d.additive_sibling.clone()),
    }
}

/// The signature without its leading name, shortened for display.
pub fn short_signature(d: &StoredDecl) -> String {
    let mut sig = d.signature.strip_prefix(&d.name).unwrap_or(&d.signature);
    // Universe parameters: `.{u_1, u_2}`.
    if let Some(rest) = sig.strip_prefix(".{")
        && let Some(end) = rest.find('}')
    {
        sig = &rest[end + 1..];
    }
    let sig = sig.trim();
    let sig: String = sig.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&sig, 400)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The first sentence of the description, or of the docstring.
fn summary(d: &StoredDecl) -> Option<String> {
    let text = d
        .informal
        .as_ref()
        .map(|i| i.statement_nl.as_str())
        .or(d.docstring.as_deref())?;
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = match text.find(". ") {
        Some(i) => &text[..=i],
        None => &text,
    };
    (!first.is_empty()).then(|| truncate(first, 240))
}

/// About 150 tokens for a model: name, kind and library; signature; a summary;
/// a line of flags when there are any.
fn render(n: usize, d: &StoredDecl, summary: Option<&str>, flags: &Flags) -> String {
    let mut out = format!(
        "{n}. {}   {} · {}\n   {}",
        d.name,
        d.kind,
        d.library,
        short_signature(d)
    );
    if let Some(s) = summary {
        out.push_str("\n   ");
        out.push_str(s);
    }
    let mut notes = Vec::new();
    if flags.uses_sorry {
        notes.push("depends on sorry: not usable as a proof step".to_string());
    }
    if flags.informal {
        notes.push("informal result: stated, not proved".to_string());
    }
    if let Some(t) = &flags.deprecated_to {
        notes.push(format!("deprecated: use {t}"));
    }
    if let Some(s) = &flags.additive_sibling {
        notes.push(format!("additive/multiplicative counterpart: {s}"));
    }
    if !notes.is_empty() {
        out.push_str("\n   ⚠ ");
        out.push_str(&notes.join("; "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_free_text() {
        assert_eq!(classify("⊢ Matrix.trace A = 1"), "lean");
        assert_eq!(classify("Matrix.trace ?A = 1"), "lean");
        assert_eq!(classify(r"S(\rho_{AB}) \le S(\rho_A)"), "latex");
        assert_eq!(classify("Real.sqrt_mul_self"), "name");
        assert_eq!(classify("the trace of a density matrix is one"), "nl");
    }

    #[test]
    fn parse_routes_fields() {
        let p = parse(&Query {
            nl: Some("trace is one".into()),
            pattern: Some("Matrix.trace ?A = 1".into()),
            ..Default::default()
        });
        assert_eq!(p.prose, ["trace", "is", "one"]);
        assert_eq!(p.identifiers, ["Matrix.trace"]);
        assert!(p.lean.contains(&"matrix.trace".to_string()));
    }
}
