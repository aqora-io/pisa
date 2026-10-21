//! Stage 1: router + REPL workers, Kimina-compatible API, warm headers, metrics.

mod common;

use common::Cluster;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn api_check_matches_kimina_shape_and_positions() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let (status, body) = c
        .post(
            "/api/check",
            json!({"snippets": [{"id": "a", "code": "import Mathlib\n\n#check Nat"}]}),
        )
        .await;
    assert_eq!(status, 200);
    let r = &body["results"][0];
    assert_eq!(r["id"], "a");
    assert!(r.get("error").is_none(), "{body}");
    assert_eq!(
        r["response"]["messages"],
        json!([{"severity": "info", "pos": {"line": 3, "column": 0},
                "endPos": {"line": 3, "column": 6}, "data": "Nat : Type"}])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn second_request_with_same_header_is_a_warm_hit() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for i in 0..3 {
        let code = format!("import Foo\ntheorem t{i} : True := trivial");
        let (_, body) = c
            .post(
                "/api/check",
                json!({"snippets": [{"id": "x", "code": code}]}),
            )
            .await;
        assert!(body["results"][0].get("error").is_none(), "{body}");
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
async fn batch_results_keep_order_and_errors_are_per_item() {
    let c = Cluster::start(&[("w0", "v", 2), ("w1", "v", 2)]).await;
    let snippets: Vec<_> = (0..6)
        .map(|i| {
            let body = if i % 2 == 0 {
                "#check Nat"
            } else {
                "error_here"
            };
            json!({"id": format!("s{i}"), "code": format!("import A{}\n{body}", i % 3)})
        })
        .collect();
    let (status, body) = c.post("/api/check", json!({"snippets": snippets})).await;
    assert_eq!(status, 200);
    for (i, r) in body["results"].as_array().unwrap().iter().enumerate() {
        assert_eq!(r["id"], format!("s{i}"));
        let severity = &r["response"]["messages"][0]["severity"];
        assert_eq!(severity, if i % 2 == 0 { "info" } else { "error" });
        assert_eq!(r["response"]["messages"][0]["pos"]["line"], 2);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_reports_kimina_error_and_replaces_the_process() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (_, body) = c
        .post(
            "/api/check",
            json!({"timeout": 1, "snippets": [{"id": "slow", "code": "SLEEP 5000"}]}),
        )
        .await;
    let r = &body["results"][0];
    assert_eq!(r["error"], "Lean REPL command timed out in 1 seconds");
    assert_eq!(r["time"], 1.0);

    let (_, body) = c
        .post(
            "/api/check",
            json!({"snippets": [{"id": "ok", "code": "#check Nat"}]}),
        )
        .await;
    assert!(body["results"][0].get("error").is_none(), "{body}");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        c.metrics()
            .await
            .contains("pisa_repl_restarts_total{cause=\"timeout\"}")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn header_errors_and_crashes_are_reported() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (_, body) = c
        .post(
            "/api/check",
            json!({"snippets": [
                {"id": "missing", "code": "import Missing\n#check Nat"},
                {"id": "crash", "code": "CRASH"},
                {"id": "after", "code": "#check Nat"},
            ]}),
        )
        .await;
    let results = body["results"].as_array().unwrap();
    assert_eq!(results[0]["response"]["messages"][0]["severity"], "error");
    assert!(
        results[1]["error"]
            .as_str()
            .unwrap()
            .contains("Lean REPL failed"),
        "{body}"
    );
    assert!(results[2].get("error").is_none(), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_verify_endpoint() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (status, body) = c
        .post(
            "/verify",
            json!({"codes": [{"custom_id": 1, "proof": "#check Nat"}], "infotree_type": null}),
        )
        .await;
    assert_eq!(status, 200);
    let r = &body["results"][0];
    assert_eq!(r["custom_id"], "1");
    assert!(r["response"]["time"].is_number(), "{body}");
    assert_eq!(r["response"]["messages"][0]["data"], "Nat : Type");
}

#[tokio::test(flavor = "multi_thread")]
async fn versions_route_to_matching_pools() {
    let mut cfg = common::router_config();
    cfg.default_version = Some("mathlib".into());
    cfg.alias = vec![("mathlib".into(), "v1".into())];
    cfg.max_wait_secs = 1;
    let c = Cluster::start_with(cfg, &[("w0", "v1", 1)], |w| w).await;
    let (status, _) = c
        .post(
            "/api/check",
            json!({"snippets": [{"id": "a", "code": "#check Nat"}]}),
        )
        .await;
    assert_eq!(status, 200);
    // Nobody serves v2, so the job waits out max_wait and the request gets 429.
    let (status, body) = c
        .post(
            "/api/check",
            json!({"version": "v2", "snippets": [{"id": "a", "code": "#check Nat"}]}),
        )
        .await;
    assert_eq!(status, 429);
    assert_eq!(body["detail"], "No available REPLs");
}
