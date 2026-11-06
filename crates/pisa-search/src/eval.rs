//! Retrieval quality on MathlibQR (LeanSearch v2's benchmark, Apache-2.0):
//! 200 Mathlib declarations, each with up to six query styles.
//!
//! Targets name Mathlib v4.28 declarations; they are mapped onto ours by
//! name, following deprecation redirects, and dropped when missing.

use std::collections::BTreeMap;
use std::path::Path;

use pisa_protocol::search::Query;
use serde::Deserialize;

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

#[derive(Debug, Deserialize)]
pub struct Item {
    pub full_name: String,
    #[serde(flatten)]
    pub queries: BTreeMap<String, serde_json::Value>,
}

impl Item {
    pub fn query(&self, style: &str) -> Option<&str> {
        self.queries
            .get(style)?
            .as_str()
            .filter(|q| !q.trim().is_empty())
    }
}

pub async fn load(path: &Path) -> anyhow::Result<Vec<Item>> {
    if !path.exists() {
        let text = reqwest::get(MATHLIB_QR_URL)
            .await?
            .error_for_status()?
            .text()
            .await?;
        std::fs::write(path, text)?;
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
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
    fn add(&mut self, rank: Option<usize>) {
        self.queries += 1;
        if let Some(r) = rank {
            self.r1 += f64::from(u8::from(r <= 1));
            self.r5 += f64::from(u8::from(r <= 5));
            self.r10 += f64::from(u8::from(r <= 10));
            self.r50 += f64::from(u8::from(r <= 50));
            self.mrr += 1.0 / r as f64;
            if r <= 10 {
                self.ndcg10 += 1.0 / (r as f64 + 1.0).log2();
            }
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
    pub targets: usize,
    pub missing: Vec<String>,
    /// (lanes label, style or "all") → metrics.
    pub rows: BTreeMap<(String, String), Metrics>,
}

/// Queries sent per search request.
const BATCH: usize = 64;

/// Runs every query style of every item under each `(label, lanes,
/// weights)` configuration.
pub async fn run(
    engine: &mut Engine,
    items: &[Item],
    configs: &[(String, Lanes, Weights)],
) -> anyhow::Result<Report> {
    let mut targets = Vec::new();
    let mut missing = Vec::new();
    for chunk in items.chunks(BATCH) {
        let names: Vec<String> = chunk.iter().map(|i| i.full_name.clone()).collect();
        for (item, r) in chunk.iter().zip(engine.resolve(&names).await?) {
            if r.exists {
                targets.push(item);
            } else {
                missing.push(item.full_name.clone());
            }
        }
    }
    let mut rows: BTreeMap<(String, String), Metrics> = BTreeMap::new();
    for (label, lanes, weights) in configs {
        engine.weights = weights.clone();
        for style in STYLES {
            let asked: Vec<(&str, &str)> = targets
                .iter()
                .filter_map(|item| Some((item.full_name.as_str(), item.query(style)?)))
                .collect();
            for chunk in asked.chunks(BATCH) {
                let queries: Vec<Query> = chunk
                    .iter()
                    .map(|(_, text)| Query {
                        text: Some(text.to_string()),
                        limit: 50,
                        ..Default::default()
                    })
                    .collect();
                let results = engine.search_batch(&queries, *lanes).await?;
                for ((target, _), r) in chunk.iter().zip(results) {
                    let rank = r.hits.iter().position(|h| h.name == *target).map(|i| i + 1);
                    for key in [style, "all"] {
                        rows.entry((label.clone(), key.to_string()))
                            .or_default()
                            .add(rank);
                    }
                }
            }
        }
    }
    let rows = rows.into_iter().map(|(k, m)| (k, m.finish())).collect();
    Ok(Report {
        targets: targets.len(),
        missing,
        rows,
    })
}

impl Report {
    pub fn markdown(&self, env_id: &str) -> String {
        let mut out = format!(
            "# MathlibQR baseline ({env_id})\n\n{} of {} targets found in this index; missing: {}.\n\n",
            self.targets,
            self.targets + self.missing.len(),
            if self.missing.is_empty() {
                "none".to_string()
            } else {
                self.missing.join(", ")
            },
        );
        out.push_str(
            "| Lanes | Style | Queries | R@1 | R@5 | R@10 | R@50 | MRR | nDCG@10 |\n\
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

    #[test]
    fn metrics_average_over_queries() {
        let mut m = Metrics::default();
        m.add(Some(1));
        m.add(Some(4));
        m.add(None);
        let m = m.finish();
        assert_eq!(m.queries, 3);
        assert!((m.r1 - 1.0 / 3.0).abs() < 1e-9);
        assert!((m.r5 - 2.0 / 3.0).abs() < 1e-9);
        assert!((m.mrr - (1.0 + 0.25) / 3.0).abs() < 1e-9);
    }
}
