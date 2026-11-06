//! Stage 3: tactic sessions with stored proof states.

mod common;

use common::Cluster;
use serde_json::{Value, json};

async fn start(c: &Cluster, code: &str) -> Value {
    let (status, body) = c.post("/sessions", json!({"code": code})).await;
    assert_eq!(status, 200, "{body}");
    body
}

async fn try_tactic(c: &Cluster, state: &Value, tactic: &str) -> (u16, Value) {
    let state = state.as_str().unwrap();
    c.post(
        &format!("/states/{state}/tactic"),
        json!({"tactic": tactic}),
    )
    .await
}

async fn tactic(c: &Cluster, state: &Value, tactic: &str) -> Value {
    let (status, body) = try_tactic(c, state, tactic).await;
    assert_eq!(status, 200, "{body}");
    body
}

async fn check(c: &Cluster, session: &Value, code: &str) -> Value {
    let session = session.as_str().unwrap();
    let (status, body) = c
        .post(&format!("/sessions/{session}/check"), json!({"code": code}))
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
    assert_eq!(root["goals"], json!(["⊢ goal 1"]));
    assert_eq!(root["pos"]["line"], 2, "positions match the submitted code");
    assert_eq!(s["messages"][0]["severity"], "warning");

    let step = tactic(&c, &root["id"], "simp").await;
    assert_eq!(step["state"]["goals"], json!(["⊢ goal 1 after simp"]));
    assert!(
        step["state"].get("pos").is_none(),
        "only a session's first states have positions: {step}"
    );
    assert_eq!(step["complete"], false);
    let done = tactic(&c, &step["state"]["id"], "rfl").await;
    assert_eq!(done["complete"], true);
    assert_eq!(done["state"]["goals"], json!([]));

    // Old states stay valid: branch from the root again.
    let other = tactic(&c, &root["id"], "rfl").await;
    assert_eq!(other["complete"], true);
    assert_ne!(other["state"]["id"], step["state"]["id"]);

    let (status, failed) = try_tactic(&c, &root["id"], "fail").await;
    assert_eq!(status, 422);
    assert_eq!(
        failed,
        json!({"error": {"kind": "lean", "message": "Lean error:\nfailed"}})
    );

    let session = &s["id"];
    let ok = check(&c, session, "import Foo\ntheorem t : 1 = 1 := by rfl").await;
    assert_eq!(ok["ok"], true, "{ok}");

    let weaker = check(&c, session, "import Foo\ntheorem t : 2 = 2 := by rfl").await;
    assert_eq!(weaker["ok"], false);
    assert_eq!(
        weaker["reasons"],
        json!(["statement: t has a different type"])
    );

    let renamed = check(&c, session, "import Foo\ntheorem u : 1 = 1 := by rfl").await;
    assert_eq!(renamed["reasons"], json!(["statement: t is missing"]));

    let reimported = check(&c, session, "import Bar\ntheorem t : 1 = 1 := by rfl").await;
    assert_eq!(reimported["ok"], false);
    assert_eq!(
        reimported["reasons"],
        json!(["statement: the imports differ from the session's"])
    );

    let cheated = check(&c, session, "import Foo\ntheorem t : 1 = 1 := by sorry").await;
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
    let step = tactic(&c, &s["states"][0]["id"], "simp").await;

    // Bring up a second worker and kill the first, which held every state.
    c.add_worker(common::worker_config("w1", "v", 1));
    c.wait_for_workers(2).await;
    c.kill_worker(0).await;

    let done = tactic(&c, &step["state"]["id"], "rfl").await;
    assert_eq!(done["complete"], true);
    let branch = tactic(&c, &s["states"][0]["id"], "omega").await;
    assert_eq!(branch["state"]["goals"], json!(["⊢ goal 1 after omega"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_keys_unknown_states_and_policy() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let unknown = format!("{}~abc~0123", c.version("v"));
    let (status, body) = try_tactic(&c, &json!("not-a-key"), "rfl").await;
    assert_eq!(
        (status, &body["error"]["kind"]),
        (400, &json!("bad_request"))
    );

    let (status, body) = try_tactic(&c, &json!(unknown), "rfl").await;
    assert_eq!((status, &body["error"]["kind"]), (404, &json!("not_found")));

    let (status, body) = try_tactic(&c, &json!(unknown), "run_tac pure ()").await;
    assert_eq!(status, 422);
    assert_eq!(
        body["error"],
        json!({"kind": "policy", "message": "rejected: forbidden: run_tac"})
    );

    let (status, body) = c
        .post(
            &format!("/sessions/{unknown}/check"),
            json!({"code": "#check Nat"}),
        )
        .await;
    assert_eq!((status, &body["error"]["kind"]), (404, &json!("not_found")));

    let (status, body) = c.post("/sessions", json!({"code": "#eval 1"})).await;
    assert_eq!((status, &body["error"]["kind"]), (422, &json!("policy")));
}
