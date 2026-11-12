//! Status through links: a theorem proved with a lemma from another snapshot
//! module inherits the lemma's `sorry`, and the audit reads that lemma on the
//! head of its run. The fake tools give a theorem `sorryAx` when its module
//! says `sorry`, and inherit it through an imported theorem its line names
//! from a module that says `sorry`.

mod common;

use common::Cluster;
use serde_json::{Value, json};

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert!(r.get("snapshot").is_some(), "not kept: {r}");
    r
}

fn id(r: &Value) -> String {
    r["snapshot"]["id"].as_str().unwrap().to_string()
}

/// `spec`'s header, a blank line, then `body`.
fn on(spec: &Value, body: &str) -> String {
    format!("{}\n\n{body}", spec["snapshot"]["header"].as_str().unwrap())
}

fn replacing(c: &Value, code: &str) -> Value {
    json!({"code": code, "replaces": {"snapshot_id": id(c)}})
}

async fn audit(c: &Cluster, specs: &[&Value]) -> Value {
    let query: Vec<String> = specs.iter().map(|s| format!("against={}", id(s))).collect();
    let path = format!("/snapshots/audit?{}", query.join("&"));
    let (status, r) = c.request(reqwest::Method::GET, &path, None).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// Why target `name` of `audit` reads `theorem` as recorded.
fn unlinked(audit: &Value, name: &str, theorem: &str) -> String {
    let unlinked = target(audit, name)["unlinked"].as_array().cloned();
    let unlinked = unlinked.unwrap_or_else(|| panic!("{name} says nothing: {audit}"));
    let found = unlinked.iter().find(|u| u["name"] == theorem);
    let found = found.unwrap_or_else(|| panic!("no {theorem} for {name}: {audit}"));
    found["reason"].as_str().unwrap().to_string()
}

fn target<'a>(audit: &'a Value, name: &str) -> &'a Value {
    let targets = audit["targets"].as_array().unwrap();
    targets
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no target {name}: {audit}"))
}

