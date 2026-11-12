//! Retrieval quality on evaluation sets: MathlibQR (LeanSearch v2's
//! benchmark, Apache-2.0: 200 Mathlib declarations, each with up to six
//! query styles) and the sets in `search/eval/sets/`.
//!
//! Answers name declarations; they are mapped onto the index by name,
//! following deprecation redirects, and dropped when missing.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use pisa_protocol::search::{Hit, Query, Resolution};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::engine::Lanes;
use crate::engine::{Engine, Weights};

pub const MATHLIB_QR_URL: &str = "https://raw.githubusercontent.com/frenzymath/LeanSearch-v2/94f4888cbaf9f4322535755f86cbac690ec18080/benchmark/MathlibQR.json";

/// The query styles, as named in MathlibQR.
pub const STYLES: [&str; 6] = [
    "q1a_lean",
    "q1b_latex",
    "q1c_natural",
    "q2_slogan",
    "q3_nickname",
    "q4_special_case",
];

/// Hits asked for per query: the most a search returns.
const HITS: usize = 50;

/// An evaluation set: queries, each with the declarations it should find.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Set {
    pub name: String,
    /// Where the queries and answers come from, and what to keep in mind.
    #[serde(default)]
    pub about: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    /// The report's row: how the query is written, and where from.
    pub style: String,
    pub split: Split,
    pub query: Query,
    /// The declarations it should find; none for a statement the index
    /// holds no counterpart of.
    #[serde(default)]
    pub answers: Vec<String>,
    /// Left out of the hits before ranking, e.g. the theorem whose
    /// premises are asked for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Where the query was taken from: a paper, a registry entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Half of a set to tune weights on, and half to report them on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Dev,
    Test,
}

/// The split of an item, from its id: stable across runs and machines.
pub fn split_of(id: &str) -> Split {
    if Sha256::digest(id.as_bytes())[0] & 1 == 0 {
        Split::Dev
    } else {
        Split::Test
    }
}

/// MathlibQR's items as published.
#[derive(Debug, Deserialize)]
struct QrItem {
    full_name: String,
    #[serde(flatten)]
    queries: BTreeMap<String, serde_json::Value>,
}

/// MathlibQR as a set: one item per non-empty style, sent as `text`, split
/// by the target so that every style of a target is on one side.
fn from_mathlib_qr(items: Vec<QrItem>) -> Set {
    let mut out = Vec::new();
    for item in items {
        for style in STYLES {
            let Some(text) = (item.queries.get(style))
                .and_then(|q| q.as_str())
                .filter(|q| !q.trim().is_empty())
            else {
                continue;
            };
            out.push(Item {
                id: format!("{}#{style}", item.full_name),
                style: style.to_string(),
                split: split_of(&item.full_name),
                query: Query {
                    text: Some(text.to_string()),
                    ..Default::default()
                },
                answers: vec![item.full_name.clone()],
                exclude: Vec::new(),
                source: None,
            });
        }
    }
    Set {
        name: "MathlibQR".into(),
        about: "LeanSearch v2's benchmark: Mathlib v4.28 declarations, each \
                asked for in up to six styles, all sent as text."
            .into(),
        items: out,
    }
}

/// MathlibQR from `path`, downloaded from the pinned commit if absent.
pub async fn mathlib_qr(path: &Path) -> anyhow::Result<Set> {
    if !path.exists() {
        let text = reqwest::get(MATHLIB_QR_URL)
            .await?
            .error_for_status()?
            .text()
            .await?;
        std::fs::write(path, text)?;
    }
    Ok(from_mathlib_qr(serde_json::from_slice(&std::fs::read(
        path,
    )?)?))
}

pub fn read_set(path: &Path) -> anyhow::Result<Set> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

/// A set as JSON with one item a line, so that changes read as lines.
pub fn write_set(set: &Set) -> anyhow::Result<String> {
    let mut out = format!(
        "{{\"name\": {},\n\"about\": {},\n\"items\": [\n",
        serde_json::to_string(&set.name)?,
        serde_json::to_string(&set.about)?
    );
    let default_limit = Query::default().limit;
    for (i, item) in set.items.iter().enumerate() {
        let mut value = serde_json::to_value(item)?;
        if let Some(query) = value.get_mut("query").and_then(|q| q.as_object_mut())
            && query.get("limit") == Some(&default_limit.into())
        {
            query.remove("limit");
        }
        out.push_str(&serde_json::to_string(&value)?);
        out.push_str(if i + 1 < set.items.len() { ",\n" } else { "\n" });
    }
    out.push_str("]}\n");
    Ok(out)
}

