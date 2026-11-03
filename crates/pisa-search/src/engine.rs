//! Answering queries over a bundle: routing, lanes, fusion, priors, rendering.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pisa_protocol::labels::check_labels;
use pisa_protocol::search::{
    DeclDetail, Flags, Hit, Query, QueryResult, Resolution, SnapshotScope, Suggestion,
};
use tantivy::collector::TopDocs;
use tantivy::query::{
    Bm25StatisticsProvider, BooleanQuery, BoostQuery, Occur, Query as TQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, Value};
use tantivy::{IndexReader, Searcher, TantivyDocument, Term};

use crate::corpus::{is_generated, is_tooling};
use crate::dense::quantize;
use crate::index::{Bundle, StoredDecl};
use crate::snapshots::{self, Combined, Scope, SnapshotIndex, Snapshots};
use crate::symbols::Symbols;
use crate::text::{latex_to_unicode, lean_identifiers, levenshtein, tokens};

/// Candidates each lane contributes to fusion.
const LANE_DEPTH: usize = 100;
/// Lexical hits checked for vectors to weight the dense lane.
const COVERAGE_PROBE: usize = 20;
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
            dense: 2.0,
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
    kinds: std::collections::BTreeSet<String>,
    pub dense: Option<crate::dense::Dense>,
    /// Snapshot declarations, when this server searches them.
    pub snapshots: Option<Arc<Snapshots>>,
}

/// The snapshot index and the modules a query covers.
type Snap<'a> = Option<(&'a SnapshotIndex, &'a Scope)>;

/// The declarations one query can see: the bundle's, and those of the
/// snapshot modules it covers.
struct View<'a> {
    engine: &'a Engine,
    snap: Snap<'a>,
}

