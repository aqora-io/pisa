//! Prometheus metrics. One global recorder per process.

use std::sync::OnceLock;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

pub const QUEUE_DEPTH: &str = "pisa_queue_depth";
pub const QUEUE_WAIT: &str = "pisa_queue_wait_seconds";
pub const HEADER_CACHE: &str = "pisa_header_cache_total";
pub const SLOTS: &str = "pisa_slots";
pub const WORKERS: &str = "pisa_workers";
pub const RETRIES: &str = "pisa_job_retries_total";
pub const EXPIRED: &str = "pisa_jobs_expired_total";
pub const JOBS: &str = "pisa_jobs_total";
pub const RESTARTS: &str = "pisa_repl_restarts_total";
pub const REJECTIONS: &str = "pisa_verification_rejections_total";

pub fn handle() -> &'static PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full(QUEUE_WAIT.into()),
                &[0.001, 0.005, 0.025, 0.1, 0.5, 1.0, 5.0, 15.0, 60.0, 300.0],
            )
            .expect("valid buckets")
            .install_recorder()
            .expect("metrics recorder installed once")
    })
}
