//! Replacing a snapshot: a create that names the snapshot it replaces is kept
//! only if it keeps that snapshot's statements, and the new snapshot records
//! the link.
//! The fake Lean's "type hash" is a declaration's text between its name and
//! `:=`, and a `def`'s "value hash" the text after it.

mod common;

use common::{Cluster, create};
use pisa_protocol::api::Replacement;
use pisa_worker::catalog::Catalog;
use serde_json::{Value, json};

const SKELETON: &str = "import Foo\n\ntheorem t : 1 = 1 := sorry";
const PROOF: &str = "import Foo\n\ntheorem t : 1 = 1 := rfl";

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
    let body = json!({"code": code, "replaces": replacing(old), "fork": true});
    let partial = create(&c, body).await;
    assert_eq!(partial["replacement"], json!({"filled": []}));
    assert_eq!(partial["snapshot"]["status"], "sorry");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_says_what_changed_and_how() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let retyped = "import Foo\n\ntheorem t : 2 = 2 := rfl";
    let r = create(&c, json!({"code": retyped, "replaces": replacing(&old)})).await;
    // `was` and `now` are the signatures the modules' exports show.
    assert_eq!(
        r["check"]["changes"],
        json!([{"name": "t", "change": "type", "was": "t : 1 = 1", "now": "t : 2 = 2"}]),
        "{r}"
    );
    assert_eq!(
        r["check"]["reasons"],
        json!(["statement: t has a different type"])
    );

    let own = "import Foo\n\naxiom helper : 1 = 1\ntheorem t : 1 = 1 := helper";
    let r = create(&c, json!({"code": own, "replaces": replacing(&old)})).await;
    assert_eq!(
        r["check"]["changes"],
        json!([{"name": "helper", "change": "added_assumption"}]),
        "{r}"
    );

    // A constant of a snapshot the code no longer builds on names it.
    let a = create(&c, json!({"code": "import Foo\n\ndef foo : Nat := 1"})).await;
    let skeleton = create(&c, json!({"code": on(&a, "theorem s : foo = 1 := sorry")})).await;
    let other = create(&c, json!({"code": "import Foo\n\ndef foo : Nat := 2"})).await;
    let proof = on(&other, "theorem s : foo = 1 := rfl");
    let r = create(
        &c,
        json!({"code": proof, "replaces": replacing(entry(&skeleton))}),
    )
    .await;
    assert_eq!(
        r["check"]["changes"],
        json!([{"name": "foo", "module": module(&a), "change": "value"}]),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_type_that_prints_alike_is_shown_with_every_argument() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    // Prints as the spec's statement; differs where the printer hides it, as
    // an instance found by another path does. The fake's types start at `:`.
    let hidden =
        "import Foo\n\n-- printed-as: theorem t : 1 = 1 := rfl\ntheorem t : 1 = (1 : Nat) := rfl";
    let r = create(&c, json!({"code": hidden, "replaces": replacing(&old)})).await;
    assert_eq!(
        r["check"]["changes"],
        json!([{"name": "t", "change": "type", "was": "t : 1 = 1", "now": "t : 1 = 1",
                "was_explicit": ": 1 = 1", "now_explicit": ": 1 = (1 : Nat)"}]),
        "{r}"
    );
    assert_eq!(
        r["check"]["reasons"],
        json!([
            "statement: t has a different type; it prints alike, but with every argument \
                shown it was `: 1 = 1` and is now `: 1 = (1 : Nat)`"
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_type_alike_as_far_as_the_export_prints_says_the_difference_is_further() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let long = vec!["1 = 1"; 1000].join(" ∧ ");
    let spec = format!("import Foo\n\ntheorem t : {long} := sorry");
    let old = entry(&create(&c, json!({"code": spec})).await).to_string();
    // Differs only past the characters the export prints of the type with
    // every argument shown.
    let hidden = format!(
        "import Foo\n\n-- printed-as: theorem t : {long} := rfl\ntheorem t : {long} ∧ True := rfl"
    );
    let r = create(&c, json!({"code": hidden, "replaces": replacing(&old)})).await;
    let change = &r["check"]["changes"][0];
    assert_eq!(change["change"], "type", "{r}");
    assert_eq!(change["explicit_alike_chars"], 4000, "{r}");
    assert!(change.get("was_explicit").is_none(), "{r}");
    assert!(
        r["check"]["reasons"][0]
            .as_str()
            .unwrap()
            .ends_with("for the first 4000 characters, which is as far as the exports print it: the types differ after that"),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_against_a_spec_says_which_statements_it_changes() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = "import Foo\n\ndef f : Nat := 1\ntheorem t : 1 = 1 := sorry";
    let spec = entry(&create(&c, json!({"code": spec})).await).to_string();
    // The same statements, with a helper and another value for `f`, which
    // only the create judges, with the spec's holes and `allow`.
    let kept =
        "import Foo\n\ndef f : Nat := 2\ntheorem helper : 2 = 2 := rfl\ntheorem t : 1 = 1 := rfl";
    let (status, r) = c
        .post("/check", json!({"code": kept, "against": spec}))
        .await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["ok"], true, "{r}");
    assert!(r.get("changes").is_none(), "{r}");

    let retyped = "import Foo\n\ntheorem t : 2 = 2 := rfl";
    let (status, r) = c
        .post("/check", json!({"code": retyped, "against": spec}))
        .await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(
        r["changes"],
        json!([{"name": "t", "change": "type", "was": "t : 1 = 1"}]),
        "{r}"
    );
    assert_eq!(
        r["reasons"],
        json!(["statement: t has a different type"]),
        "{r}"
    );

    let version = spec.split_once('~').unwrap().0;
    let unknown = format!("{version}~{}", "0".repeat(32));
    let (status, r) = c
        .post("/check", json!({"code": retyped, "against": unknown}))
        .await;
    assert_eq!(status, 404, "{r}");
    let (status, r) = c
        .post("/check", json!({"code": retyped, "against": "nonsense"}))
        .await;
    assert_eq!(status, 400, "{r}");
    let (status, r) = c
        .post(
            "/check",
            json!({"code": retyped, "against": spec, "version": "other"}),
        )
        .await;
    assert_eq!(status, 400, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_may_name_a_structure_whose_fields_change() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let skeleton = "import Foo\n\nstructure S where x : Nat\ntheorem t (s : S) : s = s := sorry";
    let old = entry(&create(&c, json!({"code": skeleton})).await).to_string();
    let wider = "import Foo\n\nstructure S where x : Nat y : Nat\ntheorem t (s : S) : s = s := rfl";
    let r = create(&c, json!({"code": wider, "replaces": replacing(&old)})).await;
    refused(
        &r,
        "statement: S has different constructors or what Lean makes from them (S.mk)",
    );
    assert_eq!(
        r["check"]["changes"],
        json!([{"name": "S", "change": "derived", "includes": ["S.mk"]}]),
        "{r}"
    );
    let replaces = json!({"snapshot_id": old, "allow": {"definitions": ["S"]}});
    let r = create(&c, json!({"code": wider, "replaces": replaces})).await;
    assert_eq!(
        r["replacement"],
        // What changed under it is listed after it, for review.
        json!({"filled": ["t"], "changed_definitions": ["S", "S.mk"], "affected": ["t"]}),
        "{r}"
    );
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

    // An import that declares the spec's `t` identically doesn't stand in
    // for it, and the refusal says where it is from.
    let helper = create(&c, json!({"code": PROOF})).await;
    let code = on(&helper, "theorem other : 1 = 1 := rfl");
    let r = create(&c, json!({"code": code, "replaces": replacing(&old)})).await;
    assert_eq!(r["check"]["changes"][0]["name"], "t", "{r}");
    assert_eq!(r["check"]["changes"][0]["change"], "missing", "{r}");
    assert_eq!(
        r["check"]["changes"][0]["imported_from"],
        module(&helper),
        "{r}"
    );
    let reason = r["check"]["reasons"][0].as_str().unwrap();
    assert!(reason.starts_with("statement: t is missing; `"), "{r}");
    assert!(reason.contains("Reusing a proof"), "{r}");

    // Code that is kept already gets no new snapshot either.
    create(&c, json!({"code": weakened})).await;
    let r = create(&c, json!({"code": weakened, "replaces": replacing(&old)})).await;
    refused(&r, "statement: t has a different type");
    let listed = get(&c, "/snapshots").await;
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), 3, "{listed}");
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

    let replaces = json!({"snapshot_id": old, "allow": {"definitions": ["f"]}});
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
    let r = create(
        &c,
        json!({"code": retyped, "replaces": replaces, "fork": true}),
    )
    .await;
    refused(&r, "statement: f has a different type");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_named_definitions_may_change() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let skeleton = "import Foo\n\ndef f : Nat := 1\ndef g : Nat := 1\ntheorem t : f = g := sorry";
    let old = entry(&create(&c, json!({"code": skeleton})).await).to_string();
    let replaces = json!({"snapshot_id": old, "allow": {"definitions": ["f"]}});

    let f = "import Foo\n\ndef f : Nat := 2\ndef g : Nat := 1\ntheorem t : f = g := sorry";
    let r = create(&c, json!({"code": f, "replaces": replaces})).await;
    assert_eq!(r["replacement"]["changed_definitions"], json!(["f"]), "{r}");
    assert_eq!(r["snapshot"]["replaces"], replaces);

    let g = "import Foo\n\ndef f : Nat := 1\ndef g : Nat := 2\ntheorem t : f = g := sorry";
    let r = create(&c, json!({"code": g, "replaces": replaces, "fork": true})).await;
    refused(&r, "statement: g has a different value");
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_must_name_a_definition() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let skeleton = "import Foo\n\ndef f : Nat := 1\ntheorem t : f = f := sorry";
    let old = entry(&create(&c, json!({"code": skeleton})).await).to_string();
    let fixed = "import Foo\n\ndef f : Nat := 2\ntheorem t : f = f := rfl";
    let allowing = |allow: Value| json!({"snapshot_id": old, "allow": allow});

    // An empty list, and the bare form, aren't requests.
    for allow in [json!({"definitions": []}), json!("definitions")] {
        let body = json!({"code": fixed, "replaces": allowing(allow.clone())});
        let (status, r) = c.post("/snapshots", body).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{allow}: {r}"
        );
    }

    // A name that isn't a definition, such as a theorem's, keeps nothing.
    let unknown = |r: &Value, name: &str| {
        assert!(r.get("snapshot").is_none(), "{r}");
        assert_eq!(r["check"]["error"]["kind"], "bad_request", "{r}");
        let message = format!(
            "allow names {name}, which isn't a definition or inductive type of the replaced snapshot or of a \
             snapshot the code no longer builds on"
        );
        assert_eq!(r["check"]["error"]["message"], message, "{r}");
    };
    for name in ["nope", "t"] {
        let replaces = allowing(json!({"definitions": ["f", name]}));
        let r = create(&c, json!({"code": fixed, "replaces": replaces})).await;
        unknown(&r, name);
    }
    // Code that is kept already gets no new snapshot either.
    create(&c, json!({"code": fixed})).await;
    let replaces = allowing(json!({"definitions": ["nope"]}));
    let r = create(&c, json!({"code": fixed, "replaces": replaces})).await;
    unknown(&r, "nope");
    let listed = get(&c, "/snapshots").await;
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), 2, "{listed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_misnamed_allow_builds_nothing() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    // CRASH would kill fake-lean: a build would answer `crash`.
    let code = format!("{PROOF}\nCRASH");
    let replaces = json!({"snapshot_id": old, "allow": {"definitions": ["nope"]}});
    let r = create(&c, json!({"code": code, "replaces": replaces})).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(r["check"]["error"]["kind"], "bad_request", "{r}");
    let message = r["check"]["error"]["message"].as_str().unwrap_or_default();
    assert!(message.starts_with("allow names nope, "), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_comparison_is_stored_with_the_link_for_new_code_and_kept_code() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let bare = |r: &Value| entry(r).split_once('~').unwrap().1.to_string();
    let skeleton =
        "import Foo\n\ndef f : Nat := 1\ntheorem t : f = f := sorry\ntheorem u : 1 = 1 := sorry";
    let fixed =
        "import Foo\n\ndef f : Nat := 2\ntheorem t : f = f := rfl\ntheorem u : 1 = 1 := rfl";
    let old = entry(&create(&c, json!({"code": skeleton})).await).to_string();
    let replaces = json!({"snapshot_id": old, "allow": {"definitions": ["f"]}});
    let comparison = Replacement {
        filled: vec!["t".into(), "u".into()],
        changed_definitions: vec!["f".into()],
        affected: vec!["t".into()],
    };

    // The code is new, so the create builds it.
    let built = create(&c, json!({"code": fixed, "replaces": replaces})).await;
    let stored = catalog.entry(&bare(&built)).await.unwrap();
    assert_eq!(stored.replacement, Some(comparison.clone()));
    let found = catalog
        .replacers(old.split_once('~').unwrap().1)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);

    // The code is kept now, so another create only adds an entry.
    let body = json!({"code": fixed, "replaces": replaces, "fork": true});
    let kept = create(&c, body).await;
    assert_ne!(entry(&kept), entry(&built));
    let stored = catalog.entry(&bare(&kept)).await.unwrap();
    assert_eq!(stored.replacement, Some(comparison));

    // An entry that replaces nothing has no comparison.
    let plain = create(&c, json!({"code": skeleton})).await;
    assert_eq!(
        catalog.entry(&bare(&plain)).await.unwrap().replacement,
        None
    );
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
    let replaces = json!({"snapshot_id": old, "allow": {"definitions": ["foo"]}});
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
        json!({"code": on(&more, "theorem thm3 : foo = 1 := rfl"), "replaces": replacing(&old),
               "fork": true}),
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
    let body = json!({"code": both, "replaces": replacing(&old), "fork": true});
    let r = create(&c, body).await;
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
async fn a_replacement_without_a_version_stays_on_the_replaced_snapshots_pool() {
    let mut cfg = common::router_config();
    cfg.default_version = Some("va".into());
    let c = Cluster::start_with(cfg, &[("w0", "va", 1), ("w1", "vb", 1)], |w| w).await;
    let (va, vb) = (c.version("va"), c.version("vb"));

    // Without `replaces`, the server's default applies.
    let plain = create(
        &c,
        json!({"code": "import Foo\n\ntheorem u : 2 = 2 := rfl"}),
    )
    .await;
    assert_eq!(plain["snapshot"]["version"], va, "{plain}");

    let skeleton = create(&c, json!({"code": SKELETON, "version": "vb"})).await;
    let old = entry(&skeleton);
    assert!(old.starts_with(&format!("{vb}~")), "{skeleton}");

    // The replaced snapshot's version beats the server's default.
    let proof = create(&c, json!({"code": PROOF, "replaces": replacing(old)})).await;
    assert_eq!(proof["snapshot"]["version"], vb, "{proof}");
    assert!(entry(&proof).starts_with(&format!("{vb}~")), "{proof}");
    assert_eq!(proof["snapshot"]["status"], "proved", "{proof}");
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
    let body = json!({"code": stronger, "replaces": replacing(&old), "fork": true});
    let r = create(&c, body).await;
    refused(&r, "statement: kun has a different type");
}

/// The entry id in a snapshot id.
fn bare(id: &str) -> &str {
    id.split_once('~').unwrap().1
}

async fn delete(c: &Cluster, id: &str) {
    let r = c.client.delete(c.url(&format!("/snapshots/{id}"))).send();
    assert_eq!(r.await.unwrap().status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn replacing_a_replaced_snapshot_is_a_conflict() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let p = entry(&create(&c, json!({"code": PROOF, "replaces": replacing(&s)})).await).to_string();
    let blobs = c.stored_blobs().await;

    // CRASH would kill fake-lean: a build would answer `crash`.
    let code = format!("{PROOF}\nCRASH");
    let r = create(&c, json!({"code": code, "replaces": replacing(&s)})).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(r["check"]["error"]["kind"], "conflict", "{r}");
    assert_eq!(r["heads"], json!([p]), "{r}");
    let message = r["check"]["error"]["message"].as_str().unwrap();
    assert_eq!(
        message,
        format!(
            "{s} is replaced by {p}; the run's heads from there are {p}. Replace a head, or set fork"
        )
    );
    assert_eq!(c.stored_blobs().await, blobs);
    let listed = get(&c, "/snapshots").await;
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), 2, "{listed}");

    // The head can be replaced.
    let code = "import Foo\n\ntheorem t : 1 = 1 := rfl\ntheorem u : 2 = 2 := rfl";
    let r = create(&c, json!({"code": code, "replaces": replacing(&p)})).await;
    assert_eq!(r["snapshot"]["replaces"], replacing(&p), "{r}");
    assert!(r.get("heads").is_none(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_heads_are_the_ends_of_the_run_from_the_replaced_snapshot() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let p = create(&c, json!({"code": PROOF, "replaces": replacing(&s)})).await;
    let p = entry(&p).to_string();
    // A fork at S, and a step above its older branch.
    let q = format!("{PROOF}\n-- q");
    let body = json!({"code": q, "replaces": replacing(&s), "fork": true});
    let q = entry(&create(&c, body).await).to_string();
    let p2 = format!("{PROOF}\n-- p2");
    let p2 = entry(&create(&c, json!({"code": p2, "replaces": replacing(&p)})).await).to_string();

    let r = create(&c, json!({"code": PROOF, "replaces": replacing(&s)})).await;
    assert_eq!(r["check"]["error"]["kind"], "conflict", "{r}");
    // Oldest first, each once.
    assert_eq!(r["heads"], json!([q, p2]), "{r}");
    let message = r["check"]["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("{s} is replaced by {p}, {q}; ")),
        "{r}"
    );
    let r = create(&c, json!({"code": PROOF, "replaces": replacing(&p)})).await;
    assert_eq!(r["heads"], json!([p2]), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_replaces_a_replaced_snapshot() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let s = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let p = entry(&create(&c, json!({"code": PROOF, "replaces": replacing(&s)})).await).to_string();

    let code = format!("{PROOF}\n-- another proof");
    let body = json!({"code": code, "replaces": replacing(&s), "fork": true});
    let q = create(&c, body).await;
    assert_eq!(q["snapshot"]["replaces"], replacing(&s), "{q}");
    assert!(q.get("heads").is_none(), "{q}");
    let found = catalog.replacers(bare(&s)).await.unwrap();
    let found: Vec<&str> = found.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(found, [bare(&p), bare(entry(&q))]);

    // `fork` is a request to skip the check, not a link: it is never stored.
    let info = get(&c, &format!("/snapshots/{}", entry(&q))).await;
    assert!(info.get("fork").is_none(), "{info}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_whose_replacer_was_deleted_can_be_replaced() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let p = entry(&create(&c, json!({"code": PROOF, "replaces": replacing(&s)})).await).to_string();
    delete(&c, &p).await;

    let code = format!("{PROOF}\n-- another proof");
    let r = create(&c, json!({"code": code, "replaces": replacing(&s)})).await;
    assert_eq!(r["snapshot"]["replaces"], replacing(&s), "{r}");
    assert!(r.get("heads").is_none(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_replay_comes_before_the_head_check() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let body = json!({"code": PROOF, "key": "k", "replaces": replacing(&s)});
    let p = create(&c, body.clone()).await;
    let code = format!("{PROOF}\n-- next");
    let next = json!({"code": code, "replaces": replacing(entry(&p))});
    create(&c, next).await;

    // S and P are both replaced now, and the key still returns P.
    let replay = create(&c, body).await;
    assert_eq!(replay["snapshot"], p["snapshot"], "{replay}");
    assert!(replay.get("heads").is_none(), "{replay}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replacement_landing_during_a_build_wins() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let s = entry(&create(&c, json!({"code": SKELETON})).await).to_string();

    let slow = format!("{PROOF}\n-- SLEEP 800");
    let building = tokio::spawn({
        let (client, url) = (c.client.clone(), c.url("/snapshots"));
        let body = json!({"code": slow, "replaces": replacing(&s)});
        async move {
            let r = client.post(url).json(&body).send().await.unwrap();
            r.json::<Value>().await.unwrap()
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let p = create(&c, json!({"code": PROOF, "replaces": replacing(&s)})).await;
    let p = entry(&p).to_string();
    // The slow build is still running, and has stored nothing.
    let blobs = c.stored_blobs().await;

    let r = building.await.unwrap();
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(r["check"]["error"]["kind"], "conflict", "{r}");
    assert_eq!(r["heads"], json!([p]), "{r}");
    let message = r["check"]["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("{s} is replaced by {p}; ")),
        "{r}"
    );
    assert!(
        message.ends_with(". The build is kept: the same code created again isn't built again"),
        "{r}"
    );
    // The build is kept with no entry, so nothing lists it, and the same
    // code created again isn't built again.
    let listed = get(&c, "/snapshots").await;
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), 2, "{listed}");
    let kept = c.stored_blobs().await;
    assert!(kept.len() > blobs.len(), "{kept:?}");
    let body = json!({"code": slow, "replaces": replacing(&s), "fork": true});
    let fork = create(&c, body).await;
    assert_eq!(fork["snapshot"]["status"], "proved", "{fork}");
    assert_eq!(c.stored_blobs().await, kept);
}

/// A create refused for what it replaces keeps its build with no entry:
/// nothing lists it, and the same code created again isn't built again.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_replacement_keeps_its_build_for_the_create_that_follows() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let old = entry(&create(&c, json!({"code": SKELETON})).await).to_string();
    let blobs = c.stored_blobs().await;
    let retyped = "import Foo\n\ntheorem t : 2 = 2 := rfl";
    let r = create(&c, json!({"code": retyped, "replaces": replacing(&old)})).await;
    refused(&r, "statement: t has a different type");
    let kept = c.stored_blobs().await;
    assert!(kept.len() > blobs.len(), "{kept:?}");
    let listed = get(&c, "/snapshots").await;
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), 1, "{listed}");

    // Sent again, it is refused the same way, from the kept record.
    let again = create(&c, json!({"code": retyped, "replaces": replacing(&old)})).await;
    refused(&again, "statement: t has a different type");
    assert_eq!(again["check"]["changes"], r["check"]["changes"], "{again}");
    // Without `replaces`, it is kept, and nothing is built.
    let plain = create(&c, json!({"code": retyped})).await;
    assert_eq!(plain["check"]["ok"], true, "{plain}");
    assert_eq!(plain["snapshot"]["status"], "proved", "{plain}");
    assert_eq!(c.stored_blobs().await, kept);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spec_leaves_its_holes_for_replacements_to_fill() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = "import Foo\n\ndef f : Nat := 1\ndef h : Nat := 1\ntheorem t : f = h := sorry";
    let s = create(&c, json!({"code": spec, "holes": ["f"]})).await;
    assert_eq!(s["snapshot"]["holes"], json!(["f"]), "{s}");
    // A hole changes without `allow`, and another definition doesn't.
    let both = "import Foo\n\ndef f : Nat := 2\ndef h : Nat := 2\ntheorem t : f = h := sorry";
    let r = create(&c, json!({"code": both, "replaces": replacing(entry(&s))})).await;
    refused(&r, "statement: h has a different value");
    let filled = "import Foo\n\ndef f : Nat := 2\ndef h : Nat := 1\ntheorem t : f = h := sorry";
    let p = create(
        &c,
        json!({"code": filled, "replaces": replacing(entry(&s))}),
    )
    .await;
    assert_eq!(p["replacement"]["changed_definitions"], json!(["f"]), "{p}");
    // What replaces it inherits them.
    assert_eq!(p["snapshot"]["holes"], json!(["f"]), "{p}");
    let again = "import Foo\n\ndef f : Nat := 3\ndef h : Nat := 1\ntheorem t : f = h := sorry";
    let q = create(&c, json!({"code": again, "replaces": replacing(entry(&p))})).await;
    assert_eq!(q["replacement"]["changed_definitions"], json!(["f"]), "{q}");
    let got = get(&c, &format!("/snapshots/{}", entry(&q))).await;
    assert_eq!(got["holes"], json!(["f"]), "{got}");
}

#[tokio::test(flavor = "multi_thread")]
async fn holes_name_definitions_of_the_spec_or_of_what_it_builds_on() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let b = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    let spec = on(&b, "def f : Nat := 1\ntheorem t : f = g := sorry");

    // An empty list isn't a request, and a replacement inherits its holes.
    let bodies = [
        json!({"code": spec, "holes": []}),
        json!({"code": spec, "holes": ["f"], "replaces": replacing(entry(&b))}),
    ];
    for body in bodies {
        let (status, r) = c.post("/snapshots", body).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{r}"
        );
    }

    // A name that isn't a definition, such as a theorem's, keeps nothing.
    let unknown = |r: &Value, name: &str| {
        assert!(r.get("snapshot").is_none(), "{r}");
        assert_eq!(r["check"]["error"]["kind"], "bad_request", "{r}");
        let message = format!(
            "holes names {name}, which isn't a definition or inductive type of the snapshot or of \
             a snapshot it builds on"
        );
        assert_eq!(r["check"]["error"]["message"], message, "{r}");
    };
    for name in ["nope", "t"] {
        let r = create(&c, json!({"code": spec, "holes": ["f", name]})).await;
        unknown(&r, name);
    }
    // Code that is kept already gets no new snapshot either.
    create(&c, json!({"code": spec})).await;
    let r = create(&c, json!({"code": spec, "holes": ["nope"]})).await;
    unknown(&r, "nope");

    // A definition of a snapshot it builds on may be one, and change on
    // another base.
    let s = create(&c, json!({"code": spec, "holes": ["g"]})).await;
    assert_eq!(s["snapshot"]["holes"], json!(["g"]), "{s}");
    let b2 = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 2"})).await;
    let moved = on(&b2, "def f : Nat := 1\ntheorem t : f = g := sorry");
    let r = create(&c, json!({"code": moved, "replaces": replacing(entry(&s))})).await;
    assert_eq!(r["replacement"]["changed_definitions"], json!(["g"]), "{r}");
}

/// Code on `a`'s and `b`'s headers, a blank line, then `body`.
fn on_both(a: &Value, b: &Value, body: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for s in [a, b] {
        for line in s["snapshot"]["header"].as_str().unwrap().lines() {
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    format!("{}\n\n{body}", lines.join("\n"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_a_document_or_a_session_on_a_clashing_header_is_refused() {
    let c = Cluster::start_beam(&[("w0", "v", 1)]).await;
    let skeleton = create(&c, json!({"code": SKELETON})).await;
    let proof = create(
        &c,
        json!({"code": PROOF, "replaces": replacing(entry(&skeleton))}),
    )
    .await;
    let code = on_both(&skeleton, &proof, "theorem u : 1 = 1 := t");
    let (status, checked) = c.post("/check", json!({"code": code})).await;
    assert_eq!(status, 200, "{checked}");
    assert_eq!(checked["ok"], false, "{checked}");
    assert_eq!(checked["error"]["kind"], "bad_request", "{checked}");
    let message = checked["error"]["message"].as_str().unwrap();
    assert!(message.contains(" t,"), "{message}");
    let code = on_both(&skeleton, &proof, "theorem u : 1 = 1 := by\n  sorry");
    for path in ["/documents", "/sessions"] {
        let (status, e) = c.post(path, json!({"code": code})).await;
        assert_eq!(status, 400, "{path}: {e}");
        let message = e["error"]["message"].as_str().unwrap();
        assert!(message.contains(" t,"), "{path}: {message}");
    }
    // Each alone is fine.
    let one = on(&proof, "theorem u : 1 = 1 := t");
    let (_, checked) = c.post("/check", json!({"code": one})).await;
    assert!(checked["error"].is_null(), "{checked}");
}

/// Two snapshots that each copied one file's code declare its names alike,
/// and the refusal says to import that code instead.
#[tokio::test(flavor = "multi_thread")]
async fn a_clash_on_copied_code_says_to_import_it() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let copied = "def helper : Nat := 1";
    let a = create(
        &c,
        json!({"code": format!("import Foo\n\n{copied}\ntheorem a : 1 = 1 := rfl")}),
    )
    .await;
    let b = create(
        &c,
        json!({"code": format!("import Foo\n\n{copied}\ntheorem b : 1 = 1 := rfl")}),
    )
    .await;
    let other = create(&c, json!({"code": "import Foo\n\ndef helper : Nat := 2"})).await;
    let message = |r: &Value| r["error"]["message"].as_str().unwrap().to_string();
    let both = format!(
        "import {}\nimport {}\n\ntheorem t : 1 = 1 := rfl",
        module(&a),
        module(&b)
    );
    let (_, r) = c.post("/check", json!({"code": both})).await;
    assert_eq!(r["error"]["kind"], "bad_request", "{r}");
    assert!(message(&r).contains("each declare helper"), "{r}");
    assert!(
        message(&r).contains("and import it rather than copy it"),
        "{r}"
    );
    // Declared otherwise, it is no copy.
    let both = format!(
        "import {}\nimport {}\n\ntheorem t : 1 = 1 := rfl",
        module(&a),
        module(&other)
    );
    let (_, r) = c.post("/check", json!({"code": both})).await;
    assert!(message(&r).contains("each declare helper"), "{r}");
    assert!(!message(&r).contains("copied"), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_header_cant_import_two_snapshots_that_declare_one_name() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let skeleton = create(&c, json!({"code": SKELETON})).await;
    let proof = create(
        &c,
        json!({"code": PROOF, "replaces": replacing(entry(&skeleton))}),
    )
    .await;
    let both = create(
        &c,
        json!({"code": on_both(&skeleton, &proof, "theorem u : 1 = 1 := t")}),
    )
    .await;
    assert!(both.get("snapshot").is_none(), "{both}");
    assert_eq!(both["check"]["error"]["kind"], "bad_request", "{both}");
    let message = both["check"]["error"]["message"].as_str().unwrap();
    for m in [module(&skeleton), module(&proof)] {
        assert!(message.contains(m), "{message}");
    }
    assert!(message.contains(" t,"), "{message}");
    assert!(
        message.contains(&format!("import its head {}", entry(&proof))),
        "{message}"
    );

    // Two snapshots of no one run name no head.
    let other = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 1 = 1 := by decide"}),
    )
    .await;
    let both = create(
        &c,
        json!({"code": on_both(&proof, &other, "theorem u : 1 = 1 := t")}),
    )
    .await;
    assert_eq!(both["check"]["error"]["kind"], "bad_request", "{both}");
    let message = both["check"]["error"]["message"].as_str().unwrap();
    assert!(!message.contains("head"), "{message}");

    // Each alone is fine.
    let one = create(&c, json!({"code": on(&proof, "theorem u : 1 = 1 := t")})).await;
    assert!(one["snapshot"]["id"].is_string(), "{one}");
}