impl View<'_> {
    fn decl(&self, id: u32) -> &StoredDecl {
        match self.engine.bundle.decls.get(id as usize) {
            Some(d) => d,
            // The query holds the snapshot index, so its ids stay live.
            None => self
                .snap
                .and_then(|(index, _)| index.decl(id))
                .expect("a live snapshot declaration"),
        }
    }

    /// The declarations named exactly `name`.
    fn exact(&self, name: &str) -> Vec<u32> {
        let mut ids: Vec<u32> = self.engine.id_of(name).into_iter().collect();
        if let Some((index, scope)) = self.snap {
            ids.extend(index.exact(name, scope));
        }
        ids
    }

    fn has_vector(&self, id: u32) -> bool {
        match (&self.engine.dense, self.snap) {
            _ if (id as usize) >= self.engine.bundle.decls.len() => {
                self.snap.is_some_and(|(index, _)| index.has_vector(id))
            }
            (Some(dense), _) => dense.contains(id),
            (None, _) => false,
        }
    }
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
    // Blank fields count as absent.
    let field = |f: &Option<String>| {
        f.as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    let mut p = Parsed::default();
    let mut nl = field(&q.nl);
    let mut latex = field(&q.latex);
    let mut lean: Vec<String> = field(&q.pattern)
        .into_iter()
        .chain(field(&q.goal))
        .collect();
    p.name = field(&q.name);
    if let Some(text) = &field(&q.q) {
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
    pub fn new(mut bundle: Bundle, symbols: Symbols) -> anyhow::Result<Engine> {
        let dense = bundle.dense.take();
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
        let bundle_kinds = bundle.decls.iter().map(|d| d.kind.clone()).collect();
        Ok(Engine {
            bundle,
            weights: Weights::default(),
            reader,
            by_name,
            by_lower,
            by_last,
            used_by,
            symbols,
            kinds: bundle_kinds,
            dense,
            snapshots: None,
        })
    }

    /// Spells a query's library and kind filters as the index does, matching
    /// case-insensitively; an unknown one is an error listing the known ones.
    pub fn check_filters(&self, q: &mut Query) -> Result<(), String> {
        match &q.snapshots {
            SnapshotScope::None => {}
            _ if self.snapshots.is_none() => {
                return Err("this server doesn't search snapshots".into());
            }
            SnapshotScope::All => {}
            SnapshotScope::Labels(labels) if labels.is_empty() => {
                return Err("snapshots: no labels given; use \"all\" for every snapshot".into());
            }
            SnapshotScope::Labels(labels) => check_labels(labels)?,
        }
        let snapshot_library = snapshots::LIBRARY.to_string();
        let mut libraries: Vec<&String> = self.bundle.manifest.libraries.keys().collect();
        if self.snapshots.is_some() {
            libraries.push(&snapshot_library);
        }
        let kinds: Vec<&String> = self.kinds.iter().collect();
        for (wanted, known, what) in [
            (&mut q.libraries, &libraries, "library"),
            (&mut q.kinds, &kinds, "kind"),
        ] {
            for w in wanted.iter_mut() {
                match known.iter().find(|k| k.eq_ignore_ascii_case(w)) {
                    Some(k) => *w = k.to_string(),
                    None => {
                        let names: Vec<String> = known.iter().map(|k| k.to_string()).collect();
                        return Err(format!("unknown {what} {w:?}; known: {}", names.join(", ")));
                    }
                }
            }
        }
        if q.snapshots.is_none() && q.libraries.contains(&snapshot_library) {
            return Err(format!(
                "library {snapshot_library} needs snapshots: \"all\" or labels"
            ));
        }
        Ok(())
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

    /// The bundle's declaration named `name`.
    pub fn base_decl(&self, name: &str) -> Option<&StoredDecl> {
        self.id_of(name).map(|id| self.decl(id))
    }

    pub fn search(&self, q: &Query) -> QueryResult {
        self.search_with(q, Lanes::ALL)
    }

    /// The snapshot index, read-locked, if any of `queries` asks for
    /// snapshots. It is locked once, outside the rayon tasks that search with
    /// it: a task waiting on its own nested `par_iter` may run another
    /// query's, which would otherwise lock it again and wait behind a writer.
    fn snapshot_index<'a>(
        &'a self,
        queries: &[&Query],
    ) -> Option<std::sync::RwLockReadGuard<'a, SnapshotIndex>> {
        let s = self.snapshots.as_ref()?;
        queries
            .iter()
            .any(|q| !q.snapshots.is_none())
            .then(|| s.index.read().unwrap())
    }

    /// Several queries, embedded for the dense lane in one request and
    /// otherwise searched in parallel; results in order.
    pub fn search_batch(&self, queries: &[Query]) -> Vec<QueryResult> {
        use rayon::prelude::*;
        let guard = self.snapshot_index(&queries.iter().collect::<Vec<_>>());
        let index = guard.as_deref();
        let parsed: Vec<Parsed> = queries.iter().map(parse).collect();
        let wanted: Vec<usize> = (0..queries.len())
            .filter(|&i| queries[i].k > 0 && !parsed[i].dense_text.is_empty())
            .collect();
        let mut dense: Vec<Option<Result<Vec<f32>, String>>> = vec![None; queries.len()];
        if let Some(d) = &self.dense
            && !wanted.is_empty()
        {
            let texts: Vec<&str> = wanted
                .iter()
                .map(|&i| parsed[i].dense_text.as_str())
                .collect();
            match d.embed_queries(&texts) {
                Some(Ok(found)) => {
                    for (&i, v) in wanted.iter().zip(found) {
                        dense[i] = Some(Ok(v));
                    }
                }
                Some(Err(e)) => {
                    for &i in &wanted {
                        dense[i] = Some(Err(e.to_string()));
                    }
                }
                None => {}
            }
        }
        queries
            .par_iter()
            .zip(parsed)
            .zip(dense)
            .map(|((q, p), d)| self.search_parsed(q, p, Lanes::ALL, d, index))
            .collect()
    }

    pub fn search_with(&self, q: &Query, use_lanes: Lanes) -> QueryResult {
        let parsed = parse(q);
        let dense = match &self.dense {
            Some(d) if use_lanes.dense && q.k > 0 && !parsed.dense_text.is_empty() => {
                d.embed_queries(&[parsed.dense_text.as_str()]).map(|r| {
                    r.map(|mut v| v.pop().unwrap_or_default())
                        .map_err(|e| e.to_string())
                })
            }
            _ => None,
        };
        let guard = self.snapshot_index(&[q]);
        self.search_parsed(q, parsed, use_lanes, dense, guard.as_deref())
    }

    /// `dense`: the query's vector for the dense lane, or why there is none.
    /// `index`: the snapshot index, locked by the caller.
    fn search_parsed(
        &self,
        q: &Query,
        parsed: Parsed,
        use_lanes: Lanes,
        dense: Option<Result<Vec<f32>, String>>,
        index: Option<&SnapshotIndex>,
    ) -> QueryResult {
        if q.k == 0 {
            return QueryResult::default();
        }
        let w = &self.weights;
        let mut warnings = Vec::new();
        if let Some(s) = &self.snapshots
            && !q.snapshots.is_none()
            && !s.loaded()
        {
            warnings.push("snapshots are still loading: results may lack them".into());
        }
        let scope = index.and_then(|index| Some((index, index.scope(&q.snapshots)?)));
        let view = View {
            engine: self,
            snap: scope.as_ref().map(|(index, scope)| (*index, scope)),
        };
        let mut lanes: Vec<(&'static str, f64, Vec<u32>)> = Vec::new();

        if use_lanes.lexical
            && let Some(query) = self.bm25_query(&parsed.prose, &parsed.lean)
        {
            lanes.push(("lexical", w.lexical, self.top_in(&query, view.snap)));
        }
        if use_lanes.name
            && let Some(name) = &parsed.name
        {
            lanes.push(("name", w.name, self.name_lane(name, &view)));
        }
        let mut consts: Vec<(String, f32)> = parsed
            .identifiers
            .iter()
            .filter(|i| {
                self.by_name.contains_key(*i)
                    || view.snap.is_some_and(|(index, _)| index.has_name(i))
            })
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
            let query = self.structural_query(&consts);
            lanes.push(("structural", weight, self.top_in(&query, view.snap)));
        }
        if use_lanes.dense
            && let Some(found) = dense
            && let Some(dense) = &self.dense
        {
            let ids = match found {
                Ok(v) => {
                    let v = quantize(&v);
                    let mut near = dense.nearest_scored(&v, LANE_DEPTH);
                    if let Some((index, scope)) = view.snap {
                        near.extend(index.nearest(&v, LANE_DEPTH, scope));
                        near = crate::dense::best(near, LANE_DEPTH);
                    }
                    near.into_iter().map(|(_, id)| id).collect()
                }
                Err(e) => {
                    tracing::warn!("query embedding failed: {e}");
                    warnings.push(format!(
                        "dense lane unavailable, results from the other lanes only: {e}"
                    ));
                    Vec::new()
                }
            };
            // Only some declarations have vectors. Trust the lane as far as it
            // covers the lexical lane's top hits, so a query about uncovered
            // declarations isn't flooded with the nearest covered ones.
            let coverage = match lanes.iter().find(|(lane, ..)| *lane == "lexical") {
                Some((_, _, lexical)) if !lexical.is_empty() => {
                    let top = &lexical[..lexical.len().min(COVERAGE_PROBE)];
                    let covered = top.iter().filter(|&&id| view.has_vector(id)).count();
                    covered as f64 / top.len() as f64
                }
                _ => 1.0,
            };
            if coverage > 0.0 && !ids.is_empty() {
                lanes.push(("dense", w.dense * coverage, ids));
            }
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
        let exact_ids = parsed.name.as_deref().map_or(Vec::new(), |n| view.exact(n));
        if use_lanes.name {
            for &id in &exact_ids {
                fused.entry(id).or_default().0 += 1.0;
            }
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
            let d = view.decl(id);
            if !q.libraries.is_empty() && !q.libraries.contains(&d.library) {
                continue;
            }
            if !q.kinds.is_empty() && !q.kinds.contains(&d.kind) {
                continue;
            }
            // An exact name match is shown whatever it is, flagged.
            let exact = exact_ids.contains(&id);
            if !exact
                && (d.uses_sorry && !q.include_sorried
                    || is_generated(&d.name)
                    || is_tooling(&d.name))
            {
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
                .and_then(|t| view.exact(t).first().copied())
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
            let Some(sib) = view
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
                    e.2 = Some(view.decl(drop).name.clone());
                }
            }
        }
        let mut ranked: Vec<(u32, f64, Vec<&'static str>, Option<String>)> = scored
            .into_iter()
            .map(|(id, (s, l, sib))| (id, s, l, sib))
            .collect();
        ranked.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| view.decl(a.0).name.cmp(&view.decl(b.0).name))
                .then(a.0.cmp(&b.0))
        });
        let k = q.k.min(MAX_K);
        let hits = ranked
            .into_iter()
            .take(k)
            .enumerate()
            .map(|(i, (id, score, lanes, sibling))| {
                self.hit(&view, i + 1, id, score, lanes, sibling)
            })
            .collect();
        QueryResult {
            hits,
            verification: q.verify.then(|| "unavailable".to_string()),
            warnings,
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
        self.top_scored(&searcher, query, &searcher, n)
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    fn top_scored(
        &self,
        searcher: &Searcher,
        query: &dyn TQuery,
        stats: &dyn Bm25StatisticsProvider,
        n: usize,
    ) -> Vec<(f32, u32)> {
        let collector = TopDocs::with_limit(n).order_by_score();
        let Ok(top) = searcher.search_with_statistics_provider(query, &collector, stats) else {
            return Vec::new();
        };
        top.into_iter()
            .filter_map(|(score, addr)| {
                let doc: TantivyDocument = searcher.doc(addr).ok()?;
                let id = doc.get_first(self.bundle.fields.id)?.as_u64()?;
                Some((score, id as u32))
            })
            .collect()
    }

    /// The best [`LANE_DEPTH`] for `query` in the bundle and, if the query
    /// covers any, the snapshot modules, scored alike.
    fn top_in(&self, query: &dyn TQuery, snap: Snap<'_>) -> Vec<u32> {
        self.top_merged(query, LANE_DEPTH, snap)
    }

    fn top_merged(&self, query: &dyn TQuery, n: usize, snap: Snap<'_>) -> Vec<u32> {
        let Some((index, scope)) = snap else {
            return self.top(query, n);
        };
        let base = self.reader.searcher();
        let other = index.searcher();
        let stats = Combined(&base, &other);
        let mut scored = self.top_scored(&base, query, &stats, n);
        scored.extend(index.top(&other, query, &stats, n, scope));
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(n).map(|(_, id)| id).collect()
    }

    /// BM25: prose against names, descriptions, signatures and docstrings;
    /// Lean text against signatures and names only, so a pattern matches
    /// statements rather than prose that mentions the same words.
    pub fn bm25(&self, prose: &[String], lean: &[String], n: usize) -> Vec<u32> {
        match self.bm25_query(prose, lean) {
            Some(query) => self.top(&query, n),
            None => Vec::new(),
        }
    }

    fn bm25_query(&self, prose: &[String], lean: &[String]) -> Option<BooleanQuery> {
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
            return None;
        }
        Some(BooleanQuery::new(clauses))
    }

    /// Declarations whose types mention the given constants, idf-weighted.
    fn structural_query(&self, consts: &[(String, f32)]) -> BooleanQuery {
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
        BooleanQuery::new(clauses)
    }

    /// Exact names first, then the closest matches.
    fn name_lane(&self, name: &str, view: &View) -> Vec<u32> {
        let exact = view.exact(name);
        let suggested = self.suggest_in(name, 20, view);
        let mut ids = exact.clone();
        ids.extend(suggested.into_iter().filter(|id| !exact.contains(id)));
        ids
    }

    /// Existing names closest to `name`: same name in other case, same last
    /// component, or shared trigrams; ranked by edit distance, then prominence.
    pub fn suggest(&self, name: &str, n: usize) -> Vec<u32> {
        self.suggest_in(
            name,
            n,
            &View {
                engine: self,
                snap: None,
            },
        )
    }

    fn suggest_in(&self, name: &str, n: usize, view: &View) -> Vec<u32> {
        let lower = name.to_lowercase();
        let last = lower.rsplit('.').next().unwrap_or(&lower).to_string();
        let mut candidates: HashSet<u32> = HashSet::new();
        candidates.extend(self.by_lower.get(&lower).into_iter().flatten());
        candidates.extend(self.by_last.get(&last).into_iter().flatten().take(200));
        if let Some((index, scope)) = view.snap {
            candidates.extend(index.name_candidates(&lower, &last, scope));
        }
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
            let query = BooleanQuery::new(clauses);
            candidates.extend(self.top(&query, 100));
            if let Some((index, scope)) = view.snap {
                let searcher = index.searcher();
                let found = index.top(&searcher, &query, &searcher, 100, scope);
                candidates.extend(found.into_iter().map(|(_, id)| id));
            }
        }
        let mut ranked: Vec<(usize, f32, u32)> = candidates
            .into_iter()
            .filter(|&id| view.decl(id).name != name)
            .map(|id| {
                let d = view.decl(id);
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
            deps: d
                .deps
                .iter()
                .map(|&i| self.decl(i))
                .filter(|dep| !is_plumbing(d, dep))
                .map(|dep| dep.name.clone())
                .collect(),
            used_by: self.used_by[id as usize]
                .iter()
                .map(|&i| self.decl(i))
                .filter(|user| !is_plumbing(d, user))
                .map(|user| user.name.clone())
                .collect(),
        })
    }

    fn hit(
        &self,
        view: &View,
        n: usize,
        id: u32,
        score: f64,
        lanes: Vec<&'static str>,
        sibling: Option<String>,
    ) -> Hit {
        let d = view.decl(id);
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
            snapshot: view.snap.and_then(|(index, scope)| index.hit(id, scope)),
        }
    }
}

