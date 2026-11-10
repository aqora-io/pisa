//! Stage 1: router + REPL workers, the check API, warm headers.

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
                "end_pos": {"line": 3, "column": 6}, "text": "Nat : Type"}])
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
            json!({"items": [{"code": code}], "detail": ["tactics"]}),
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
async fn check_answers_positions_in_the_submitted_code() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    // The fake's goals are named by the body's lines, one less than these.
    let code = "import Foo\n\ntheorem t : True := by\n  exact foo\n  trivial";
    let pos = |line, column| json!({"line": line, "column": column});
    let at = json!([pos(4, 9), pos(4, 2), pos(5, 3), pos(1, 3), pos(3, 0)]);
    let exact = json!({"tactic": "exact foo", "goals_before": ["⊢ goal 3"], "goals_after": [],
                       "pos": pos(4, 2), "end_pos": pos(4, 11)});
    let trivial = json!({"tactic": "trivial", "goals_before": ["⊢ goal 4"], "goals_after": [],
                         "pos": pos(5, 2), "end_pos": pos(5, 9)});
    let goals = json!([
        {"pos": pos(4, 9), "tactic": exact},
        {"pos": pos(4, 2), "tactic": exact},
        {"pos": pos(5, 3), "tactic": trivial},
        // In the header, and before the proof.
        {"pos": pos(1, 3)},
        {"pos": pos(3, 0)},
    ]);
    let r = check(&c, json!({"code": code, "at": at})).await;
    assert_eq!(r["at"], goals, "{r}");
    assert!(r.get("tactics").is_none(), "{r}");
    // Code without tactics has no tree, and still an entry per position.
    let r = check(
        &c,
        json!({"code": "theorem u : True := trivial", "at": [pos(1, 22)]}),
    )
    .await;
    assert_eq!(r["at"], json!([{"pos": pos(1, 22)}]), "{r}");

    // From the full tree: `foo` with what it was expected to be, and the
    // same tactics. Its synthetic tactic on `exact`'s `e` is left out.
    let mut terms = goals.clone();
    terms[0]["term"] = json!({"term": "foo", "expected_type": "goal 3", "expr": "foo",
                              "pos": pos(4, 8), "end_pos": pos(4, 11)});
    let r = check(
        &c,
        json!({"code": code, "at": at, "detail": ["tactics", "terms"]}),
    )
    .await;
    assert_eq!(r["at"], terms, "{r}");
    assert_eq!(r["tactics"], json!([exact, trivial]), "{r}");

    // A batch's positions apply to each item, in its own lines.
    let items = json!([{"code": code}, {"code": "theorem u : True := by\n  trivial"}]);
    let (status, r) = c
        .post("/check/batch", json!({"items": items, "at": [pos(2, 2)]}))
        .await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["results"][0]["at"], json!([{"pos": pos(2, 2)}]), "{r}");
    assert_eq!(
        r["results"][1]["at"][0]["tactic"]["goals_before"],
        json!(["⊢ goal 2"]),
        "{r}"
    );

    let many = vec![pos(4, 9); 65];
    for (path, body, says) in [
        (
            "/check",
            json!({"code": code, "detail": ["terms"]}),
            "needs positions",
        ),
        (
            "/check",
            json!({"code": code, "at": many}),
            "the most is 64",
        ),
        (
            "/check/batch",
            json!({"items": items, "at": many}),
            "the most is 64",
        ),
    ] {
        let (status, r) = c.post(path, body).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{r}"
        );
        assert!(
            r["error"]["message"].as_str().unwrap().contains(says),
            "{r}"
        );
    }
}

/// A worker's one slot keeps the header it imported, and says so.
#[tokio::test(flavor = "multi_thread")]
async fn a_header_stays_warm_across_checks() {
    use pisa_protocol::header::split_header;
    use pisa_worker::queue::LIVE;

    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let code = |i| format!("import Foo\ntheorem t{i} : True := trivial");
    for i in 0..3 {
        let r = check(&c, json!({"code": code(i)})).await;
        assert!(r.get("error").is_none(), "{r}");
        let peers = c.queue.registry(&c.version("v"), LIVE).await.unwrap();
        assert_eq!(peers[0].warm, [split_header(&code(i)).header_key]);
    }
}

