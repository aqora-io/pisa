//! Replacing an entry: a create that names the entry it replaces is kept only
//! if it keeps that entry's statements, and the new entry records the link.
//! fake-lean's "type hash" is a declaration's text between its name and
//! `:=`, and a `def`'s "value hash" the text after it.

mod common;

use common::Cluster;
use pisa_worker::catalog::Catalog;
use serde_json::{Value, json};

const SKELETON: &str = "import Foo\n\ntheorem t : 1 = 1 := sorry";
const PROOF: &str = "import Foo\n\ntheorem t : 1 = 1 := rfl";

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    r
}

fn entry(r: &Value) -> &str {
    r["entry"]
        .as_str()
        .unwrap_or_else(|| panic!("no entry: {r}"))
}

async fn get(c: &Cluster, path: &str) -> Value {
    let r = c.client.get(c.url(path)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

/// Asserts that a create kept nothing because of `reason`.
fn refused(r: &Value, reason: &str) {
    assert!(r["entry"].is_null(), "{r}");
    assert_eq!(r["result"]["ok"], false, "{r}");
    let reasons = r["result"]["reasons"].as_array().unwrap();
    assert!(reasons.contains(&json!(reason)), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proof_replaces_its_skeleton_and_the_link_is_kept() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let skeleton = create(&c, json!({"code": SKELETON})).await;
    assert_eq!(skeleton["dirty"], true, "{skeleton}");
    let old = entry(&skeleton);

    let proof = create(&c, json!({"code": PROOF, "replaces": old})).await;
    assert_eq!(proof["dirty"], false, "{proof}");
    assert_eq!(proof["replaces"], json!({"entry": old, "filled": ["t"]}));
    let link = json!({"entry": old});
    let info = get(&c, &format!("/snapshots/{}", entry(&proof))).await;
    assert_eq!(info["replaces"], link);
    let listed = get(&c, "/snapshots").await;
    let summary = |e: &str| {
        let entries = listed["entries"].as_array().unwrap();
        entries.iter().find(|s| s["entry"] == e).unwrap().clone()
    };
    assert_eq!(summary(entry(&proof))["replaces"], link);
    assert!(summary(old).get("replaces").is_none());

    // A replacement may add declarations, and still leave a `sorry`.
    let code = "import Foo\n\ntheorem helper : 2 = 2 := sorry\ntheorem t : 1 = 1 := sorry";
    let partial = create(&c, json!({"code": code, "replaces": old})).await;
    assert_eq!(partial["replaces"], json!({"entry": old, "filled": []}));
    assert_eq!(partial["dirty"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_statement_is_not_kept() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let weakened = "import Foo\n\ntheorem t : True := trivial";
    let r = create(&c, json!({"code": weakened, "replaces": old})).await;
    refused(&r, "statement: t has a different type");
    assert!(r["snapshot"].is_null(), "the new code isn't kept: {r}");
    let r = create(
        &c,
        json!({"code": "import Foo\n\ntheorem other : 1 = 1 := rfl", "replaces": old}),
    )
    .await;
    refused(&r, "statement: t is missing");

    // Code that is kept already gets no entry either.
    let kept = create(&c, json!({"code": weakened})).await;
    let r = create(&c, json!({"code": weakened, "replaces": old})).await;
    refused(&r, "statement: t has a different type");
    assert_eq!(r["snapshot"], kept["snapshot"]);
    let listed = get(&c, "/snapshots").await;
    assert_eq!(listed["entries"].as_array().unwrap().len(), 2, "{listed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_definition_changes_only_when_allowed_and_its_users_are_listed() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let skeleton =
        "import Foo\n\ndef f : Nat := 1\ntheorem t : f = f := sorry\ntheorem u : 1 = 1 := sorry";
    let old = entry(&create(&c, json!({"code": skeleton})).await).to_string();
    let fixed =
        "import Foo\n\ndef f : Nat := 2\ntheorem t : f = f := rfl\ntheorem u : 1 = 1 := rfl";

    let r = create(&c, json!({"code": fixed, "replaces": old})).await;
    refused(&r, "statement: f has a different value");

    let replaces = json!({"entry": old, "allow": "definitions"});
    let r = create(&c, json!({"code": fixed, "replaces": replaces})).await;
    assert_eq!(
        r["replaces"],
        json!({"entry": old, "allow": "definitions", "filled": ["t", "u"],
               "changed_definitions": ["f"], "affected": ["t"]}),
        "{r}"
    );
    let info = get(&c, &format!("/snapshots/{}", entry(&r))).await;
    assert_eq!(info["replaces"], replaces);

    // A definition's type stays the same.
    let retyped =
        "import Foo\n\ndef f : Int := 2\ntheorem t : f = f := rfl\ntheorem u : 1 = 1 := rfl";
    let r = create(&c, json!({"code": retyped, "replaces": replaces})).await;
    refused(&r, "statement: f has a different type");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_entries_are_errors_and_key_replays_skip_the_comparison() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for (replaces, kind) in [
        ("0".repeat(32), "not_found"),
        ("nope".into(), "bad_request"),
    ] {
        let r = create(&c, json!({"code": PROOF, "replaces": replaces})).await;
        assert_eq!(r["result"]["error"]["kind"], kind, "{r}");
        assert!(r["entry"].is_null() && r["snapshot"].is_null(), "{r}");
    }

    let weakened = "import Foo\n\ntheorem t : True := trivial";
    let first = create(&c, json!({"code": weakened, "key": "k"})).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let replay = create(&c, json!({"code": weakened, "key": "k", "replaces": old})).await;
    assert_eq!(replay["entry"], first["entry"], "{replay}");
    assert!(replay.get("replaces").is_none(), "{replay}");
}

/// `header`, a blank line, then `body`.
fn on(snapshot: &Value, body: &str) -> String {
    format!("{}\n\n{body}", snapshot["header"].as_str().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replacement_on_other_snapshots_keeps_what_its_statements_use() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let a = create(&c, json!({"code": "import Foo\n\ndef foo : Nat := 1"})).await;
    let skeleton = create(
        &c,
        json!({"code": on(&a, "theorem thm3 : foo = 1 := sorry")}),
    )
    .await;
    let old = entry(&skeleton).to_string();

    // `thm3`'s type names `foo` the same way on another `foo`, so the
    // comparison follows it into the snapshot the code no longer builds on.
    let other = create(&c, json!({"code": "import Foo\n\ndef foo : Nat := 2"})).await;
    let proof = on(&other, "theorem thm3 : foo = 1 := rfl");
    let r = create(&c, json!({"code": proof, "replaces": old})).await;
    let lost = a["snapshot"].as_str().unwrap();
    refused(
        &r,
        &format!("statement: foo, from {lost}, has a different value"),
    );
    let replaces = json!({"entry": old, "allow": "definitions"});
    let r = create(&c, json!({"code": proof, "replaces": replaces})).await;
    assert_eq!(r["replaces"]["changed_definitions"], json!(["foo"]), "{r}");
    assert_eq!(r["replaces"]["affected"], json!(["thm3"]));

    // A snapshot with the same `foo` and more is fine.
    let more = create(
        &c,
        json!({"code": "import Foo\n\ndef foo : Nat := 1\ntheorem extra : 2 = 2 := rfl"}),
    )
    .await;
    let r = create(
        &c,
        json!({"code": on(&more, "theorem thm3 : foo = 1 := rfl"), "replaces": old}),
    )
    .await;
    assert_eq!(
        r["replaces"],
        json!({"entry": old, "filled": ["thm3"]}),
        "{r}"
    );

    // So is building on more.
    let helper = create(
        &c,
        json!({"code": "import Foo\n\ntheorem helper : 2 = 2 := rfl"}),
    )
    .await;
    let both = format!(
        "import Foo\nimport {lost}\nimport {}\n\ntheorem thm3 : foo = 1 := rfl",
        helper["snapshot"].as_str().unwrap()
    );
    let r = create(&c, json!({"code": both, "replaces": old})).await;
    assert_eq!(r["replaces"]["filled"], json!(["thm3"]), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_that_doesnt_say_what_it_uses_is_verified_to_say_it() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let a = create(&c, json!({"code": "import Foo\n\ndef foo : Nat := 1"})).await;
    let skeleton = create(
        &c,
        json!({"code": on(&a, "theorem thm3 : foo = 1 := sorry")}),
    )
    .await;
    let old = entry(&skeleton).to_string();
    // As a module kept before records said what declarations use, verified.
    let id = &skeleton["snapshot"].as_str().unwrap()["PisaSnap.s".len()..];
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let build = catalog
        .module(id)
        .await
        .unwrap()
        .modules
        .last()
        .unwrap()
        .build
        .clone();
    let stripped = catalog.update_module(id, &build, |meta| {
        meta.verified = true;
        for d in &mut meta.result.declarations {
            (d.type_refs, d.value_refs) = (None, None);
        }
    });
    assert!(stripped.await.unwrap().is_some());

    let more = create(
        &c,
        json!({"code": "import Foo\n\ndef foo : Nat := 1\ntheorem extra : 2 = 2 := rfl"}),
    )
    .await;
    let proof = on(&more, "theorem thm3 : foo = 1 := rfl");
    let r = create(&c, json!({"code": proof, "replaces": old})).await;
    assert!(r["entry"].is_null(), "{r}");
    let reasons = r["result"]["reasons"].to_string();
    assert!(reasons.contains("verify it to record them"), "{r}");

    let (status, verified) = c.post(&format!("/snapshots/{old}/verify"), json!({})).await;
    assert_eq!(status, 200, "{verified}");
    assert!(
        verified["checked"]
            .as_array()
            .unwrap()
            .contains(&skeleton["snapshot"]),
        "{verified}"
    );
    let r = create(&c, json!({"code": proof, "replaces": old})).await;
    assert_eq!(r["replaces"]["filled"], json!(["thm3"]), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_link_outlives_the_replaced_entry() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let new = entry(&create(&c, json!({"code": PROOF, "replaces": old})).await).to_string();
    let r = c
        .client
        .delete(c.url(&format!("/snapshots/{old}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let info = get(&c, &format!("/snapshots/{new}")).await;
    assert_eq!(info["replaces"], json!({"entry": old}));
    let r = c
        .client
        .get(c.url(&format!("/snapshots/{old}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replacement_cant_assume_what_it_should_prove() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let skeleton = "import Foo\n\ntheorem lem2 : 1 = 1 := sorry";
    let old = entry(&create(&c, json!({"code": skeleton})).await).to_string();

    // The theorem itself, assumed: same name, same type.
    let r = create(
        &c,
        json!({"code": "import Foo\n\naxiom lem2 : 1 = 1", "replaces": old}),
    )
    .await;
    refused(&r, "statement: lem2 is a different kind of declaration");
    refused(&r, "statement: the code adds the assumption lem2");
    // An assumption of its own.
    let own = "import Foo\n\naxiom helper : 1 = 1\ntheorem lem2 : 1 = 1 := helper";
    let r = create(&c, json!({"code": own, "replaces": old})).await;
    refused(&r, "statement: the code adds the assumption helper");
    // Or of a fresh snapshot it now builds on.
    let fresh = create(&c, json!({"code": "import Foo\n\naxiom helper2 : 1 = 1"})).await;
    let code = on(&fresh, "theorem lem2 : 1 = 1 := helper2");
    let r = create(&c, json!({"code": code, "replaces": old})).await;
    refused(&r, "statement: the code adds the assumption helper2");

    // A skeleton's own assumptions may stay, as they were.
    let with = "import Foo\n\naxiom kun : 1 = 1\ntheorem t : 1 = 1 := sorry";
    let old = entry(&create(&c, json!({"code": with})).await).to_string();
    let proof = "import Foo\n\naxiom kun : 1 = 1\ntheorem t : 1 = 1 := kun";
    let r = create(&c, json!({"code": proof, "replaces": old})).await;
    assert_eq!(r["replaces"]["filled"], json!(["t"]), "{r}");
    assert_eq!(r["status"], "conditional");
    let stronger = "import Foo\n\naxiom kun : 2 = 2\ntheorem t : 1 = 1 := kun";
    let r = create(&c, json!({"code": stronger, "replaces": old})).await;
    refused(&r, "statement: kun has a different type");
}
