//! A create that keeps nothing says what to do next in `refused`: change
//! the code (`proof`), give it more room (`limit`), change the request
//! (`request`), resend it onto `heads` (`conflict`), or try again later
//! (`retry`). The fake Lean errs on `error_here`, crashes on `CRASH`, and
//! its "type hash" is a declaration's text between its name and `:=`.

mod common;

use common::{Cluster, create};
use serde_json::{Value, json};

const SPEC: &str = "import Foo\n\ntheorem t : 1 = 2 := sorry";

fn id(r: &Value) -> &str {
    r["snapshot"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no snapshot: {r}"))
}

/// Asserts that `r` kept nothing, refused as `refusal`.
fn refused(r: &Value, refusal: &str) {
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(r["refused"], refusal, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_keeps_nothing_says_what_to_do_next() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    // Kept, `sorry` and all: no refusal.
    let spec = create(&c, json!({"code": SPEC, "name": "refusals.spec"})).await;
    assert!(spec.get("refused").is_none(), "{spec}");
    let header = spec["snapshot"]["header"].as_str().unwrap();
    let replaces = json!({"snapshot_id": id(&spec)});

    // The code's own: Lean's errors, the policy, a changed statement, a
    // target not refuted.
    let erring = "import Foo\n\ntheorem u : 1 = 1 := rfl\nerror_here";
    refused(&create(&c, json!({"code": erring})).await, "proof");
    let elab = "elab \"x\" : term => CRASH";
    let r = create(&c, json!({"code": elab})).await;
    refused(&r, "proof");
    assert_eq!(r["check"]["error"]["kind"], "policy", "{r}");
    let changed = "import Foo\n\ntheorem t : 2 = 2 := rfl";
    let r = create(&c, json!({"code": changed, "replaces": replaces})).await;
    refused(&r, "proof");
    assert!(r["check"]["changes"].is_array(), "{r}");
    let not_negated = format!("{header}\n\ntheorem r : ¬ (1 = 3) := by decide");
    let refutes = json!({"snapshot_id": id(&spec), "target": "t"});
    let r = create(&c, json!({"code": not_negated, "refutes": refutes})).await;
    refused(&r, "proof");

    // Lean crashed: maybe right.
    let crashing = "import Foo\n\ntheorem s : 1 = 1 := rfl\nCRASH";
    let r = create(&c, json!({"code": crashing})).await;
    refused(&r, "limit");
    assert_eq!(r["check"]["error"]["kind"], "crash", "{r}");

    // Replaced already: resend onto the head.
    let proof = "import Foo\n\ntheorem t : 1 = 2 := proof";
    let head = create(&c, json!({"code": proof, "replaces": replaces})).await;
    let again = format!("{proof}\n-- again");
    let r = create(&c, json!({"code": again, "replaces": replaces})).await;
    refused(&r, "conflict");
    assert_eq!(r["heads"], json!([id(&head)]), "{r}");

    // The request's own: holes that aren't definitions, a name another run
    // has, a key on other code, a run no one named.
    let r = create(&c, json!({"code": SPEC, "holes": ["nope"]})).await;
    refused(&r, "request");
    let named = "import Foo\n\ntheorem n : 1 = 1 := sorry";
    let r = create(&c, json!({"code": named, "name": "refusals.spec"})).await;
    refused(&r, "request");
    assert_eq!(r["check"]["error"]["kind"], "conflict", "{r}");
    create(&c, json!({"code": named, "key": "refusals-1"})).await;
    let other = format!("{named}\n-- other");
    let r = create(&c, json!({"code": other, "key": "refusals-1"})).await;
    refused(&r, "request");
    let unnamed = "import PisaRun.nobody\n\ntheorem m : 1 = 1 := rfl";
    refused(&create(&c, json!({"code": unnamed})).await, "request");

    // A job's answer says so too.
    let r = c
        .client
        .post(c.url("/snapshots"))
        .header("prefer", "respond-async")
        .json(&json!({"code": erring}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let job: Value = r.json().await.unwrap();
    let path = format!("/jobs/{}?wait=30", job["id"].as_str().unwrap());
    let r = c.client.get(c.url(&path)).send().await.unwrap();
    let status = r.status().as_u16();
    let (_, answer) = common::job_answer(status, r.json().await.unwrap());
    refused(&answer, "proof");
}