fn flags(d: &StoredDecl, sibling: Option<String>) -> Flags {
    Flags {
        uses_sorry: d.uses_sorry,
        informal: d.informal_result,
        deprecated_to: d.deprecated_to.clone(),
        additive_sibling: sibling.or_else(|| d.additive_sibling.clone()),
        imports: d.imports.clone(),
    }
}

/// Instances, typeclasses and Lean's own logic and notation (`Eq`,
/// `HMul.hMul`, `congrArg`) that `d` mentions without being about them.
fn is_plumbing(d: &StoredDecl, other: &StoredDecl) -> bool {
    let core = |l: &str| matches!(l, "Init" | "Std" | "Lean");
    matches!(other.kind.as_str(), "instance" | "class")
        || is_generated(&other.name)
        || (core(&other.library) && !core(&d.library))
}

/// The signature without its leading name, universe parameters or instance
/// arguments (`[Fintype n]`), shortened for display.
pub fn short_signature(d: &StoredDecl) -> String {
    let mut sig = d.signature.strip_prefix(&d.name).unwrap_or(&d.signature);
    // Universe parameters: `.{u_1, u_2}`.
    if let Some(rest) = sig.strip_prefix(".{")
        && let Some(end) = rest.find('}')
    {
        sig = &rest[end + 1..];
    }
    let sig = drop_instance_binders(sig.trim());
    let sig: String = sig.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&sig, 400)
}