/// A worker of version "v" alone on a queue of its own, with the queue.
async fn lone_worker() -> (pisa_worker::queue::Queue, tempfile::TempDir) {
    use pisa_worker::queue::{LIVE, Queue};
    let db = pisa_worker::db::Db::memory().await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = common::worker_config("w0", "v", 1);
    cfg.work_dir = Some(dir.path().into());
    let store: std::sync::Arc<dyn object_store::ObjectStore> =
        std::sync::Arc::new(object_store::memory::InMemory::new());
    tokio::spawn(pisa_worker::run_with_stores(
        cfg,
        store,
        db.clone(),
        std::future::pending(),
    ));
    let queue = Queue::new(db, "pisa");
    while queue.counts(LIVE).await.unwrap().workers == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    (queue, dir)
}

/// Queues `job` as it is, as a server would, and waits for its result.
async fn run_raw(queue: &pisa_worker::queue::Queue, job: Value) -> Value {
    use pisa_worker::queue::{NewJob, Poll};
    let id = queue
        .enqueue(&NewJob {
            version: Some("v".into()),
            priority: 1,
            header_key: None,
            header: None,
            state: None,
            document: None,
            job,
            max_wait: std::time::Duration::from_secs(10),
            max_attempts: 1,
        })
        .await
        .unwrap();
    loop {
        match queue.poll(id).await.unwrap() {
            Poll::Done(result) => return result,
            Poll::Waiting { .. } => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            other => panic!("{other:?}"),
        }
    }
}

/// The registry must learn that a worker is warm before the client sees the
/// result, or the client's next request may go to another worker.
#[tokio::test(flavor = "multi_thread")]
async fn worker_publishes_a_new_warm_header_before_the_result() {
    use pisa_protocol::header::split_header;
    use pisa_protocol::wire::{CheckJob, Job};
    use pisa_worker::queue::LIVE;

    let (queue, _dir) = lone_worker().await;
    for i in 0..8 {
        let code = format!("import H{i}\ntheorem t : 1 = 1 := rfl");
        let key = split_header(&code).header_key;
        let job = Job::Check(CheckJob {
            code,
            timeout_secs: 5,
            detail: Vec::new(),
            at: Vec::new(),
        });
        run_raw(&queue, serde_json::to_value(job).unwrap()).await;
        let warm = &queue.registry("v", LIVE).await.unwrap()[0].warm;
        assert!(
            warm.contains(&key),
            "job {i}: result before the warm header"
        );
    }
}

/// During a rolling deploy a server of another release can queue a job this
/// worker can't decode. That job fails, and the worker carries on.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_does_not_decode_fails_alone() {
    use pisa_protocol::wire::{CheckJob, Job, JobResult};

    let (queue, _dir) = lone_worker().await;
    let result = async |job: Value| -> JobResult {
        serde_json::from_value(run_raw(&queue, job).await).unwrap()
    };

    let r = result(json!({"kind": "check", "code": 5})).await;
    assert!(
        matches!(&r, JobResult::Failed { error } if error.contains("another release")),
        "{r:?}"
    );

    let job = Job::Check(CheckJob {
        code: "theorem t : 1 = 1 := rfl".into(),
        timeout_secs: 5,
        detail: Vec::new(),
        at: Vec::new(),
    });
    let r = result(serde_json::to_value(job).unwrap()).await;
    assert!(
        matches!(&r, JobResult::Check(o) if o.error.is_none()),
        "{r:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_results_keep_order() {
    let c = Cluster::start(&[("w0", "v", 2), ("w1", "v", 2)]).await;
    let items = json!([{"code": "import A0\n#check Nat"}, {"code": "import A1\nerror_here"}]);
    let (status, body) = c.post("/check/batch", json!({"items": items})).await;
    assert_eq!(status, 200);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for (r, severity) in results.iter().zip(["info", "error"]) {
        assert_eq!(r["messages"][0]["severity"], severity);
        assert_eq!(r["messages"][0]["pos"]["line"], 2);
        assert!(r.get("id").is_none(), "{r}");
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
            json!({"version": "v2", "items": [{"code": "#check Nat"}]}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["results"][0]["error"]["kind"], "capacity");
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
    let stop = c.add_drainable_worker(common::worker_config("w0", "v", 2));
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
    assert_eq!(slow["messages"][0]["text"], "Nat : Type");

    // w0 exits once its job is answered.
    let w0 = c.workers.remove(0);
    tokio::time::timeout(std::time::Duration::from_secs(5), w0)
        .await
        .expect("drained worker exits")
        .unwrap();
    assert!(!later.is_finished());
    c.add_worker(common::worker_config("w1", "v", 1));
    let later = later.await.unwrap();
    assert!(later.get("error").is_none(), "{later}");
}
