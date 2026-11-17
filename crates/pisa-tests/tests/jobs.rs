//! Async requests: with `Prefer: respond-async`, the long endpoints answer 202
//! with a job at once, which `/jobs/{id}` answers later as the request would
//! have.

mod common;

use std::time::{Duration, Instant};

use common::{Cluster, router_config, url_of};
use pisa_worker::queue::{NewJob, Poll};
use reqwest::header::HeaderMap;
use serde_json::{Value, json};

const CODE: &str = "theorem t : 1 = 1 := by\n  sorry";

/// POSTs `body` to `url` with `Prefer: respond-async`.
async fn submit(c: &Cluster, url: &str, body: Value) -> (u16, HeaderMap, Value) {
    let r = c
        .client
        .post(url)
        .header("prefer", "respond-async")
        .json(&body);
    let r = r.send().await.unwrap();
    let (status, headers) = (r.status().as_u16(), r.headers().clone());
    (status, headers, r.json().await.unwrap())
}

/// Submits to the cluster's server and checks the 202; returns the job.
async fn accepted(c: &Cluster, path: &str, body: Value, kind: &str) -> String {
    let (status, headers, r) = submit(c, &c.url(path), body).await;
    assert_eq!(status, 202, "{path}: {r}");
    let job = r["id"].as_str().unwrap().to_string();
    assert_eq!(r, json!({"id": job, "kind": kind, "status": "queued"}));
    assert_eq!(headers["location"], format!("/jobs/{job}").as_str());
    assert_eq!(headers["preference-applied"], "respond-async");
    job
}

/// `GET /jobs/{id}?wait=…` at `url` (a server's `/jobs/{id}`), as the
/// job's request would have answered once it is done.
async fn look(c: &Cluster, url: &str, wait: &str) -> (u16, Value) {
    let (status, r) = look_raw(c, url, wait).await;
    if status == 200 {
        let id = url.rsplit('/').next().unwrap();
        assert_eq!(
            (&r["id"], &r["status"]),
            (&json!(id), &json!("done")),
            "{r}"
        );
    }
    common::job_answer(status, r)
}

