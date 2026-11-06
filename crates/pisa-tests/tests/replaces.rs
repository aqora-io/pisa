//! Replacing a snapshot: a create that names the snapshot it replaces is kept
//! only if it keeps that snapshot's statements, and the new snapshot records
//! the link.
//! fake-lean's "type hash" is a declaration's text between its name and
//! `:=`, and a `def`'s "value hash" the text after it.

mod common;

use common::Cluster;
use serde_json::{Value, json};

const SKELETON: &str = "import Foo\n\ntheorem t : 1 = 1 := sorry";
const PROOF: &str = "import Foo\n\ntheorem t : 1 = 1 := rfl";

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    r
}

fn entry(r: &Value) -> &str {
    r["snapshot"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no snapshot: {r}"))
}

/// What `replaces` takes for the snapshot `id`.
fn replacing(id: &str) -> Value {
    json!({"snapshot_id": id})
}

async fn get(c: &Cluster, path: &str) -> Value {
    let r = c.client.get(c.url(path)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

/// Asserts that a create kept nothing because of `reason`.
fn refused(r: &Value, reason: &str) {
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(r["check"]["ok"], false, "{r}");
    let reasons = r["check"]["reasons"].as_array().unwrap();
    assert!(reasons.contains(&json!(reason)), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proof_replaces_its_skeleton_and_the_link_is_kept() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let skeleton = create(&c, json!({"code": SKELETON})).await;
    assert_eq!(skeleton["snapshot"]["status"], "sorry", "{skeleton}");
    let old = entry(&skeleton);

    let proof = create(&c, json!({"code": PROOF, "replaces": replacing(old)})).await;
    assert_eq!(proof["snapshot"]["status"], "proved", "{proof}");
    assert_eq!(proof["replacement"], json!({"filled": ["t"]}));
    let link = replacing(old);
    assert_eq!(proof["snapshot"]["replaces"], link);
    let info = get(&c, &format!("/snapshots/{}", entry(&proof))).await;
    assert_eq!(info["replaces"], link);
    let listed = get(&c, "/snapshots").await;
    let listed = |id: &str| {
        let snapshots = listed["snapshots"].as_array().unwrap();
        snapshots.iter().find(|s| s["id"] == id).unwrap().clone()
    };
    assert_eq!(listed(entry(&proof))["replaces"], link);
    assert!(listed(old).get("replaces").is_none());

    // A replacement may add declarations, and still leave a `sorry`.
    let code = "import Foo\n\ntheorem helper : 2 = 2 := sorry\ntheorem t : 1 = 1 := sorry";
    let partial = create(&c, json!({"code": code, "replaces": replacing(old)})).await;
    assert_eq!(partial["replacement"], json!({"filled": []}));
    assert_eq!(partial["snapshot"]["status"], "sorry");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_statement_is_not_kept() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let weakened = "import Foo\n\ntheorem t : True := trivial";
    let r = create(&c, json!({"code": weakened, "replaces": replacing(&old)})).await;
    refused(&r, "statement: t has a different type");
    let r = create(
        &c,
        json!({"code": "import Foo\n\ntheorem other : 1 = 1 := rfl", "replaces": replacing(&old)}),
    )
    .await;
    refused(&r, "statement: t is missing");

    // Code that is kept already gets no new snapshot either.
    create(&c, json!({"code": weakened})).await;
    let r = create(&c, json!({"code": weakened, "replaces": replacing(&old)})).await;
    refused(&r, "statement: t has a different type");
    let listed = get(&c, "/snapshots").await;
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), 2, "{listed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_definition_changes_only_when_allowed_and_its_users_are_listed() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let skeleton =
        "import Foo\n\ndef f : Nat := 1\ntheorem t : f = f := sorry\ntheorem u : 1 = 1 := sorry";
    let old = entry(&create(&c, json!({"code": skeleton})).await).to_string();
    let fixed =
        "import Foo\n\ndef f : Nat := 2\ntheorem t : f = f := rfl\ntheorem u : 1 = 1 := rfl";

    let r = create(&c, json!({"code": fixed, "replaces": replacing(&old)})).await;
    refused(&r, "statement: f has a different value");

    let replaces = json!({"snapshot_id": old, "allow": "definitions"});
    let r = create(&c, json!({"code": fixed, "replaces": replaces})).await;
    assert_eq!(
        r["replacement"],
        json!({"filled": ["t", "u"], "changed_definitions": ["f"], "affected": ["t"]}),
        "{r}"
    );
    assert_eq!(r["snapshot"]["replaces"], replaces);
    let info = get(&c, &format!("/snapshots/{}", entry(&r))).await;
    assert_eq!(info["replaces"], replaces);

    // A definition's type stays the same.
    let retyped =
        "import Foo\n\ndef f : Int := 2\ntheorem t : f = f := rfl\ntheorem u : 1 = 1 := rfl";
    let r = create(&c, json!({"code": retyped, "replaces": replaces})).await;
    refused(&r, "statement: f has a different type");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_snapshots_are_errors_and_key_replays_skip_the_comparison() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let unknown = format!("{}~{}", c.version("v"), "0".repeat(32));
    let r = create(&c, json!({"code": PROOF, "replaces": replacing(&unknown)})).await;
    assert_eq!(r["check"]["error"]["kind"], "not_found", "{r}");
    assert!(r.get("snapshot").is_none(), "{r}");
    // Not snapshot ids, and a bare id is not a `replaces`.
    for replaces in [
        replacing("nope"),
        replacing(&"0".repeat(32)),
        json!("0".repeat(32)),
    ] {
        let (status, r) = c
            .post("/snapshots", json!({"code": PROOF, "replaces": replaces}))
            .await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{replaces}: {r}"
        );
    }

    let weakened = "import Foo\n\ntheorem t : True := trivial";
    let first = create(&c, json!({"code": weakened, "key": "k"})).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let body = json!({"code": weakened, "key": "k", "replaces": replacing(&old)});
    let replay = create(&c, body).await;
    assert_eq!(replay["snapshot"], first["snapshot"], "{replay}");
    assert!(replay.get("replacement").is_none(), "{replay}");
}

/// `header`, a blank line, then `body`.
fn on(snapshot: &Value, body: &str) -> String {
    format!(
        "{}\n\n{body}",
        snapshot["snapshot"]["header"].as_str().unwrap()
    )
}

fn module(r: &Value) -> &str {
    r["snapshot"]["module"].as_str().unwrap()
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
    let r = create(&c, json!({"code": proof, "replaces": replacing(&old)})).await;
    let lost = module(&a);
    refused(
        &r,
        &format!("statement: foo, from {lost}, has a different value"),
    );
    let replaces = json!({"snapshot_id": old, "allow": "definitions"});
    let r = create(&c, json!({"code": proof, "replaces": replaces})).await;
    assert_eq!(
        r["replacement"]["changed_definitions"],
        json!(["foo"]),
        "{r}"
    );
    assert_eq!(r["replacement"]["affected"], json!(["thm3"]));

    // A snapshot with the same `foo` and more is fine.
    let more = create(
        &c,
        json!({"code": "import Foo\n\ndef foo : Nat := 1\ntheorem extra : 2 = 2 := rfl"}),
    )
    .await;
    let r = create(
        &c,
        json!({"code": on(&more, "theorem thm3 : foo = 1 := rfl"), "replaces": replacing(&old)}),
    )
    .await;
    assert_eq!(r["replacement"], json!({"filled": ["thm3"]}), "{r}");

    // So is building on more.
    let helper = create(
        &c,
        json!({"code": "import Foo\n\ntheorem helper : 2 = 2 := rfl"}),
    )
    .await;
    let both = format!(
        "import Foo\nimport {lost}\nimport {}\n\ntheorem thm3 : foo = 1 := rfl",
        module(&helper)
    );
    let r = create(&c, json!({"code": both, "replaces": replacing(&old)})).await;
    assert_eq!(r["replacement"]["filled"], json!(["thm3"]), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replaced_id_of_another_version_is_a_bad_request() {
    let c = Cluster::start(&[("w0", "va", 1), ("w1", "vb", 1)]).await;
    let skeleton = create(&c, json!({"code": SKELETON, "version": "va"})).await;
    let body = json!({"code": PROOF, "version": "vb", "replaces": replacing(entry(&skeleton))});
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(
        (status, &r["error"]["kind"]),
        (400, &json!("bad_request")),
        "{r}"
    );
    let message = r["error"]["message"].as_str().unwrap();
    for v in [c.version("va"), c.version("vb")] {
        assert!(message.contains(&v), "{v}: {message}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_link_outlives_the_replaced_snapshot() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let body = json!({"code": PROOF, "replaces": replacing(&old)});
    let new = entry(&create(&c, body).await).to_string();
    let r = c
        .client
        .delete(c.url(&format!("/snapshots/{old}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let info = get(&c, &format!("/snapshots/{new}")).await;
    assert_eq!(info["replaces"], replacing(&old));
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
        json!({"code": "import Foo\n\naxiom lem2 : 1 = 1", "replaces": replacing(&old)}),
    )
    .await;
    refused(&r, "statement: lem2 is a different kind of declaration");
    refused(&r, "statement: the code adds the assumption lem2");
    // An assumption of its own.
    let own = "import Foo\n\naxiom helper : 1 = 1\ntheorem lem2 : 1 = 1 := helper";
    let r = create(&c, json!({"code": own, "replaces": replacing(&old)})).await;
    refused(&r, "statement: the code adds the assumption helper");
    // Or of a fresh snapshot it now builds on.
    let fresh = create(&c, json!({"code": "import Foo\n\naxiom helper2 : 1 = 1"})).await;
    let code = on(&fresh, "theorem lem2 : 1 = 1 := helper2");
    let r = create(&c, json!({"code": code, "replaces": replacing(&old)})).await;
    refused(&r, "statement: the code adds the assumption helper2");

    // A skeleton's own assumptions may stay, as they were.
    let with = "import Foo\n\naxiom kun : 1 = 1\ntheorem t : 1 = 1 := sorry";
    let old = entry(&create(&c, json!({"code": with})).await).to_string();
    let proof = "import Foo\n\naxiom kun : 1 = 1\ntheorem t : 1 = 1 := kun";
    let r = create(&c, json!({"code": proof, "replaces": replacing(&old)})).await;
    assert_eq!(r["replacement"]["filled"], json!(["t"]), "{r}");
    assert_eq!(r["snapshot"]["status"], "conditional");
    let stronger = "import Foo\n\naxiom kun : 2 = 2\ntheorem t : 1 = 1 := kun";
    let r = create(&c, json!({"code": stronger, "replaces": replacing(&old)})).await;
    refused(&r, "statement: kun has a different type");
}
