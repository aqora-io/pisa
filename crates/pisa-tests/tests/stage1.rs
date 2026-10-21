//! Stage 1: router + REPL workers, the check API, warm headers, metrics.

mod common;

use common::Cluster;
use serde_json::{Value, json};

async fn check(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/check", body).await;
    assert_eq!(status, 200, "{r}");
    r
}

#[tokio::test(flavor = "multi_thread")]
async fn check_reports_messages_at_submitted_positions() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let r = check(&c, json!({"code": "import Mathlib\n\n#check Nat"})).await;
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(
        r["messages"],
        json!([{"severity": "info", "pos": {"line": 3, "column": 0},
                "end_pos": {"line": 3, "column": 6}, "data": "Nat : Type"}])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn second_request_with_same_header_is_a_warm_hit() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for i in 0..3 {
        let code = format!("import Foo\ntheorem t{i} : True := trivial");
        let r = check(&c, json!({"code": code})).await;
        assert!(r.get("error").is_none(), "{r}");
    }
    let metrics = c.metrics().await;
    assert!(
        metrics.contains("pisa_header_cache_total{result=\"hit\"}"),
        "{metrics}"
    );
    assert!(
        metrics.contains("pisa_header_cache_total{result=\"miss\"}"),
        "{metrics}"
    );
    assert!(
        metrics.contains("pisa_queue_wait_seconds_bucket"),
        "{metrics}"
    );
    assert!(
        metrics.contains("pisa_queue_depth{priority=\"batch\"}"),
        "{metrics}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_results_keep_order() {
    let c = Cluster::start(&[("w0", "v", 2), ("w1", "v", 2)]).await;
    let items: Vec<_> = (0..6)
        .map(|i| {
            let body = if i % 2 == 0 {
                "#check Nat"
            } else {
                "error_here"
            };
            json!({"id": format!("s{i}"), "code": format!("import A{}\n{body}", i % 3)})
        })
        .collect();
    let (status, body) = c.post("/check/batch", json!({"items": items})).await;
    assert_eq!(status, 200);
    for (i, r) in body["results"].as_array().unwrap().iter().enumerate() {
        assert_eq!(r["id"], format!("s{i}"));
        let severity = &r["messages"][0]["severity"];
        assert_eq!(severity, if i % 2 == 0 { "info" } else { "error" });
        assert_eq!(r["messages"][0]["pos"]["line"], 2);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_is_in_band_and_replaces_the_process() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let r = check(&c, json!({"timeout": 1, "code": "SLEEP 5000"})).await;
    assert_eq!(r["ok"], false);
    assert_eq!(
        r["error"],
        json!({"kind": "timeout", "message": "Lean ran for more than 1s"})
    );
    assert_eq!(r["time"], 1.0);

    let r = check(&c, json!({"code": "#check Nat"})).await;
    assert!(r.get("error").is_none(), "{r}");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        c.metrics()
            .await
            .contains("pisa_repl_restarts_total{cause=\"timeout\"}")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn header_errors_and_crashes_are_in_band() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let r = check(&c, json!({"code": "import Missing\n#check Nat"})).await;
    assert_eq!(r["ok"], false);
    assert_eq!(r["messages"][0]["severity"], "error");

    let r = check(&c, json!({"code": "CRASH"})).await;
    assert_eq!(r["error"]["kind"], "crash", "{r}");

    let r = check(&c, json!({"code": "#check Nat"})).await;
    assert!(r.get("error").is_none(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn versions_route_to_matching_pools() {
    let mut cfg = common::router_config();
    cfg.default_version = Some("mathlib".into());
    cfg.alias = vec![("mathlib".into(), "v1".into())];
    cfg.max_wait_secs = 1;
    let c = Cluster::start_with(cfg, &[("w0", "v1", 1)], |w| w).await;
    check(&c, json!({"code": "#check Nat"})).await;

    // Nobody serves v2: a single check is a 429, a batch item says so in-band.
    let (status, body) = c
        .post("/check", json!({"version": "v2", "code": "#check Nat"}))
        .await;
    assert_eq!(status, 429);
    assert_eq!(body["error"]["kind"], "capacity");
    let (status, body) = c
        .post(
            "/check/batch",
            json!({"version": "v2", "items": [{"id": "a", "code": "#check Nat"}]}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["results"][0]["error"]["kind"], "capacity");
    assert_eq!(body["results"][0]["id"], "a");
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_requests_use_the_error_shape() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (status, body) = c.post("/check", json!({"source": "x"})).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["kind"], "bad_request");
    let (status, body) = c.post("/nope", json!({})).await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["kind"], "not_found");
}
