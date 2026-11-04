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
async fn module_system_code_is_checked_with_its_header() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let r = check(
        &c,
        json!({"code": "module\npublic import Foo\n\n@[expose] public section\n#check Nat"}),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(r["messages"][0]["pos"]["line"], 5, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn check_reports_tactics_only_when_asked() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let code = "import Foo\n\ntheorem t : True := by\n  trivial";
    let r = check(&c, json!({"code": code})).await;
    assert!(r.get("error").is_none(), "{r}");
    assert!(r.get("tactics").is_none(), "{r}");

    let tactics = json!([{"tactic": "trivial", "goals_before": ["⊢ goal 3"], "goals_after": [],
                          "pos": {"line": 4, "column": 2}, "end_pos": {"line": 4, "column": 9}}]);
    let r = check(&c, json!({"code": code, "detail": ["tactics"]})).await;
    assert_eq!(r["tactics"], tactics, "{r}");
    let (status, r) = c
        .post(
            "/check/batch",
            json!({"items": [{"id": "a", "code": code}], "detail": ["tactics"]}),
        )
        .await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["results"][0]["tactics"], tactics, "{r}");

    let (status, r) = c
        .post("/check", json!({"code": code, "detail": ["bogus"]}))
        .await;
    assert_eq!(status, 400, "{r}");
    assert_eq!(r["error"]["kind"], "bad_request", "{r}");
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

/// The router must learn that a worker is warm before the client sees the
/// result, or the client's next request may be routed elsewhere.
#[tokio::test(flavor = "multi_thread")]
async fn worker_reports_a_new_warm_header_before_the_result() {
    use pisa_protocol::header::split_header;
    use pisa_protocol::wire::{self, CheckJob, Job, RouterMessage, WorkerMessage};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cfg = common::worker_config(listener.local_addr().unwrap(), "w0", "v", 1);
    let store: std::sync::Arc<dyn object_store::ObjectStore> =
        std::sync::Arc::new(object_store::memory::InMemory::new());
    tokio::spawn(pisa_worker::run_with_stores(
        cfg,
        store,
        pisa_worker::db::Db::memory().await,
        std::future::pending(),
    ));
    let (stream, _) = listener.accept().await.unwrap();
    let mut conn = wire::connection(stream);
    async fn recv(conn: &mut wire::Connection<tokio::net::TcpStream>) -> WorkerMessage {
        wire::recv(conn).await.unwrap().unwrap()
    }
    assert!(matches!(recv(&mut conn).await, WorkerMessage::Hello { .. }));
    assert!(matches!(
        recv(&mut conn).await,
        WorkerMessage::Status { .. }
    ));
    for i in 0..8 {
        let code = format!("import H{i}\ntheorem t : 1 = 1 := rfl");
        let key = split_header(&code).header_key;
        let job = Job::Check(CheckJob {
            code,
            timeout_secs: 5,
            detail: Vec::new(),
        });
        let msg = RouterMessage::Job { request_id: i, job };
        wire::send(&mut conn, &msg).await.unwrap();
        let mut warm = false;
        loop {
            match recv(&mut conn).await {
                WorkerMessage::Status { warm_headers } => warm = warm_headers.contains(&key),
                WorkerMessage::Result { .. } => break,
                WorkerMessage::Restart { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(warm, "job {i}: result arrived before the warm status");
    }
}

/// During a rolling deploy a router of another release can send a job this
/// worker can't decode. It fails that job and keeps the connection, so the
/// router doesn't retry everything the worker is running elsewhere.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_does_not_decode_fails_alone() {
    use pisa_protocol::api::ErrorKind;
    use pisa_protocol::wire::{self, CheckJob, Job, JobResult, RouterMessage, WorkerMessage};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = common::worker_config(listener.local_addr().unwrap(), "w0", "v", 1);
    // Not the default, which the test above shares with any other "w0".
    let dir = tempfile::tempdir().unwrap();
    cfg.work_dir = Some(dir.path().into());
    let store: std::sync::Arc<dyn object_store::ObjectStore> =
        std::sync::Arc::new(object_store::memory::InMemory::new());
    tokio::spawn(pisa_worker::run_with_stores(
        cfg,
        store,
        pisa_worker::db::Db::memory().await,
        std::future::pending(),
    ));
    let (stream, _) = listener.accept().await.unwrap();
    let mut conn = wire::connection(stream);
    async fn result(conn: &mut wire::Connection<tokio::net::TcpStream>) -> (u64, JobResult) {
        loop {
            match wire::recv(conn).await.unwrap().unwrap() {
                WorkerMessage::Result { request_id, result } => return (request_id, result),
                WorkerMessage::Hello { .. }
                | WorkerMessage::Status { .. }
                | WorkerMessage::Restart { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    let bad = json!({"type": "job", "request_id": 1, "job": {"kind": "check", "code": 5}});
    wire::send(&mut conn, &bad).await.unwrap();
    let (id, r) = result(&mut conn).await;
    assert!(
        id == 1 && matches!(&r, JobResult::Failed { error } if error.contains("another release")),
        "{id} {r:?}"
    );

    // The release before entries named the entry `snapshot`.
    let old = json!({
        "type": "job", "request_id": 2,
        "job": {"kind": "snapshot_get", "snapshot": "PisaSnap.s0"}
    });
    wire::send(&mut conn, &old).await.unwrap();
    let (id, r) = result(&mut conn).await;
    assert!(
        id == 2 && matches!(&r, JobResult::Snapshot(Err(e)) if e.kind == ErrorKind::BadRequest),
        "{id} {r:?}"
    );

    let job = Job::Check(CheckJob {
        code: "theorem t : 1 = 1 := rfl".into(),
        timeout_secs: 5,
        detail: Vec::new(),
    });
    let msg = RouterMessage::Job { request_id: 3, job };
    wire::send(&mut conn, &msg).await.unwrap();
    let (id, r) = result(&mut conn).await;
    assert!(
        id == 3 && matches!(&r, JobResult::Check(o) if o.error.is_none()),
        "{id} {r:?}"
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

#[tokio::test(flavor = "multi_thread")]
async fn draining_worker_finishes_its_jobs_and_exits() {
    let mut c = Cluster::start(&[]).await;
    let addr = c.router.worker_addr;
    let stop = c.add_drainable_worker(common::worker_config(addr, "w0", "v", 2));
    c.wait_for_workers(1).await;

    let slow = {
        let client = c.client.clone();
        let url = c.url("/check");
        tokio::spawn(async move {
            let r = client
                .post(url)
                .json(&json!({"code": "SLEEP 1000\n#check Nat", "timeout": 10}))
                .send()
                .await
                .unwrap();
            r.json::<Value>().await.unwrap()
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    stop.send(()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // w0 still has a free slot but is draining, so this waits for w1.
    let later = {
        let client = c.client.clone();
        let url = c.url("/check");
        tokio::spawn(async move {
            let r = client
                .post(url)
                .json(&json!({"code": "#check Nat"}))
                .send()
                .await
                .unwrap();
            r.json::<Value>().await.unwrap()
        })
    };
    let slow = slow.await.unwrap();
    assert!(slow.get("error").is_none(), "{slow}");
    assert_eq!(slow["messages"][0]["data"], "Nat : Type");

    // w0 exits once its job is answered.
    let w0 = c.workers.remove(0);
    tokio::time::timeout(std::time::Duration::from_secs(5), w0)
        .await
        .expect("drained worker exits")
        .unwrap();
    assert!(!later.is_finished());
    c.add_worker(common::worker_config(addr, "w1", "v", 1));
    let later = later.await.unwrap();
    assert!(later.get("error").is_none(), "{later}");
    assert!(!c.metrics().await.contains("pisa_job_retries_total"));
}
