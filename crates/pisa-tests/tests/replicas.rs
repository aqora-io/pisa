//! Several servers on one job queue: any of them serves any request, they
//! share the workers' slots, and a request that goes away loses nothing else.

mod common;

use std::time::{Duration, Instant};

use common::{Cluster, url_of};
use pisa_client::{Client, HealthStatus};
use pisa_worker::queue::LIVE;
use serde_json::{Value, json};

const CODE: &str = "import Foo\ntheorem t : 1 = 1 := by sorry";

async fn until(what: &str, mut done: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn any_server_serves_any_request() {
    let c = Cluster::start_beam(&[("w0", "v", 1), ("w1", "v", 1)]).await;
    let b = c.start_server().await;
    for server in [&c.router, &b] {
        let (status, r) = c
            .post_to(server, "/check", json!({"code": "#check Nat"}))
            .await;
        assert_eq!(status, 200, "{r}");
        assert_eq!(r["messages"][0]["text"], "Nat : Type");
    }
    let (status, s) = c.post("/sessions", json!({"code": CODE})).await;
    assert_eq!(status, 200, "{s}");
    let state = s["states"][0]["id"].as_str().unwrap();
    let (status, step) = c
        .post_to(
            &b,
            &format!("/states/{state}/tactic"),
            json!({"tactic": "simp"}),
        )
        .await;
    assert_eq!(status, 200, "{step}");
}

#[tokio::test(flavor = "multi_thread")]
async fn servers_share_the_workers_slots() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let b = c.start_server().await;
    let slow = json!({"code": "SLEEP 500\n#check Nat", "timeout": 10});
    let started = Instant::now();
    let (r1, r2) = tokio::join!(
        c.post_to(&c.router, "/check", slow.clone()),
        c.post_to(&b, "/check", slow)
    );
    for (status, r) in [r1, r2] {
        assert_eq!(status, 200, "{r}");
        assert!(r.get("error").is_none(), "{r}");
    }
    assert!(
        started.elapsed() >= Duration::from_millis(1000),
        "one slot ran both at once"
    );
}

/// A session's tactics go to the worker holding its document open,
/// whichever server they come through, while that worker is live.
#[tokio::test(flavor = "multi_thread")]
async fn tactics_go_to_the_worker_holding_their_session() {
    let c = Cluster::start_beam(&[("w0", "v", 1), ("w1", "v", 1)]).await;
    let b = c.start_server().await;
    let (status, s) = c.post("/sessions", json!({"code": CODE})).await;
    assert_eq!(status, 200, "{s}");
    let state = s["states"][0]["id"].as_str().unwrap().to_string();
    let (_, session) = s["id"].as_str().unwrap().split_once('~').unwrap();
    let records = pisa_worker::records::Records::new(c.db.clone(), c.version("v"));
    let holder = async || records.document(session).await.unwrap().unwrap().holder;
    let first = holder().await;
    assert!(first.is_some());
    for server in [&b, &c.router, &b, &c.router, &b, &c.router] {
        let (status, step) = c
            .post_to(
                server,
                &format!("/states/{state}/tactic"),
                json!({"tactic": "simp"}),
            )
            .await;
        assert_eq!(status, 200, "{step}");
        assert_eq!(holder().await, first);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_goes_away_leaves_the_rest_alone() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let impatient = reqwest::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    // Running when its client goes, its job runs to the end.
    let slow = json!({"code": "SLEEP 1000\n#check Nat", "timeout": 10});
    assert!(
        impatient
            .post(c.url("/check"))
            .json(&slow)
            .send()
            .await
            .is_err()
    );
    // Queued behind it when its client goes, its job is given up.
    let queued = json!({"code": "#check Int"});
    assert!(
        impatient
            .post(c.url("/check"))
            .json(&queued)
            .send()
            .await
            .is_err()
    );
    until("the queued job is given up", async || {
        c.queue.counts(LIVE).await.unwrap().queued == [0, 0]
    })
    .await;
    assert_eq!(c.queue.counts(LIVE).await.unwrap().running, 1);

    let (status, r) = c.post("/check", json!({"code": "#check Nat"})).await;
    assert_eq!(status, 200, "{r}");
    assert!(r.get("error").is_none(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_shutting_down_answers_what_it_has() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let b = c.start_server().await;
    let url = url_of(&b, "/check");
    let slow = {
        let client = c.client.clone();
        let url = url.clone();
        tokio::spawn(async move {
            let r = client
                .post(url)
                .json(&json!({"code": "SLEEP 500\n#check Nat", "timeout": 10}))
                .send()
                .await
                .unwrap();
            (r.status().as_u16(), r.json::<Value>().await.unwrap())
        })
    };
    until("the job is claimed", async || {
        c.queue.counts(LIVE).await.unwrap().running == 1
    })
    .await;
    let health = url_of(&b, "/health");
    let base = url_of(&b, "/");
    let client = Client::new(&base).unwrap();
    assert_eq!(client.health().await.unwrap().status, HealthStatus::Ok);
    let stopping = tokio::spawn(b.shutdown(Duration::from_millis(300), Duration::from_secs(10)));
    // Load balancers learn first, while it still serves.
    let mut draining = None;
    until("the server drains", async || {
        let health = client.health().await.unwrap();
        let drained = health.status == HealthStatus::Draining;
        draining = Some(health);
        drained
    })
    .await;
    // The client returns that health rather than failing on the 503.
    assert_eq!(draining.unwrap().workers, Some(1));
    let r = c.client.get(health).send().await.unwrap();
    assert_eq!(r.status(), 503);
    assert_eq!(r.json::<Value>().await.unwrap()["status"], "draining");
    stopping.await.unwrap();
    let (status, r) = slow.await.unwrap();
    assert_eq!(status, 200, "{r}");
    assert!(r.get("error").is_none(), "{r}");
    assert!(c.client.post(url).json(&json!({})).send().await.is_err());
}
