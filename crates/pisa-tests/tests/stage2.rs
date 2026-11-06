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
    assert_eq!(r["declarations"][0]["axioms"], json!(["propext"]));
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