/// Removes `[...]` binders, and `{inst✝ : ...}` ones, from the binders before
/// a signature's type; brackets inside the type are left alone.
fn drop_instance_binders(sig: &str) -> String {
    let mut out = String::new();
    let mut rest = sig;
    loop {
        let trimmed = rest.trim_start();
        let Some(open) = trimmed.chars().next().filter(|c| "([{⦃".contains(*c)) else {
            out.push_str(trimmed);
            return out;
        };
        let mut depth = 0usize;
        let mut end = None;
        for (i, c) in trimmed.char_indices() {
            match c {
                '(' | '[' | '{' | '⦃' => depth += 1,
                ')' | ']' | '}' | '⦄' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + c.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            out.push_str(trimmed);
            return out;
        };
        let group = &trimmed[..end];
        let instance = open == '[' || (open == '{' && group[1..].trim_start().starts_with("inst"));
        if !instance {
            out.push_str(group);
            out.push(' ');
        }
        rest = &trimmed[end..];
    }
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
    if let Some(h) = &flags.imports {
        notes.push(format!("load with `{h}` instead of the default imports"));
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
    fn signatures_drop_instance_binders() {
        assert_eq!(
            drop_instance_binders(
                "{n : Type u_3} [Fintype n] [DecidableEq n] {inst✝ : Ring R} (A : Matrix n n R) : (A * 1).trace = A.trace"
            ),
            "{n : Type u_3} (A : Matrix n n R) : (A * 1).trace = A.trace"
        );
        assert_eq!(
            drop_instance_binders(": [a, b].length = 2"),
            ": [a, b].length = 2"
        );
        assert_eq!(
            drop_instance_binders("[(c : C) → AddCommGroup (V c)] (x : V c) : x = x"),
            "(x : V c) : x = x"
        );
    }

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
