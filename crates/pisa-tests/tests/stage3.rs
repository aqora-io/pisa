//! Stage 3: tactic sessions, on Lean Beam documents: here the fake Beam, on
//! which a `sorry`'s goal is `⊢ True`, `rfl` closes a goal, `fail` fails, and
//! any other tactic leaves `<goal> after <tactic>`. The final check runs on
//! the REPL, here fake-repl.

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
    let c = Cluster::start_beam(&[("w0", "v", 2)]).await;
    let s = start(&c, CODE).await;
    let root = &s["states"][0];
    assert_eq!(root["goals"], json!(["⊢ True"]));
    assert_eq!(root["pos"]["line"], 2, "positions match the submitted code");
    assert_eq!(s["messages"][0]["severity"], "warning");

    let step = tactic(&c, &root["id"], "simp").await;
    assert_eq!(step["state"]["goals"], json!(["⊢ True after simp"]));
    assert!(
        step["state"].get("pos").is_none(),
        "only a session's first states have positions: {step}"
    );
    assert_eq!(step["complete"], false);
    let done = tactic(&c, &step["state"]["id"], "rfl").await;
    assert_eq!(done["complete"], true);
    assert_eq!(done["state"]["goals"], json!([]));

    // Old states stay valid: branch from the root again. The same tactic on
    // the same state is the same state.
    let other = tactic(&c, &root["id"], "rfl").await;
    assert_eq!(other["complete"], true);
    assert_ne!(other["state"]["id"], step["state"]["id"]);
    let again = tactic(&c, &root["id"], "simp").await;
    assert_eq!(again["state"]["id"], step["state"]["id"]);

    // A tactic that fails leaves no state.
    let (status, failed) = try_tactic(&c, &root["id"], "fail").await;
    assert_eq!(status, 422);
    assert_eq!(
        failed,
        json!({"error": {"kind": "lean", "message": "fake: tactic failed"}})
    );
    // A sorry closes nothing.
    let admitted = tactic(&c, &root["id"], "rfl -- sorry").await;
    assert_eq!(admitted["complete"], false);

    let session = &s["id"];
    let ok = check(&c, session, "import Foo\ntheorem t : 1 = 1 := by rfl").await;
    assert_eq!(ok["ok"], true, "{ok}");

    let weaker = check(&c, session, "import Foo\ntheorem t : 2 = 2 := by rfl").await;
    assert_eq!(weaker["ok"], false);
    assert_eq!(
        weaker["reasons"],
        json!(["statement: t has a different type"])
    );
    // A session has no exports, so no signatures.
    assert_eq!(weaker["changes"], json!([{"name": "t", "change": "type"}]));

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
async fn sessions_resume_on_another_worker_by_replaying_their_steps() {
    let mut c = Cluster::start_beam(&[("w0", "v", 1)]).await;
    let s = start(&c, CODE).await;
    let root = &s["states"][0]["id"];
    let step = tactic(&c, root, "simp").await;
    let deeper = tactic(&c, &step["state"]["id"], "intro").await;

    // Bring up a second worker and kill the first, which held the document
    // and every handle.
    c.add_worker(common::beam_worker_config("w1", "v", 1));
    c.wait_for_workers(2).await;
    c.kill_worker(0).await;

    // The other worker reopens the document and replays simp, then intro.
    let next = tactic(&c, &deeper["state"]["id"], "ring").await;
    assert_eq!(
        next["state"]["goals"],
        json!(["⊢ True after simp after intro after ring"])
    );
    let branch = tactic(&c, root, "intro").await;
    assert_eq!(branch["state"]["goals"], json!(["⊢ True after intro"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_is_an_open_document_whose_code_stays() {
    let c = Cluster::start_beam(&[("w0", "v", 1)]).await;
    let s = start(&c, CODE).await;
    let id = s["id"].as_str().unwrap();
    let (status, g) = c
        .post(
            &format!("/documents/{id}/goals"),
            json!({"at": [{"line": 2, "column": 24}]}),
        )
        .await;
    assert_eq!(status, 200, "{g}");
    assert_eq!(g["at"][0]["goals"], json!(["⊢ sorry"]));
    let edit = json!({"code": "import Foo\ntheorem t : 2 = 2 := by sorry", "revision": 1});
    let (status, e) = c
        .request(
            reqwest::Method::PATCH,
            &format!("/documents/{id}"),
            Some(edit),
        )
        .await;
    assert_eq!(
        (status, &e["error"]["kind"]),
        (400, &json!("bad_request")),
        "{e}"
    );
    // Closing the document ends its states.
    let (status, _) = c
        .request(reqwest::Method::DELETE, &format!("/documents/{id}"), None)
        .await;
    assert_eq!(status, 204);
    let (status, e) = try_tactic(&c, &s["states"][0]["id"], "simp").await;
    assert_eq!(
        (status, &e["error"]["kind"]),
        (404, &json!("not_found")),
        "{e}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_beam_sessions_are_unavailable() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (status, e) = c.post("/sessions", json!({"code": CODE})).await;
    assert_eq!(
        (status, &e["error"]["kind"]),
        (503, &json!("unavailable")),
        "{e}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_keys_unknown_states_and_policy() {
    let c = Cluster::start_beam(&[("w0", "v", 1)]).await;
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
    assert_eq!(body["error"]["kind"], "policy", "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("rejected: forbidden: run_tac (line 1: "),
        "{message}"
    );

    let no_session = format!("{}~{}", c.version("v"), "0".repeat(32));
    let (status, body) = c
        .post(
            &format!("/sessions/{no_session}/check"),
            json!({"code": "#check Nat"}),
        )
        .await;
    assert_eq!((status, &body["error"]["kind"]), (404, &json!("not_found")));
    let (status, body) = c
        .post(
            &format!("/sessions/{unknown}/check"),
            json!({"code": "#check Nat"}),
        )
        .await;
    assert_eq!(
        (status, &body["error"]["kind"]),
        (400, &json!("bad_request"))
    );

    let (status, body) = c.post("/sessions", json!({"code": "#eval 1"})).await;
    assert_eq!((status, &body["error"]["kind"]), (422, &json!("policy")));
}
