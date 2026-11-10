//! Stage 2: verification policy and sandboxed workers.

mod common;

use common::Cluster;
use pisa_worker::config::Sandbox;
use serde_json::{Value, json};

async fn check(c: &Cluster, code: &str) -> Value {
    let (status, body) = c.post("/check", json!({"code": code})).await;
    assert_eq!(status, 200, "{body}");
    body
}

#[tokio::test(flavor = "multi_thread")]
async fn clean_proof_passes_with_declarations() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let r = check(&c, "import Mathlib\ntheorem t : 1 = 1 := rfl").await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["declarations"][0]["name"], "t");
    // Only the axioms outside the allowed ones, unless asked for in full.
    assert_eq!(r["declarations"][0]["axioms"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn declarations_are_summarised_unless_asked_for_in_full() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let get = |path: String| {
        let c = &c;
        async move {
            let r = c.client.get(c.url(&path)).send().await.unwrap();
            assert_eq!(r.status(), 200);
            r.json::<Value>().await.unwrap()
        }
    };
    // Its name, kind and status, and the axioms outside the allowed ones:
    // the fake's `propext` is allowed.
    let summary =
        |name: &str| json!({"name": name, "kind": "theorem", "axioms": [], "status": "proved"});
    let full = |d: &Value| d["type_hash"].is_string() && d["axioms"] == json!(["propext"]);
    let code = "import Foo\ntheorem t : 1 = 1 := rfl";
    let detail = json!(["declarations"]);

    let r = check(&c, code).await;
    assert_eq!(r["declarations"], json!([summary("t")]), "{r}");
    let (_, r) = c
        .post("/check", json!({"code": code, "detail": detail}))
        .await;
    assert!(full(&r["declarations"][0]), "{r}");
    let (_, r) = c
        .post("/check/batch", json!({"items": [{"code": code}]}))
        .await;
    assert_eq!(
        r["results"][0]["declarations"],
        json!([summary("t")]),
        "{r}"
    );

    let snapshot = "import Foo\n\ntheorem u : 2 = 2 := rfl";
    let (_, r) = c.post("/snapshots", json!({"code": snapshot})).await;
    assert_eq!(r["check"]["declarations"], json!([summary("u")]), "{r}");
    let id = r["snapshot"]["id"].as_str().unwrap().to_string();
    let (_, r) = c
        .post("/snapshots", json!({"code": snapshot, "detail": detail}))
        .await;
    assert!(full(&r["check"]["declarations"][0]), "{r}");
    let r = get(format!("/snapshots/{id}")).await;
    assert_eq!(r["check"]["declarations"], json!([summary("u")]), "{r}");
    let r = get(format!("/snapshots/{id}?detail=declarations")).await;
    assert!(full(&r["check"]["declarations"][0]), "{r}");

    let (_, s) = c
        .post(
            "/sessions",
            json!({"code": "import Foo\ntheorem v : 3 = 3 := by sorry"}),
        )
        .await;
    let checked = format!("/sessions/{}/check", s["id"].as_str().unwrap());
    let done = "import Foo\ntheorem v : 3 = 3 := rfl";
    let (_, r) = c.post(&checked, json!({"code": done})).await;
    assert_eq!(r["declarations"], json!([summary("v")]), "{r}");
    let (_, r) = c
        .post(&checked, json!({"code": done, "detail": detail}))
        .await;
    assert!(full(&r["declarations"][0]), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_report_runs_on_code_with_errors_and_says_what_it_cant_describe() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let names = |r: &Value| -> Vec<String> {
        let decls = r["declarations"].as_array().unwrap();
        decls
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect()
    };
    // One broken lemma leaves the others' statuses.
    let r = check(
        &c,
        "import Foo\ntheorem good : 1 = 1 := rfl\ntheorem bad : 1 = 2 := error_here",
    )
    .await;
    assert_eq!(r["reasons"], json!(["error: Lean reported errors"]), "{r}");
    assert!(names(&r).contains(&"good".to_string()), "{r}");
    // What the report can't describe is a reason, and the rest is reported.
    let code =
        "import Foo\ntheorem good : 1 = 1 := rfl\ntheorem odd : 2 = 2 := rfl -- report-fails: odd";
    let r = check(&c, code).await;
    assert_eq!(
        r["reasons"],
        json!(["error: axiom report: describing odd: a metavariable"]),
        "{r}"
    );
    assert_eq!(names(&r), ["good"], "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unsound_proofs_are_rejected() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let cases = [
        ("theorem t : False := by sorry", "axiom: t uses sorryAx"),
        (
            "theorem t : False := by admit sorry",
            "axiom: t uses sorryAx",
        ),
        (
            "theorem t : 2 + 2 = 4 := by native_decide",
            "axiom: t uses Lean.ofReduceBool",
        ),
        (
            "axiom cheat : False\ntheorem t : False := cheat",
            "axiom: cheat uses cheat",
        ),
        (
            "theorem t : 1 = 1 := error_here",
            "error: Lean reported errors",
        ),
    ];
    for (code, reason) in cases {
        let r = check(&c, code).await;
        assert_eq!(r["ok"], false, "{code}: {r}");
        let reasons = r["reasons"].as_array().unwrap();
        assert!(reasons.iter().any(|x| x == reason), "{code}: {r}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_rejects_before_running() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for (code, reason) in [
        ("#eval IO.println \"hi\"\nCRASH", "forbidden: #eval"),
        (
            "set_option debug.skipKernelTC true in\ntheorem t : False := CRASH",
            "option: debug.skipKernelTC",
        ),
        ("elab \"x\" : term => CRASH", "forbidden: elab"),
    ] {
        // CRASH would kill the fake REPL if the code ran.
        let r = check(&c, code).await;
        assert_eq!(r["error"]["kind"], "policy", "{r}");
        assert_eq!(r["reasons"], json!([reason]), "{r}");
    }
    let r = check(
        &c,
        "set_option maxHeartbeats 400000 in\ntheorem t : 1 = 1 := rfl",
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
}

fn bwrap_works() -> bool {
    std::process::Command::new("bwrap")
        .args(["--unshare-all", "--ro-bind", "/", "/", "true"])
        .status()
        .is_ok_and(|s| s.success())
}

#[tokio::test(flavor = "multi_thread")]
async fn sandboxed_worker_runs_jobs() {
    if !bwrap_works() {
        eprintln!("skipping: bubblewrap cannot create namespaces here");
        return;
    }
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |mut w| {
        w.sandbox = Sandbox::Bwrap;
        w
    })
    .await;
    let r = check(&c, "theorem t : 1 = 1 := rfl\n#check Nat").await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["messages"][0]["text"], "Nat : Type");
}

#[tokio::test(flavor = "multi_thread")]
async fn imports_that_fail_to_load_together_are_reported() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let r = check(&c, "import Foo\nimport Clash\ntheorem t : 1 = 1 := rfl").await;
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["messages"][0]["severity"], "error", "{r}");
    assert_eq!(
        r["messages"][0]["text"],
        "import Clash failed, environment already contains 'dup' from Other"
    );
    assert_eq!(r["messages"][0]["pos"]["line"], 2);

    // The slot is usable again afterwards.
    let r = check(&c, "import Foo\ntheorem t : 1 = 1 := rfl").await;
    assert_eq!(r["ok"], true, "{r}");
}