/// `GET /jobs/{id}?wait=…` at `url`, as it answers.
async fn look_raw(c: &Cluster, url: &str, wait: &str) -> (u16, Value) {
    let r = c
        .client
        .get(format!("{url}?wait={wait}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["cache-control"], "no-store");
    (r.status().as_u16(), r.json().await.unwrap())
}

async fn delete(c: &Cluster, url: &str) -> (u16, Value) {
    let r = c.client.delete(url).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

/// `v` without what differs between two runs of one request: times, whether
/// it was cached, and the ids of what each run made.
fn comparable(v: &Value) -> Value {
    match v {
        Value::Object(o) => o
            .iter()
            .filter(|(k, _)| !["time", "timing", "cached", "id", "created"].contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), comparable(v)))
            .collect(),
        Value::Array(a) => a.iter().map(comparable).collect(),
        v => v.clone(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_long_endpoint_answers_later_as_it_would_have_at_once() {
    let c = Cluster::start_beam(&[("w0", "v", 2)]).await;
    let (_, session) = c.post("/sessions", json!({"code": CODE})).await;
    let state = session["states"][0]["id"].as_str().unwrap();
    let session_id = session["id"].as_str().unwrap();
    // Verifying records what it checked, so each run gets a snapshot of its
    // own, on a module of its own.
    let mut snapshots = Vec::new();
    for code in ["def kept := 1", "def kept' := 1"] {
        let body = json!({"code": code, "verify": false});
        let (_, kept) = c.post("/snapshots", body).await;
        snapshots.push(kept["snapshot"]["id"].as_str().unwrap().to_string());
    }
    let cases = [
        ("/check", json!({"code": "#check Nat"}), "check"),
        ("/check", json!({"code": "error_here"}), "check"),
        ("/sessions", json!({"code": CODE}), "start_session"),
        (
            &*format!("/states/{state}/tactic"),
            json!({"tactic": "rfl"}),
            "apply_tactic",
        ),
        (
            &*format!("/states/{state}/tactic"),
            json!({"tactic": "fail"}),
            "apply_tactic",
        ),
        (
            &*format!("/sessions/{session_id}/check"),
            json!({"code": "theorem t : 1 = 1 := by\n  rfl"}),
            "check_session",
        ),
        (
            "/snapshots",
            json!({"code": "def made := 2"}),
            "create_snapshot",
        ),
    ];
    for (path, body, kind) in cases {
        let (status, at_once) = c.post(path, body.clone()).await;
        let job = accepted(&c, path, body, kind).await;
        let (later_status, later) = look(&c, &c.url(&format!("/jobs/{job}")), "10").await;
        assert_eq!(
            (later_status, comparable(&later)),
            (status, comparable(&at_once)),
            "{path}"
        );
    }

    let verify = |id: &str| format!("/snapshots/{id}/verify");
    let (status, at_once) = c.post(&verify(&snapshots[0]), json!({})).await;
    let job = accepted(&c, &verify(&snapshots[1]), json!({}), "verify_snapshot").await;
    let (later_status, later) = look(&c, &c.url(&format!("/jobs/{job}")), "10").await;
    let shape = |v: &Value| {
        (
            v["snapshot"]["verified"].clone(),
            v["snapshot"]["status"].clone(),
            v["checked"].as_array().map(Vec::len),
        )
    };
    assert_eq!((later_status, shape(&later)), (status, shape(&at_once)));
    assert_eq!(later["checked"][0], later["snapshot"]["module"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_returns_as_soon_as_the_job_is_done() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let job = accepted(
        &c,
        "/check",
        json!({"code": "SLEEP 800\n#check Nat"}),
        "check",
    )
    .await;
    let url = c.url(&format!("/jobs/{job}"));
    let (status, r) = look(&c, &url, "0").await;
    assert_eq!(status, 202);
    assert!(r["status"] == "queued" || r["status"] == "running", "{r}");

    // Two waits on one server both return, once the job is done.
    let started = Instant::now();
    let (a, b) = tokio::join!(look(&c, &url, "30"), look(&c, &url, "30"));
    assert_eq!((a.0, b.0), (200, 200));
    assert_eq!(a.1["ok"], true);
    assert!(started.elapsed() < Duration::from_secs(5));
    // The answer stays, in its envelope: the job, its request's status and
    // answer.
    let (status, r) = look_raw(&c, &url, "0").await;
    assert_eq!(status, 200);
    assert_eq!(
        (&r["id"], &r["kind"], &r["status"], &r["http_status"]),
        (&json!(job), &json!("check"), &json!("done"), &json!(200)),
        "{r}"
    );
    assert_eq!(r["body"]["ok"], true, "{r}");
}

/// The longest a `wait` of one second may take while the database stalls: the
/// wait, the margin, and slack.
const STALLED_BOUND: Duration = Duration::from_secs(1 + 5 + 1);

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_answers_within_its_bound_while_the_database_stalls() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let job = accepted(
        &c,
        "/check",
        json!({"code": "SLEEP 5000\n#check Nat"}),
        "check",
    )
    .await;
    let url = c.url(&format!("/jobs/{job}"));
    // Wait for it to run, so that the look reads `running`.
    for _ in 0..100 {
        let (_, r) = look(&c, &url, "0").await;
        if r["status"] == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The database stalls after the wait's first reads, before its last.
    let started = Instant::now();
    let waiting = look_raw(&c, &url, "1");
    let stalling = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c.db.stall().await
    };
    let ((status, r), _stalled) = tokio::join!(waiting, stalling);
    assert!(started.elapsed() < STALLED_BOUND, "{:?}", started.elapsed());
    assert_eq!((status, &r["status"]), (202, &json!("running")), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_with_no_status_read_answers_503() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let job = accepted(
        &c,
        "/check",
        json!({"code": "SLEEP 5000\n#check Nat"}),
        "check",
    )
    .await;
    let url = c.url(&format!("/jobs/{job}"));
    let _stalled = c.db.stall().await;
    let started = Instant::now();
    let r = c.client.get(format!("{url}?wait=1")).send().await.unwrap();
    assert!(started.elapsed() < STALLED_BOUND, "{:?}", started.elapsed());
    assert_eq!(r.status(), 503);
    assert!(r.headers().contains_key("retry-after"));
}

#[tokio::test(flavor = "multi_thread")]
async fn failures_are_kept_and_requests_checked_first() {
    let mut cfg = router_config();
    cfg.max_wait_secs = 1;
    let c = Cluster::start_with(cfg, &[("w0", "v", 1)], common::with_beam).await;

    // A malformed key is the request's own fault, at once.
    let (status, _, r) = submit(&c, &c.url("/states/nope/tactic"), json!({"tactic": "rfl"})).await;
    assert_eq!((status, &r["error"]["kind"]), (400, &json!("bad_request")));

    // No worker of this version takes it in time: 429, however often asked,
    // and asked at once it says when to try again.
    let body = json!({"code": "#check Nat", "version": "nope"});
    let r = c.client.post(c.url("/check")).json(&body).send().await;
    let r = r.unwrap();
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers()["retry-after"], "2");
    let job = accepted(&c, "/check", body, "check").await;
    let url = c.url(&format!("/jobs/{job}"));
    for wait in ["10", "0"] {
        let (status, r) = look(&c, &url, wait).await;
        assert_eq!((status, &r["error"]["kind"]), (429, &json!("capacity")));
    }
    // The look itself succeeded: the 429 is the job's, in its envelope.
    let (status, r) = look_raw(&c, &url, "0").await;
    assert_eq!(status, 200);
    assert_eq!(
        (&r["kind"], &r["http_status"], &r["body"]["error"]["kind"]),
        (&json!("check"), &json!(429), &json!("capacity")),
        "{r}"
    );

    // A state nobody has: the request's 404.
    let (_, session) = c.post("/sessions", json!({"code": CODE})).await;
    let state = session["states"][0]["id"].as_str().unwrap();
    let unknown = format!("{}{}", &state[..state.len() - 4], "0000");
    let path = format!("/states/{unknown}/tactic");
    let job = accepted(&c, &path, json!({"tactic": "rfl"}), "apply_tactic").await;
    let (status, r) = look(&c, &c.url(&format!("/jobs/{job}")), "10").await;
    assert_eq!(
        (status, &r["error"]["kind"]),
        (404, &json!("not_found")),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn any_server_answers_for_a_job() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let other = c.start_server().await;
    let job = accepted(
        &c,
        "/check",
        json!({"code": "SLEEP 300\n#check Nat"}),
        "check",
    )
    .await;
    let there = url_of(&other, &format!("/jobs/{job}"));
    let (status, r) = look(&c, &there, "10").await;
    assert_eq!((status, &r["ok"]), (200, &json!(true)));
    let (status, r) = delete(&c, &there).await;
    assert_eq!(status, 200);
    assert_eq!(r, json!({"id": job, "kind": "check", "status": "done"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_withdraws_only_what_no_worker_holds() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let running = accepted(
        &c,
        "/check",
        json!({"code": "SLEEP 1500\n#check Nat"}),
        "check",
    )
    .await;
    let running_url = c.url(&format!("/jobs/{running}"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while look(&c, &running_url, "0").await.1["status"] != "running" {
        assert!(Instant::now() < deadline, "never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The only slot is busy, so this one waits.
    let queued = accepted(&c, "/check", json!({"code": "#check Nat"}), "check").await;
    let queued_url = c.url(&format!("/jobs/{queued}"));

    let (status, r) = delete(&c, &queued_url).await;
    assert_eq!((status, &r["status"]), (200, &json!("cancelled")));
    let (status, r) = look(&c, &queued_url, "0").await;
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));

    let (status, r) = delete(&c, &running_url).await;
    assert_eq!((status, &r["status"]), (200, &json!("running")));
    let (status, r) = look(&c, &running_url, "30").await;
    assert_eq!((status, &r["ok"]), (200, &json!(true)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_worker_is_lost_says_so() {
    let mut cfg = router_config();
    cfg.max_retries = 0;
    let mut c = Cluster::start_with(cfg, &[("w0", "v", 1)], |w| w).await;
    let job = accepted(
        &c,
        "/check",
        json!({"code": "SLEEP 5000\n#check Nat"}),
        "check",
    )
    .await;
    let url = c.url(&format!("/jobs/{job}"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while look(&c, &url, "0").await.1["status"] != "running" {
        assert!(Instant::now() < deadline, "never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    c.kill_worker(0).await;
    for wait in ["30", "0"] {
        let (status, r) = look(&c, &url, wait).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (503, &json!("unavailable")),
            "{r}"
        );
    }
}

/// Its worker is lost and no other is live for longer than its max wait, as
/// across a reboot: it runs when a worker comes back, as a new one or under
/// the lost one's name.
#[tokio::test(flavor = "multi_thread")]
async fn an_async_job_runs_again_when_a_worker_comes_back() {
    let mut cfg = router_config();
    cfg.max_wait_secs = 2;
    let mut c = Cluster::start_with(cfg, &[("w0", "v", 1)], |w| w).await;
    for (lost, back) in [(0, "w1"), (1, "w1")] {
        // Longer than the wait below: a lost worker's run would finish it.
        let job = accepted(
            &c,
            "/check",
            json!({"code": "SLEEP 8000\n#check Nat"}),
            "check",
        )
        .await;
        let url = c.url(&format!("/jobs/{job}"));
        let deadline = Instant::now() + Duration::from_secs(10);
        while look(&c, &url, "0").await.1["status"] != "running" {
            assert!(Instant::now() < deadline, "never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if lost == 0 {
            c.kill_worker(lost).await;
        } else {
            // Dead as in a reboot: its row stays, until it registers again.
            c.workers[lost].abort();
            let _ = (&mut c.workers[lost]).await;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (status, r) = look(&c, &url, "0").await;
        assert_eq!((status, &r["status"]), (202, &json!("queued")), "{r}");
        c.add_worker(common::worker_config(back, "v", 1));
        let (status, r) = look(&c, &url, "30").await;
        assert_eq!((status, &r["ok"]), (200, &json!(true)), "{r}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn other_endpoints_answer_at_once() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let body = json!({"items": [{"code": "#check Nat"}]});
    let (status, headers, r) = submit(&c, &c.url("/check/batch"), body).await;
    assert_eq!(status, 200, "{r}");
    assert!(headers.get("preference-applied").is_none());
    assert_eq!(r["results"][0]["ok"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_jobs_and_bad_waits_are_errors() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for job in ["nope", &"0".repeat(32)] {
        let (status, r) = look(&c, &c.url(&format!("/jobs/{job}")), "0").await;
        assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));
        let (status, _) = delete(&c, &c.url(&format!("/jobs/{job}"))).await;
        assert_eq!(status, 404);
    }
    let job = accepted(&c, "/check", json!({"code": "#check Nat"}), "check").await;
    let (status, r) = look(&c, &c.url(&format!("/jobs/{job}")), "soon").await;
    assert_eq!((status, &r["error"]["kind"]), (400, &json!("bad_request")));
}

/// A create that its client gave no key gets one of its own: when its worker
/// is lost after keeping the snapshot, the job's retry returns that snapshot
/// rather than adding another.
#[tokio::test(flavor = "multi_thread")]
async fn a_retried_create_keeps_its_snapshot_once() {
    let mut c = Cluster::start(&[]).await;
    let code = "import Foo\n\ntheorem t : 1 = 1 := rfl";
    let job = accepted(&c, "/snapshots", json!({"code": code}), "create_snapshot").await;
    // A worker claims it, keeps the snapshot and is lost before it answers:
    // here a copy of the job does what its attempt did.
    let version = c.version("v");
    let candidates = c.queue.candidates(&version, 10).await.unwrap();
    let ids: Vec<i64> = candidates.iter().map(|j| j.id).collect();
    let lease = Duration::from_millis(500);
    let claimed = c.queue.claim("lost", &ids, lease).await.unwrap();
    let first = claimed[0].job.clone();
    assert!(
        first["key"].as_str().unwrap().starts_with("job:"),
        "{first}"
    );
    let copy = NewJob {
        version: Some(version),
        priority: 1,
        header_key: None,
        header: None,
        document: None,
        job: first,
        max_wait: Duration::from_secs(10),
        max_attempts: 1,
    };
    let copy = c.queue.enqueue(&copy).await.unwrap();
    c.add_worker(common::worker_config("w0", "v", 1));
    let deadline = Instant::now() + Duration::from_secs(10);
    let kept = loop {
        if let Poll::Done(r) = c.queue.poll(copy).await.unwrap() {
            break r;
        }
        assert!(Instant::now() < deadline, "the copy never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(kept["snapshot"]["id"].is_string(), "{kept}");

    // Once the lost worker's lease lapses, the job runs again.
    let (status, r) = look(&c, &c.url(&format!("/jobs/{job}")), "30").await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["snapshot"]["id"], kept["snapshot"]["id"], "{r}");
    let listed = c.client.get(c.url("/snapshots")).send().await.unwrap();
    let listed: Value = listed.json().await.unwrap();
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), 1, "{listed}");

    // The server's keys are its own.
    let body = json!({"code": code, "key": "job:mine"});
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(
        (status, &r["error"]["kind"]),
        (400, &json!("bad_request")),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_waited_for_the_worker_says_how_long() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let running = accepted(
        &c,
        "/check",
        json!({"code": "SLEEP 800\n#check Nat"}),
        "check",
    )
    .await;
    let running_url = c.url(&format!("/jobs/{running}"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while look(&c, &running_url, "0").await.1["status"] != "running" {
        assert!(Instant::now() < deadline, "never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The only slot is busy, so this one waits for it.
    let queued = accepted(&c, "/check", json!({"code": "#check Nat"}), "check").await;
    let (status, waited) = look(&c, &c.url(&format!("/jobs/{queued}")), "30").await;
    assert_eq!(status, 200, "{waited}");
    let timing = &waited["timing"];
    assert!(timing["queue"].as_f64().unwrap() > 0.2, "{waited}");
    assert!(timing["total"].as_f64().unwrap() > 0.0, "{waited}");
    let (_, first) = look(&c, &running_url, "30").await;
    assert!(first["timing"]["queue"].as_f64().unwrap() < 0.5, "{first}");
    assert!(
        first["timing"]["execution"].as_f64().unwrap() >= 0.8,
        "{first}"
    );
}