/// Each answer as the index has it: a deprecated name as its replacement,
/// as search folds aliases into their replacements; `None` when missing.
fn map_answers(answers: &[String], found: &HashMap<String, Resolution>) -> Vec<Option<String>> {
    (answers.iter())
        .map(|a| {
            let r = found.get(a).filter(|r| r.exists)?;
            Some(r.deprecated_to.clone().unwrap_or_else(|| a.clone()))
        })
        .collect()
}

/// The rank of each answer among `hits` once `exclude` is left out, counting
/// a hit for its `to_additive` counterpart too, as search folds the two.
fn ranks(answers: &[String], hits: &[Hit], exclude: &[String]) -> Vec<Option<usize>> {
    let kept: Vec<&Hit> = hits.iter().filter(|h| !exclude.contains(&h.name)).collect();
    (answers.iter())
        .map(|a| {
            let at = kept.iter().position(|h| {
                h.name == *a || h.flags.additive_sibling.as_deref() == Some(a.as_str())
            })?;
            Some(at + 1)
        })
        .collect()
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Metrics {
    pub queries: usize,
    pub r1: f64,
    pub r5: f64,
    pub r10: f64,
    pub r50: f64,
    pub mrr: f64,
    pub ndcg10: f64,
}

impl Metrics {
    /// One query, by the rank of each of its answers: R@k is the share of
    /// answers in the first k hits (for one answer, whether it is there),
    /// MRR is at the first answer found, and nDCG@10 is against the best
    /// ranking its answers allow.
    fn add(&mut self, ranks: &[Option<usize>]) {
        self.queries += 1;
        let n = ranks.len().max(1) as f64;
        let within =
            |k: usize| ranks.iter().filter(|r| r.is_some_and(|r| r <= k)).count() as f64 / n;
        self.r1 += within(1);
        self.r5 += within(5);
        self.r10 += within(10);
        self.r50 += within(50);
        if let Some(first) = ranks.iter().flatten().min() {
            self.mrr += 1.0 / *first as f64;
        }
        let gain = |r: usize| 1.0 / (r as f64 + 1.0).log2();
        let dcg: f64 = ranks
            .iter()
            .flatten()
            .filter(|r| **r <= 10)
            .map(|r| gain(*r))
            .sum();
        let ideal: f64 = (1..=ranks.len().min(10)).map(gain).sum();
        if ideal > 0.0 {
            self.ndcg10 += dcg / ideal;
        }
    }

    fn finish(mut self) -> Self {
        let n = self.queries.max(1) as f64;
        for v in [
            &mut self.r1,
            &mut self.r5,
            &mut self.r10,
            &mut self.r50,
            &mut self.mrr,
            &mut self.ndcg10,
        ] {
            *v /= n;
        }
        self
    }
}

pub struct Report {
    pub set: String,
    pub split: Option<Split>,
    /// Items with at least one answer in this index.
    pub scored: usize,
    /// Items whose answers are all missing from this index, or that have none.
    pub unanswered: Vec<String>,
    /// Answers missing from this index.
    pub missing: Vec<String>,
    /// (lanes label, style or "all") → metrics.
    pub rows: BTreeMap<(String, String), Metrics>,
}

/// Queries sent per search request.
const BATCH: usize = 64;

/// Runs every item of `set` in `split` (all when `None`) under each `(label,
/// lanes, weights)` configuration.
pub async fn run(
    engine: &mut Engine,
    set: &Set,
    split: Option<Split>,
    configs: &[(String, Lanes, Weights)],
) -> anyhow::Result<Report> {
    let items: Vec<&Item> = (set.items.iter())
        .filter(|i| split.is_none_or(|s| i.split == s))
        .collect();
    let mut names: Vec<String> = items.iter().flat_map(|i| i.answers.clone()).collect();
    names.sort();
    names.dedup();
    let mut found = HashMap::new();
    for chunk in names.chunks(BATCH) {
        for r in engine.resolve(chunk).await? {
            found.insert(r.name.clone(), r);
        }
    }
    let mut missing: Vec<String> = Vec::new();
    let mut unanswered = Vec::new();
    let mut scored: Vec<(&Item, Vec<String>)> = Vec::new();
    for item in items {
        let mut answers: Vec<String> = Vec::new();
        for (name, mapped) in item.answers.iter().zip(map_answers(&item.answers, &found)) {
            match mapped {
                Some(m) if !answers.contains(&m) => answers.push(m),
                Some(_) => {}
                None if !missing.contains(name) => missing.push(name.clone()),
                None => {}
            }
        }
        if answers.is_empty() {
            unanswered.push(item.id.clone());
        } else {
            scored.push((item, answers));
        }
    }
    let mut rows: BTreeMap<(String, String), Metrics> = BTreeMap::new();
    for (label, lanes, weights) in configs {
        engine.weights = weights.clone();
        for chunk in scored.chunks(BATCH) {
            let queries: Vec<Query> = chunk
                .iter()
                .map(|(item, _)| Query {
                    limit: HITS,
                    ..item.query.clone()
                })
                .collect();
            let results = engine.search_batch(&queries, *lanes).await?;
            for ((item, answers), r) in chunk.iter().zip(results) {
                let ranks = ranks(answers, &r.hits, &item.exclude);
                for key in [item.style.as_str(), "all"] {
                    rows.entry((label.clone(), key.to_string()))
                        .or_default()
                        .add(&ranks);
                }
            }
        }
    }
    let rows = rows.into_iter().map(|(k, m)| (k, m.finish())).collect();
    Ok(Report {
        set: set.name.clone(),
        split,
        scored: scored.len(),
        unanswered,
        missing,
        rows,
    })
}

/// Names listed in a report line, the rest counted.
const LISTED: usize = 20;

fn listed(names: &[String]) -> String {
    let mut out = names
        .iter()
        .take(LISTED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > LISTED {
        out.push_str(&format!(" and {} more", names.len() - LISTED));
    }
    out
}

impl Report {
    pub fn markdown(&self, env_id: &str) -> String {
        let split = match self.split {
            Some(Split::Dev) => ", dev split",
            Some(Split::Test) => ", test split",
            None => "",
        };
        let mut out = format!(
            "# {} ({env_id}{split})\n\n{} of {} queries have an answer in this index.",
            self.set,
            self.scored,
            self.scored + self.unanswered.len(),
        );
        if !self.unanswered.is_empty() {
            out.push_str(&format!(" Without one: {}.", listed(&self.unanswered)));
        }
        if !self.missing.is_empty() {
            out.push_str(&format!(
                " Answers missing from this index: {}.",
                listed(&self.missing)
            ));
        }
        out.push_str(
            "\n\n| Lanes | Style | Queries | R@1 | R@5 | R@10 | R@50 | MRR | nDCG@10 |\n\
             | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
        );
        for ((lanes, style), m) in &self.rows {
            out.push_str(&format!(
                "| {lanes} | {style} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
                m.queries, m.r1, m.r5, m.r10, m.r50, m.mrr, m.ndcg10
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::search::Flags;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn metrics_average_over_queries() {
        let mut m = Metrics::default();
        m.add(&[Some(1)]);
        m.add(&[Some(4)]);
        m.add(&[None]);
        let m = m.finish();
        assert_eq!(m.queries, 3);
        assert!(close(m.r1, 1.0 / 3.0));
        assert!(close(m.r5, 2.0 / 3.0));
        assert!(close(m.mrr, (1.0 + 0.25) / 3.0));
        let ndcg = (1.0 + 1.0 / 5f64.log2()) / 3.0;
        assert!(close(m.ndcg10, ndcg), "{}", m.ndcg10);
    }

    /// With several answers, R@k is the share found within k, MRR is at the
    /// first one found, and nDCG@10 is 1 for the best ranking they allow.
    #[test]
    fn metrics_share_several_answers() {
        let mut m = Metrics::default();
        m.add(&[Some(2), Some(7), None, Some(30)]);
        let m = m.finish();
        assert!(close(m.r1, 0.0));
        assert!(close(m.r5, 0.25));
        assert!(close(m.r10, 0.5));
        assert!(close(m.r50, 0.75));
        assert!(close(m.mrr, 0.5));
        let dcg = 1.0 / 3f64.log2() + 1.0 / 8f64.log2();
        let ideal: f64 = (1..=4).map(|i| 1.0 / (i as f64 + 1.0).log2()).sum();
        assert!(close(m.ndcg10, dcg / ideal), "{}", m.ndcg10);
        let mut best = Metrics::default();
        best.add(&[Some(2), Some(1)]);
        assert!(close(best.finish().ndcg10, 1.0));
    }

    fn hit(name: &str, sibling: Option<&str>) -> Hit {
        Hit {
            name: name.into(),
            kind: "theorem".into(),
            library: "Mathlib".into(),
            module: "M".into(),
            signature: String::new(),
            title: None,
            summary: None,
            flags: Flags {
                additive_sibling: sibling.map(String::from),
                ..Default::default()
            },
            score: 0.0,
            source_url: None,
            text: String::new(),
            snapshot_ids: Vec::new(),
            status: None,
            verified: None,
        }
    }

    /// An excluded hit doesn't take a rank, and a hit counts for the
    /// `to_additive` counterpart folded into it.
    #[test]
    fn ranks_skip_excluded_hits_and_count_folded_counterparts() {
        let hits = [
            hit("self_thm", None),
            hit("mul_comm", Some("add_comm")),
            hit("other", None),
            hit("wanted", None),
        ];
        let answers = ["wanted".to_string(), "add_comm".into(), "absent".into()];
        let exclude = ["self_thm".to_string()];
        assert_eq!(ranks(&answers, &hits, &exclude), [Some(3), Some(1), None]);
    }

    #[test]
    fn answers_follow_deprecations_and_drop_what_is_missing() {
        let resolution = |name: &str, exists: bool, to: Option<&str>| Resolution {
            name: name.into(),
            exists,
            deprecated_to: to.map(String::from),
            imports: None,
            suggestions: Vec::new(),
        };
        let found: HashMap<String, Resolution> = [
            resolution("old", true, Some("new")),
            resolution("same", true, None),
            resolution("gone", false, None),
        ]
        .into_iter()
        .map(|r| (r.name.clone(), r))
        .collect();
        let answers = [
            "old".to_string(),
            "same".into(),
            "gone".into(),
            "unasked".into(),
        ];
        assert_eq!(
            map_answers(&answers, &found),
            [Some("new".to_string()), Some("same".into()), None, None]
        );
    }

    /// Every style of a target lands on one side, the same on every run.
    #[test]
    fn mathlib_qr_becomes_a_set_split_by_target() {
        let items: Vec<QrItem> = serde_json::from_value(serde_json::json!([
            {"full_name": "Nat.add_comm", "id": 1, "q1a_lean": "a + b = b + a",
             "q2_slogan": "addition commutes", "q3_nickname": ""},
            {"full_name": "Nat.mul_comm", "q1c_natural": "multiplication commutes"},
        ]))
        .unwrap();
        let set = from_mathlib_qr(items);
        assert_eq!(set.name, "MathlibQR");
        let styles: Vec<(&str, &str)> = (set.items.iter())
            .map(|i| (i.style.as_str(), i.query.text.as_deref().unwrap()))
            .collect();
        assert_eq!(
            styles,
            [
                ("q1a_lean", "a + b = b + a"),
                ("q2_slogan", "addition commutes"),
                ("q1c_natural", "multiplication commutes"),
            ]
        );
        assert_eq!(set.items[0].answers, ["Nat.add_comm"]);
        assert_eq!(set.items[0].split, set.items[1].split);
        assert_eq!(set.items[0].split, split_of("Nat.add_comm"));
        assert_eq!(split_of("Nat.add_comm"), split_of("Nat.add_comm"));
        // Both halves get items.
        let dev = (0..200)
            .filter(|i| split_of(&format!("t{i}")) == Split::Dev)
            .count();
        assert!((60..140).contains(&dev), "{dev}");
    }

    #[test]
    fn a_set_written_reads_back() {
        let set = Set {
            name: "s".into(),
            about: "a \"quoted\" note".into(),
            items: vec![
                Item {
                    id: "x".into(),
                    style: "nl".into(),
                    split: Split::Dev,
                    query: Query {
                        text: Some("q".into()),
                        ..Default::default()
                    },
                    answers: vec!["A".into()],
                    exclude: Vec::new(),
                    source: None,
                };
                2
            ],
        };
        let text = write_set(&set).unwrap();
        assert_eq!(text.lines().count(), 6, "{text}");
        // The default limit isn't written: a run asks for its own.
        assert!(!text.contains("limit"), "{text}");
        let back: Set = serde_json::from_str(&text).unwrap();
        assert_eq!(back.about, set.about);
        assert_eq!(back.items.len(), 2);
        assert_eq!(back.items[1].answers, ["A"]);
    }

    #[test]
    fn a_set_reads_with_its_optional_fields() {
        let set: Set = serde_json::from_value(serde_json::json!({
            "name": "physics-papers",
            "items": [
                {"id": "doc-6", "style": "doc/latex", "split": "test",
                 "query": {"latex": "\\operatorname{var}(A)"},
                 "answers": [], "source": "https://www.alphaxiv.org/abs/2310.05039"},
                {"id": "p-1", "style": "QuantumInfo", "split": "dev",
                 "query": {"goal": "⊢ x = x"}, "answers": ["a"], "exclude": ["p"]}
            ]
        }))
        .unwrap();
        assert_eq!(set.items[0].split, Split::Test);
        assert!(set.items[0].answers.is_empty());
        assert_eq!(set.items[1].exclude, ["p"]);
        assert_eq!(set.items[1].query.goal.as_deref(), Some("⊢ x = x"));
    }
}
