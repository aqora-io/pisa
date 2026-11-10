//! Latency summaries, in milliseconds.

use std::future::Future;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub n: usize,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub mean: f64,
    pub max: f64,
    pub total: f64,
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Nearest-rank percentiles of `samples` (milliseconds).
pub fn summarize(samples: &[f64]) -> Summary {
    if samples.is_empty() {
        return Summary::default();
    }
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    let rank = |p: f64| s[(((p / 100.0) * s.len() as f64).ceil() as usize).clamp(1, s.len()) - 1];
    let total: f64 = s.iter().sum();
    Summary {
        n: s.len(),
        p50: rank(50.0),
        p90: rank(90.0),
        p99: rank(99.0),
        mean: total / s.len() as f64,
        max: s[s.len() - 1],
        total,
    }
}

pub async fn timed<T>(f: impl Future<Output = T>) -> (T, Duration) {
    let start = Instant::now();
    let out = f.await;
    (out, start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank() {
        let s: Vec<f64> = (1..=10).map(f64::from).collect();
        let sum = summarize(&s);
        assert_eq!(
            (sum.n, sum.p50, sum.p90, sum.p99, sum.max),
            (10, 5.0, 9.0, 10.0, 10.0)
        );
        assert_eq!(sum.mean, 5.5);
        assert_eq!(summarize(&[]).n, 0);
    }
}