/// A lemma's spec, a theorem's spec built on it, and a head of the theorem
/// that proves it with the lemma.
async fn top_down(c: &Cluster) -> (Value, Value, Value) {
    let spec_l = create(
        c,
        json!({"code": "import Foo\n\ntheorem lemma_l : 1 = 1 := sorry"}),
    )
    .await;
    let spec_t = create(
        c,
        json!({"code": on(&spec_l, "theorem top : 2 = 2 := sorry")}),
    )
    .await;
    let code = on(&spec_l, "theorem top : 2 = 2 := by have := lemma_l; rfl");
    let head_t = create(c, replacing(&spec_t, &code)).await;
    (spec_l, spec_t, head_t)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_theorem_holds_through_the_head_that_proves_its_lemma() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (spec_l, spec_t, head_t) = top_down(&c).await;
    // Until the lemma is proved, the theorem is sorry, through links too.
    let before = audit(&c, &[&spec_l, &spec_t]).await;
    assert_eq!(target(&before, "top")["status"], "sorry", "{before}");
    let why = unlinked(&before, "top", "lemma_l");
    assert!(why.contains("nothing replaces"), "{why}");
    assert!(
        target(&before, "top").get("linked_status").is_none(),
        "{before}"
    );
    assert!(before.get("unsettled").is_none(), "{before}");

    let head_l = create(
        &c,
        replacing(&spec_l, "import Foo\n\ntheorem lemma_l : 1 = 1 := rfl"),
    )
    .await;
    let after = audit(&c, &[&spec_l, &spec_t]).await;
    let top = target(&after, "top");
    assert!(top.get("unlinked").is_none(), "{after}");
    assert_eq!(
        (&top["status"], &top["linked_status"], &top["accepted"]),
        (&json!("sorry"), &json!("proved"), &json!(false)),
        "{after}"
    );
    assert_eq!(
        top["through"],
        json!([{"name": "lemma_l", "snapshot_id": id(&spec_l), "head": id(&head_l)}])
    );
    assert_eq!(after["clean"], false);
    assert_eq!(
        after["unsettled"],
        json!([{"head": id(&head_t), "swaps": [{"snapshot_id": id(&spec_l), "head": id(&head_l)}]}])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn what_a_lemma_cant_be_read_through_stays_as_recorded() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    // A fork on the lemma's run: two heads, so neither.
    let (spec_l, spec_t, _) = top_down(&c).await;
    for proof in ["rfl", "by rfl"] {
        let code = format!("import Foo\n\ntheorem lemma_l : 1 = 1 := {proof}");
        let mut body = replacing(&spec_l, &code);
        body["fork"] = json!(true);
        create(&c, body).await;
    }
    let forked = audit(&c, &[&spec_l, &spec_t]).await;
    assert!(
        target(&forked, "top").get("linked_status").is_none(),
        "{forked}"
    );
    let why = unlinked(&forked, "top", "lemma_l");
    assert!(why.contains("more than one head"), "{why}");

    // A head that changes a definition of the lemma's module.
    let spec_l = create(
        &c,
        json!({"code": "import Foo\n\ndef rate : Nat := 2\ntheorem lemma_m : 1 = 1 := sorry", "holes": ["rate"]}),
    )
    .await;
    let spec_t = create(
        &c,
        json!({"code": on(&spec_l, "theorem top_m : 2 = 2 := sorry")}),
    )
    .await;
    let code = on(&spec_l, "theorem top_m : 2 = 2 := by have := lemma_m; rfl");
    create(&c, replacing(&spec_t, &code)).await;
    let code = "import Foo\n\ndef rate : Nat := 3\ntheorem lemma_m : 1 = 1 := rfl";
    create(&c, replacing(&spec_l, code)).await;
    let changed = audit(&c, &[&spec_l, &spec_t]).await;
    assert!(
        target(&changed, "top_m").get("linked_status").is_none(),
        "{changed}"
    );
    let why = unlinked(&changed, "top_m", "lemma_m");
    assert!(why.contains("rate"), "{why}");

    // An inheritor the kernel hasn't re-checked.
    let spec_l = create(
        &c,
        json!({"code": "import Foo\n\ntheorem lemma_u : 1 = 1 := sorry"}),
    )
    .await;
    let spec_t = create(
        &c,
        json!({"code": on(&spec_l, "theorem top_u : 2 = 2 := sorry")}),
    )
    .await;
    let code = on(&spec_l, "theorem top_u : 2 = 2 := by have := lemma_u; rfl");
    let mut body = replacing(&spec_t, &code);
    body["verify"] = json!(false);
    create(&c, body).await;
    create(
        &c,
        replacing(&spec_l, "import Foo\n\ntheorem lemma_u : 1 = 1 := rfl"),
    )
    .await;
    let unverified = audit(&c, &[&spec_l, &spec_t]).await;
    assert!(
        target(&unverified, "top_u").get("linked_status").is_none(),
        "{unverified}"
    );
    let why = unlinked(&unverified, "top_u", "lemma_u");
    assert!(why.contains("isn't verified"), "{why}");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_proofs_cant_justify_each_other() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (spec_l, spec_t, _) = top_down(&c).await;
    // The lemma's head proves it with the theorem, which is proved with the
    // lemma.
    let code = on(&spec_t, "theorem lemma_l : 1 = 1 := by have := top; rfl");
    create(&c, replacing(&spec_l, &code)).await;
    let r = audit(&c, &[&spec_l, &spec_t]).await;
    assert!(target(&r, "top").get("linked_status").is_none(), "{r}");
    assert!(target(&r, "lemma_l").get("linked_status").is_none(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settle_that_cant_be_planned_says_why() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    // Nested: the lemma's head is proved with another lemma, whose run the
    // audit doesn't take.
    let spec_k = create(
        &c,
        json!({"code": "import Foo\n\ntheorem lemma_k : 0 = 0 := sorry"}),
    )
    .await;
    let spec_l = create(
        &c,
        json!({"code": on(&spec_k, "theorem lemma_n : 1 = 1 := sorry")}),
    )
    .await;
    let spec_t = create(
        &c,
        json!({"code": on(&spec_l, "theorem top_n : 2 = 2 := sorry")}),
    )
    .await;
    let code = on(&spec_l, "theorem top_n : 2 = 2 := by have := lemma_n; rfl");
    let head_t = create(&c, replacing(&spec_t, &code)).await;
    let code = on(
        &spec_k,
        "theorem lemma_n : 1 = 1 := by have := lemma_k; rfl",
    );
    let head_l = create(&c, replacing(&spec_l, &code)).await;
    create(
        &c,
        replacing(&spec_k, "import Foo\n\ntheorem lemma_k : 0 = 0 := rfl"),
    )
    .await;
    let alone = audit(&c, &[&spec_t]).await;
    assert_eq!(
        target(&alone, "top_n")["linked_status"],
        "proved",
        "{alone}"
    );
    let unsettled = &alone["unsettled"][0];
    assert_eq!(unsettled["head"], json!(id(&head_t)));
    assert_eq!(unsettled["swaps"].as_array().unwrap().len(), 2, "{alone}");
    let blocker = unsettled["blockers"][0].as_str().unwrap();
    assert!(blocker.contains("add the spec of its run"), "{blocker}");
    // With every spec, both heads are settled, and nothing blocks.
    let all = audit(&c, &[&spec_k, &spec_l, &spec_t]).await;
    let heads: Vec<&Value> = (all["unsettled"].as_array().unwrap().iter())
        .map(|u| &u["head"])
        .collect();
    assert_eq!(heads.len(), 2, "{all}");
    assert!(heads.contains(&&json!(id(&head_l))) && heads.contains(&&json!(id(&head_t))));
    assert!(
        (all["unsettled"].as_array().unwrap().iter()).all(|u| u.get("blockers").is_none()),
        "{all}"
    );

    // A name the lemma's head adds that the theorem's head declares too.
    let spec_l = create(
        &c,
        json!({"code": "import Foo\n\ntheorem lemma_c : 1 = 1 := sorry"}),
    )
    .await;
    let spec_t = create(
        &c,
        json!({"code": on(&spec_l, "theorem top_c : 2 = 2 := sorry")}),
    )
    .await;
    let code = on(
        &spec_l,
        "theorem top_c : 2 = 2 := by have := lemma_c; rfl\ntheorem helper : 4 = 4 := rfl",
    );
    create(&c, replacing(&spec_t, &code)).await;
    let code = "import Foo\n\ntheorem lemma_c : 1 = 1 := rfl\ntheorem helper : 3 = 3 := rfl";
    create(&c, replacing(&spec_l, code)).await;
    let r = audit(&c, &[&spec_l, &spec_t]).await;
    let blocker = r["unsettled"][0]["blockers"][0]
        .as_str()
        .unwrap_or_default();
    assert!(blocker.contains("declares helper"), "{r}");
}

async fn settle(c: &Cluster, specs: &[&Value], extra: Value) -> Value {
    let against: Vec<String> = specs.iter().map(|s| id(s)).collect();
    let mut body = json!({"against": against});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let (status, r) = c.post("/snapshots/settle", body).await;
    assert_eq!(status, 200, "{r}");
    r
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settle_rebuilds_the_heads_onto_what_they_hold_through() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (spec_l, spec_t, head_t) = top_down(&c).await;
    let head_l = create(
        &c,
        replacing(&spec_l, "import Foo\n\ntheorem lemma_l : 1 = 1 := rfl"),
    )
    .await;
    let plan = settle(&c, &[&spec_l, &spec_t], json!({"dry_run": true})).await;
    assert_eq!(plan["ok"], true, "{plan}");
    let item = &plan["plan"][0];
    assert_eq!(item["snapshot_id"], json!(id(&head_t)), "{plan}");
    assert_eq!(item["replaces"]["snapshot_id"], json!(id(&head_t)));
    let module_l = head_l["snapshot"]["module"].as_str().unwrap();
    assert!(item["code"].as_str().unwrap().contains(module_l), "{plan}");
    assert!(
        plan.get("created").is_none(),
        "a dry run creates nothing: {plan}"
    );

    let done = settle(&c, &[&spec_l, &spec_t], json!({})).await;
    assert_eq!(done["ok"], true, "{done}");
    let copy = done["created"][0]["new_snapshot_id"].clone();
    let after = audit(&c, &[&spec_l, &spec_t]).await;
    assert_eq!(after["clean"], true, "{after}");
    assert_eq!(target(&after, "top")["head"], copy);
    assert!(
        target(&after, "top").get("linked_status").is_none(),
        "{after}"
    );
    assert!(after.get("unsettled").is_none(), "{after}");
    // Settled, it has nothing left to do.
    let again = settle(&c, &[&spec_l, &spec_t], json!({})).await;
    assert_eq!(
        (&again["ok"], &again["plan"]),
        (&json!(true), &json!([])),
        "{again}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nested_settle_rebuilds_from_the_bottom_up() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec_k = create(
        &c,
        json!({"code": "import Foo\n\ntheorem lemma_k : 0 = 0 := sorry"}),
    )
    .await;
    let spec_l = create(
        &c,
        json!({"code": on(&spec_k, "theorem lemma_n : 1 = 1 := sorry")}),
    )
    .await;
    let spec_t = create(
        &c,
        json!({"code": on(&spec_l, "theorem top_n : 2 = 2 := sorry")}),
    )
    .await;
    let code = on(&spec_l, "theorem top_n : 2 = 2 := by have := lemma_n; rfl");
    let head_t = create(&c, replacing(&spec_t, &code)).await;
    let code = on(
        &spec_k,
        "theorem lemma_n : 1 = 1 := by have := lemma_k; rfl",
    );
    let head_l = create(&c, replacing(&spec_l, &code)).await;
    create(
        &c,
        replacing(&spec_k, "import Foo\n\ntheorem lemma_k : 0 = 0 := rfl"),
    )
    .await;
    let specs = [&spec_k, &spec_l, &spec_t];
    // As a job.
    let against: Vec<String> = specs.iter().map(|s| id(s)).collect();
    let r = c
        .client
        .post(c.url("/snapshots/settle"))
        .header("prefer", "respond-async")
        .json(&json!({"against": against}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let job: Value = r.json().await.unwrap();
    assert_eq!(job["kind"], "rebuild_snapshots");
    let path = format!("/jobs/{}?wait=30", job["id"].as_str().unwrap());
    let (status, done) = c.request(reqwest::Method::GET, &path, None).await;
    let (status, done) = common::job_answer(status, done);
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["ok"], true, "{done}");
    let level = |head: &Value| {
        let plan = done["plan"].as_array().unwrap();
        let item = plan
            .iter()
            .find(|i| i["snapshot_id"] == json!(id(head)))
            .unwrap();
        item["level"].as_u64().unwrap()
    };
    assert!(level(&head_l) < level(&head_t), "{done}");
    let after = audit(&c, &specs).await;
    assert_eq!(after["clean"], true, "{after}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settle_with_blockers_is_refused() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec_l = create(
        &c,
        json!({"code": "import Foo\n\ntheorem lemma_c : 1 = 1 := sorry"}),
    )
    .await;
    let spec_t = create(
        &c,
        json!({"code": on(&spec_l, "theorem top_c : 2 = 2 := sorry")}),
    )
    .await;
    let code = on(
        &spec_l,
        "theorem top_c : 2 = 2 := by have := lemma_c; rfl\ntheorem helper : 4 = 4 := rfl",
    );
    create(&c, replacing(&spec_t, &code)).await;
    let code = "import Foo\n\ntheorem lemma_c : 1 = 1 := rfl\ntheorem helper : 3 = 3 := rfl";
    create(&c, replacing(&spec_l, code)).await;
    let r = settle(&c, &[&spec_l, &spec_t], json!({})).await;
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        r["changes"][0]
            .as_str()
            .unwrap()
            .contains("declares helper"),
        "{r}"
    );
    assert!(r.get("created").is_none(), "{r}");
    let (status, r) = c.post("/snapshots/settle", json!({"against": []})).await;
    assert_eq!(status, 400, "{r}");
}
