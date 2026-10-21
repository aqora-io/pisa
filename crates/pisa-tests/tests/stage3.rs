//! Stage 3: tactic sessions with stored proof states.

mod common;

use common::Cluster;
use serde_json::{Value, json};

async fn start(c: &Cluster, code: &str) -> Value {
    let (status, body) = c.post("/sessions", json!({"code": code})).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.get("error").is_none(), "{body}");
    body
}

async fn tactic(c: &Cluster, state: &Value, tactic: &str) -> Value {
    let state = state.as_str().unwrap();
    let (status, body) = c
        .post(
            &format!("/sessions/{state}/tactic"),
            json!({"tactic": tactic}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    body
}

const CODE: &str = "import Foo\ntheorem t : 1 = 1 := by sorry";

#[tokio::test(flavor = "multi_thread")]
async fn session_is_a_tree_of_states_with_a_final_check() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let s = start(&c, CODE).await;
    let root = &s["states"][0];
    assert_eq!(root["goal"], "⊢ goal 1");
    assert_eq!(root["pos"]["line"], 2, "positions match the submitted code");
    assert_eq!(s["messages"][0]["severity"], "warning");

    let step = tactic(&c, &root["state"], "simp").await;
    assert_eq!(step["goals"], json!(["⊢ goal 1 after simp"]));
    assert_eq!(step["proofStatus"], "Incomplete: open goals remain");
    let done = tactic(&c, &step["state"], "rfl").await;
    assert_eq!(done["proofStatus"], "Completed");
    assert_eq!(done["goals"], json!([]));

    // Old states stay valid: branch from the root again.
    let other = tactic(&c, &root["state"], "rfl").await;
    assert_eq!(other["proofStatus"], "Completed");
    assert_ne!(other["state"], step["state"]);

    let failed = tactic(&c, &root["state"], "fail").await;
    assert!(failed.get("state").is_none());
    assert_eq!(failed["error"], "Lean error:\nfailed");

    let session = s["session"].as_str().unwrap();
    let check = |code: &'static str| {
        let c = &c;
        async move {
            let (status, body) = c
                .post(&format!("/sessions/{session}/check"), json!({"code": code}))
                .await;
            assert_eq!(status, 200, "{body}");
            body
        }
    };
    let ok = check("import Foo\ntheorem t : 1 = 1 := by rfl").await;
    assert_eq!(ok["ok"], true, "{ok}");

    let weaker = check("import Foo\ntheorem t : 2 = 2 := by rfl").await;
    assert_eq!(weaker["ok"], false);
    assert_eq!(
        weaker["reasons"],
        json!(["statement: t has a different type"])
    );

    let renamed = check("import Foo\ntheorem u : 1 = 1 := by rfl").await;
    assert_eq!(renamed["reasons"], json!(["statement: t is missing"]));

    let cheated = check("import Foo\ntheorem t : 1 = 1 := by sorry").await;
    assert_eq!(cheated["ok"], false);
    assert!(
        cheated["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("axiom: t uses sorryAx")),
        "{cheated}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_resume_on_another_worker() {
    let mut c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = start(&c, CODE).await;
    let step = tactic(&c, &s["states"][0]["state"], "simp").await;

    // Bring up a second worker and kill the first, which held every state.
    c.add_worker(common::worker_config(c.router.worker_addr, "w1", "v", 1));
    c.wait_for_workers(2).await;
    c.workers[0].abort();
    c.wait_for_workers(1).await;

    let done = tactic(&c, &step["state"], "rfl").await;
    assert!(done.get("error").is_none(), "{done}");
    assert_eq!(done["proofStatus"], "Completed");
    let branch = tactic(&c, &s["states"][0]["state"], "omega").await;
    assert_eq!(branch["goals"], json!(["⊢ goal 1 after omega"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_keys_and_unknown_states() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (status, _) = c
        .post("/sessions/not-a-key/tactic", json!({"tactic": "rfl"}))
        .await;
    assert_eq!(status, 400);
    let unknown = tactic(&c, &json!("v~abc~0123"), "rfl").await;
    assert!(
        unknown["error"]
            .as_str()
            .unwrap()
            .starts_with("unknown state"),
        "{unknown}"
    );
    let meta = tactic(&c, &json!("v~abc~0123"), "run_tac pure ()").await;
    assert_eq!(meta["error"], "Rejected by policy: forbidden: run_tac");
}
