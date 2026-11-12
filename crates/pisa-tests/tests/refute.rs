//! Refuting a spec's target: a create that names the target it refutes is
//! kept only if it builds on the spec and proves a theorem of `¬` the
//! target's statement. The fake Lean's "type hash" is a declaration's text
//! between its name and `:=`, and a theorem of `¬ X` or `¬ (X)` negates `X`.

mod common;

use common::Cluster;
use serde_json::{Value, json};

const SPEC: &str = "import Foo\n\ntheorem t : 1 = 2 := sorry\ntheorem easy : 1 = 1 := sorry";

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    r
}

fn id(r: &Value) -> String {
    r["snapshot"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no snapshot: {r}"))
        .to_string()
}

/// `spec`'s header, a blank line, then `body`.
fn on(spec: &Value, body: &str) -> String {
    format!("{}\n\n{body}", spec["snapshot"]["header"].as_str().unwrap())
}

fn refutes(spec: &str, target: &str) -> Value {
    json!({"snapshot_id": spec, "target": target})
}

async fn delete(c: &Cluster, id: &str) {
    let r = c.client.delete(c.url(&format!("/snapshots/{id}"))).send();
    assert_eq!(r.await.unwrap().status(), 200);
}

/// Asserts that a create kept nothing because of `reason`.
fn refused(r: &Value, reason: &str) {
    assert!(r.get("snapshot").is_none(), "{r}");
    let reasons = r["check"]["reasons"].as_array().unwrap();
    assert!(reasons.contains(&json!(reason)), "{r}");
}

/// The refutations stored, by the entry that refutes.
async fn refutations(c: &Cluster) -> Vec<String> {
    let rows = c.stored_rows("v").await;
    rows.into_iter()
        .filter(|r| r[0] == "refutations")
        .map(|r| r[1].clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proved_negation_on_the_spec_refutes_its_target() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec_r = create(&c, json!({"code": SPEC})).await;
    let spec = id(&spec_r);
    let code = on(&spec_r, "theorem r : ¬ (1 = 2) := by decide");
    let r = create(&c, json!({"code": code, "refutes": refutes(&spec, "t")})).await;
    let by = id(&r);
    assert_eq!(
        r["refutation"],
        json!({"snapshot_id": by, "spec": spec, "target": "t", "theorem": "r"})
    );
    assert_eq!(r["snapshot"]["verified"], true, "{r}");
    // The same code, kept, refutes again, and a key replay answers again.
    let keyed = json!({"code": code, "key": "refute-1", "refutes": refutes(&spec, "t")});
    let first = create(&c, keyed.clone()).await;
    assert_eq!(create(&c, keyed).await["refutation"], first["refutation"]);
    assert_eq!(first["refutation"]["theorem"], "r");
    delete(&c, &id(&first)).await;

    // The record outlives the refuter, which the audit says is gone, and
    // the spec.
    let audit = get(&c, &format!("/snapshots/audit?against={spec}")).await;
    let gone: Vec<(Value, Value)> = (audit["refutations"].as_array().unwrap().iter())
        .map(|r| (r["snapshot_id"].clone(), r["gone"].clone()))
        .collect();
    assert!(gone.contains(&(json!(id(&first)), json!(true))), "{audit}");
    assert!(gone.contains(&(json!(by), Value::Null)), "{audit}");
    assert_eq!(audit["clean"], false);
    let bare = |id: &str| id.split_once('~').unwrap().1.to_string();
    let mut kept = vec![bare(&by), bare(&id(&first))];
    kept.sort();
    delete(&c, &by).await;
    delete(&c, &spec).await;
    let mut stored = refutations(&c).await;
    stored.sort();
    assert_eq!(stored, kept);
}

/// `GET path`, which must answer 200.
async fn get(c: &Cluster, path: &str) -> Value {
    let r = c.client.get(c.url(path)).send().await.unwrap();
    assert_eq!(r.status(), 200, "{path}");
    r.json().await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_hears_of_its_refutations_and_keeps_them_through_an_amendment() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec_r = create(&c, json!({"code": SPEC})).await;
    let spec = id(&spec_r);
    let refuted = |by: &str, spec: &str| json!({"snapshot_id": by, "spec": spec, "target": "t", "theorem": "r"});
    // A head that states `easy` as `easy`.
    let attempt = |replaced: &str, easy: &str| {
        let head =
            format!("import Foo\n\ntheorem t : 1 = 2 := sorry\ntheorem easy : {easy} := rfl");
        json!({"code": head, "replaces": {"snapshot_id": replaced}, "fork": true})
    };
    // A create into a run that nothing refutes says nothing.
    let r = create(&c, attempt(&spec, "1 = 1")).await;
    assert!(r.get("refutations").is_none(), "{r}");
    let code = on(&spec_r, "theorem r : ¬ (1 = 2) := by decide");
    let by = id(&create(&c, json!({"code": code, "refutes": refutes(&spec, "t")})).await);
    // One that replaces into a refuted run is told.
    let r = create(&c, attempt(&spec, "1 = 1")).await;
    assert_eq!(r["refutations"], json!([refuted(&by, &spec)]), "{r}");

    // An amendment that keeps t's statement keeps its refutation.
    let kept = "import Foo\n\ntheorem t : 1 = 2 := sorry\ntheorem easy : 2 = 2 := sorry";
    let amends = |spec: &str| json!({"snapshot_id": spec, "reason": "easy was wrong"});
    let amended = id(&create(&c, json!({"code": kept, "amends": amends(&spec)})).await);
    let audit = get(&c, &format!("/snapshots/audit?against={amended}")).await;
    assert_eq!(
        audit["refutations"],
        json!([refuted(&by, &spec)]),
        "{audit}"
    );
    let targets = audit["targets"].as_array().unwrap();
    let t = targets.iter().find(|t| t["name"] == "t").unwrap();
    assert_eq!(t["refuted_by"], json!([by]), "{audit}");
    assert_eq!(audit["clean"], false);
    let r = create(&c, attempt(&amended, "2 = 2")).await;
    assert_eq!(r["refutations"], json!([refuted(&by, &spec)]), "{r}");
    // One that changes it doesn't.
    let changed = "import Foo\n\ntheorem t : 1 = 3 := sorry\ntheorem easy : 2 = 2 := sorry";
    let fixed = id(&create(&c, json!({"code": changed, "amends": amends(&amended)})).await);
    let audit = get(&c, &format!("/snapshots/audit?against={fixed}")).await;
    assert!(audit.get("refutations").is_none(), "{audit}");
}

/// An amendment that changes what the target means, through a definition
/// its statement uses, or that makes it use a hole, leaves the refutation
/// behind, though the statement reads the same.
#[tokio::test(flavor = "multi_thread")]
async fn an_amendment_that_changes_what_the_target_uses_leaves_its_refutation() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = "import Foo\n\ndef cost : Nat := 7\ntheorem bound : cost ≤ 5 := sorry";
    let cheaper = "import Foo\n\ndef cost : Nat := 3\ntheorem bound : cost ≤ 5 := sorry";
    // The same statement, its definition now a hole.
    for (code, holes) in [(cheaper, Value::Null), (spec, json!(["cost"]))] {
        // A spec of its own each time, refuted, as a spec is amended once.
        let spec_r = create(&c, json!({"code": spec})).await;
        let refuting = on(&spec_r, "theorem r : ¬ (cost ≤ 5) := by decide");
        let body = json!({"code": refuting, "refutes": refutes(&id(&spec_r), "bound")});
        create(&c, body).await;
        let amends = json!({"snapshot_id": id(&spec_r), "reason": "cost was wrong"});
        let body = json!({"code": code, "amends": amends, "holes": holes});
        let amended = id(&create(&c, body).await);
        let audit = get(&c, &format!("/snapshots/audit?against={amended}")).await;
        assert!(audit.get("refutations").is_none(), "{audit}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn what_doesnt_prove_the_negation_on_the_spec_is_refused() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec_r = create(&c, json!({"code": SPEC})).await;
    let spec = id(&spec_r);
    let refuting = |code: String| json!({"code": code, "refutes": refutes(&spec, "t")});
    let sorried = on(&spec_r, "theorem r : ¬ (1 = 2) := sorry");
    refused(
        &create(&c, refuting(sorried)).await,
        "refutation: r states ¬ t, but isn't proved: it is sorry",
    );
    let other = on(&spec_r, "theorem r : ¬ (1 = 3) := by decide");
    refused(
        &create(&c, refuting(other)).await,
        "refutation: no theorem states ¬ t, its statement exactly",
    );
    let apart = "import Foo\n\ntheorem r : ¬ (1 = 2) := by decide".to_string();
    let r = create(&c, refuting(apart)).await;
    let module = spec_r["snapshot"]["module"].as_str().unwrap();
    refused(
        &r,
        &format!("refutation: the code doesn't build on the spec: import {module}"),
    );
    assert!(refutations(&c).await.is_empty());

    // A target that isn't one, or that a head could fill.
    let code = on(&spec_r, "theorem r : ¬ (1 = 2) := by decide");
    let r = create(&c, json!({"code": code, "refutes": refutes(&spec, "u")})).await;
    assert_eq!(r["check"]["error"]["kind"], "bad_request", "{r}");
    let holed = "import Foo\n\ndef cost : Nat := sorry\ntheorem c : cost = 2 := sorry";
    let holed_r = create(&c, json!({"code": holed, "holes": ["cost"]})).await;
    let code = on(&holed_r, "theorem r : ¬ (cost = 2) := by decide");
    let body = json!({"code": code, "refutes": refutes(&id(&holed_r), "c")});
    let r = create(&c, body).await;
    let message = r["check"]["error"]["message"].as_str().unwrap();
    assert!(message.contains("uses the spec's holes"), "{r}");
    // A hole reached through a definition of a snapshot the spec builds on.
    let helper = "import Foo\n\ndef base : Nat := 7\ndef wrapped : Nat := base";
    let helper_r = create(&c, json!({"code": helper})).await;
    let through = on(&helper_r, "theorem w : wrapped = 2 := sorry");
    let through_r = create(&c, json!({"code": through, "holes": ["base"]})).await;
    let code = on(&through_r, "theorem r : ¬ (wrapped = 2) := by decide");
    let body = json!({"code": code, "refutes": refutes(&id(&through_r), "w")});
    let r = create(&c, body).await;
    let message = r["check"]["error"]["message"].as_str();
    assert!(
        message.is_some_and(|m| m.contains("uses the spec's holes")),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refutation_is_a_re_checked_snapshot_of_its_own() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec_r = create(&c, json!({"code": SPEC})).await;
    let spec = id(&spec_r);
    let code = on(&spec_r, "theorem r : ¬ (1 = 2) := by decide");
    let with = |extra: Value| {
        let mut body = json!({"code": code, "refutes": refutes(&spec, "t")});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    for (body, says) in [
        (
            with(json!({"replaces": {"snapshot_id": spec}})),
            "snapshot of its own",
        ),
        (with(json!({"holes": ["t"]})), "snapshot of its own"),
        (
            with(json!({"amends": {"snapshot_id": spec, "reason": "r"}})),
            "snapshot of its own",
        ),
        (with(json!({"verify": false})), "always re-checked"),
        (
            json!({"code": code, "refutes": refutes("nope", "t")}),
            "not a snapshot id",
        ),
    ] {
        let (status, r) = c.post("/snapshots", body.clone()).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{body}: {r}"
        );
        let message = r["error"]["message"].as_str().unwrap();
        assert!(message.contains(says), "{message}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refuted_target_isnt_accepted_and_the_run_isnt_clean() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec_r = create(&c, json!({"code": SPEC})).await;
    let spec = id(&spec_r);
    // The fake proves anything without `sorry`.
    let head = "import Foo\n\ntheorem t : 1 = 2 := rfl\ntheorem easy : 1 = 1 := rfl";
    create(&c, json!({"code": head, "replaces": {"snapshot_id": spec}})).await;
    let audit = || async {
        let r = c
            .client
            .get(c.url(&format!("/snapshots/audit?against={spec}")));
        let r = r.send().await.unwrap();
        assert_eq!(r.status(), 200);
        r.json::<Value>().await.unwrap()
    };
    let before = audit().await;
    assert_eq!(before["clean"], true, "{before}");
    assert!(before.get("refutations").is_none(), "{before}");

    let code = on(&spec_r, "theorem r : ¬ (1 = 2) := by decide");
    let by = id(&create(&c, json!({"code": code, "refutes": refutes(&spec, "t")})).await);
    let after = audit().await;
    assert_eq!(after["clean"], false, "{after}");
    assert_eq!(
        after["refutations"],
        json!([{"snapshot_id": by, "spec": spec, "target": "t", "theorem": "r"}])
    );
    let target = |name: &str| {
        let targets = after["targets"].as_array().unwrap();
        targets.iter().find(|t| t["name"] == name).unwrap().clone()
    };
    assert_eq!(
        (
            target("t")["accepted"].clone(),
            target("t")["refuted_by"].clone()
        ),
        (json!(false), json!([by]))
    );
    assert_eq!(target("easy")["accepted"], true);
    assert!(target("easy").get("refuted_by").is_none());
    // The head proves what is refuted: the spec's assumptions contradict.
    assert_eq!(target("t")["contradicted"], true, "{after}");
    assert!(target("easy").get("contradicted").is_none());
}
