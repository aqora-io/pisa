//! Async requests: with `Prefer: respond-async`, the long endpoints answer 202
//! with a job at once, which `/jobs/{id}` answers later as the request would
//! have.

mod common;

use std::time::{Duration, Instant};

use common::{Cluster, router_config, url_of};
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

/// `GET /jobs/{id}?wait=…` at `url` (a server's `/jobs/{id}`).
async fn look(c: &Cluster, url: &str, wait: &str) -> (u16, Value) {
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

/// `v` without what differs between two runs of one request: times, and
/// the ids of what each run made.
fn comparable(v: &Value) -> Value {
    match v {
        Value::Object(o) => o
            .iter()
            .filter(|(k, _)| !["time", "entry", "id", "created"].contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), comparable(v)))
            .collect(),
        Value::Array(a) => a.iter().map(comparable).collect(),
        v => v.clone(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_long_endpoint_answers_later_as_it_would_have_at_once() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let (_, session) = c.post("/sessions", json!({"code": CODE})).await;
    let state = session["states"][0]["id"].as_str().unwrap();
    let session_id = session["id"].as_str().unwrap();
    // Verifying records what it checked, so each run gets an entry of its
    // own, on a module of its own.
    let mut entries = Vec::new();
    for code in ["def kept := 1", "def kept' := 1"] {
        let (_, kept) = c.post("/snapshots", json!({"code": code})).await;
        entries.push(kept["entry"].as_str().unwrap().to_string());
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

    let verify = |entry: &str| format!("/snapshots/{entry}/verify");
    let (status, at_once) = c.post(&verify(&entries[0]), json!({})).await;
    let job = accepted(&c, &verify(&entries[1]), json!({}), "verify_snapshot").await;
    let (later_status, later) = look(&c, &c.url(&format!("/jobs/{job}")), "10").await;
    let shape = |v: &Value| {
        (
            v["verified"].clone(),
            v["dirty"].clone(),
            v["checked"].as_array().map(Vec::len),
        )
    };
    assert_eq!((later_status, shape(&later)), (status, shape(&at_once)));
    assert_eq!(later["checked"][0], later["snapshot"]);
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
    // The answer stays.
    assert_eq!(look(&c, &url, "0").await.0, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn failures_are_kept_and_requests_checked_first() {
    let mut cfg = router_config();
    cfg.max_wait_secs = 1;
    let c = Cluster::start_with(cfg, &[("w0", "v", 1)], |w| w).await;

    // A malformed key is the request's own fault, at once.
    let (status, _, r) = submit(&c, &c.url("/states/nope/tactic"), json!({"tactic": "rfl"})).await;
    assert_eq!((status, &r["error"]["kind"]), (400, &json!("bad_request")));

    // No worker of this version takes it in time: 429, however often asked.
    let body = json!({"code": "#check Nat", "version": "nope"});
    let job = accepted(&c, "/check", body, "check").await;
    let url = c.url(&format!("/jobs/{job}"));
    for wait in ["10", "0"] {
        let (status, r) = look(&c, &url, wait).await;
        assert_eq!((status, &r["error"]["kind"]), (429, &json!("capacity")));
    }

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
