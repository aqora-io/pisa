//! The audit against a spec: from its spec snapshots, a run is walked
//! forward through `replaces` links to its heads, and each spec declaration
//! is judged on each head it reaches.
//! The fake Lean's theorems use `sorryAx` when their code has a `sorry`, and
//! every axiom their own code declares. Code that makes the build's report
//! lie (`-- reported-as:`) is created with `verify: false`, so that the
//! build's report is what its create compares.

mod common;

use common::Cluster;
use pisa_protocol::api::{Replacement, Replaces};
use pisa_worker::catalog::{Catalog, NewEntry};
use serde_json::{Value, json};

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert!(r["snapshot"]["id"].is_string(), "{r}");
    r
}

fn id(r: &Value) -> &str {
    r["snapshot"]["id"].as_str().unwrap()
}

/// `header`, a blank line, then `body`.
fn on(snapshot: &Value, body: &str) -> String {
    format!(
        "{}\n\n{body}",
        snapshot["snapshot"]["header"].as_str().unwrap()
    )
}

async fn get(c: &Cluster, path: &str) -> (u16, Value) {
    let r = c.client.get(c.url(path)).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

/// `GET /snapshots/audit` against the spec snapshots `specs`.
async fn audit(c: &Cluster, specs: &[&str]) -> Value {
    audit_allowing(c, specs, &[]).await
}

/// `GET /snapshots/audit` against the spec snapshots `specs`, allowing the
/// definitions `allow` to change.
async fn audit_allowing(c: &Cluster, specs: &[&str], allow: &[&str]) -> Value {
    let against = specs.iter().map(|s| format!("against={s}"));
    let query: Vec<String> = against
        .chain(allow.iter().map(|n| format!("allow={n}")))
        .collect();
    let (status, r) = get(c, &format!("/snapshots/audit?{}", query.join("&"))).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// The audit's targets, each as `name spec head status verified`.
fn targets(audit: &Value) -> Vec<String> {
    let targets = audit["targets"].as_array().unwrap();
    targets
        .iter()
        .map(|t| {
            format!(
                "{} {} {} {} {}",
                t["name"].as_str().unwrap(),
                t["spec"].as_str().unwrap(),
                t["head"].as_str().unwrap(),
                t["status"].as_str().unwrap(),
                t["verified"]
            )
        })
        .collect()
}

/// What `replaces` takes for the snapshot `r`.
fn replacing(r: &Value) -> Value {
    json!({"snapshot_id": id(r)})
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_run_is_clean() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let a = create(
        &c,
        json!({"code": "import Foo\n\ntheorem a : 1 = 1 := rfl"}),
    )
    .await;
    let s = create(
        &c,
        json!({"code": on(&a, "theorem t : 2 = 2 := sorry\ntheorem u : 3 = 3 := sorry"),
               "verify": true}),
    )
    .await;
    let p = create(
        &c,
        json!({"code": on(&a, "theorem t : 2 = 2 := rfl\ntheorem u : 3 = 3 := rfl"),
               "replaces": replacing(&s), "verify": true}),
    )
    .await;
    assert_eq!(p["snapshot"]["verified"], true, "{p}");

    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], true, "{r}");
    assert_eq!(r["heads"], json!([id(&p)]));
    let (s, p) = (id(&s), id(&p));
    assert_eq!(
        targets(&r),
        [
            format!("t {s} {p} proved true"),
            format!("u {s} {p} proved true")
        ]
    );
    assert!(r.get("forks").is_none(), "{r}");
    // No link allowed a change.
    assert!(r.get("links").is_none(), "{r}");
    // The counts are the head's.
    assert_eq!(
        r["summary"],
        json!({"proved": 2, "conditional": 0, "sorry": 0, "rejected": 0, "unverified_modules": 0})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sorry_target_is_not_clean() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry", "verify": false}),
    )
    .await;
    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], false, "{r}");
    assert_eq!(r["heads"], json!([id(&s)]));
    let s_ = id(&s);
    assert_eq!(targets(&r), [format!("t {s_} {s_} sorry false")]);
    assert_eq!(r["not_proved"][0]["name"], "t", "{r}");

    // A proof that the kernel hasn't re-checked isn't accepted either.
    let p = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl", "replaces": replacing(&s), "verify": false}),
    )
    .await;
    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], false, "{r}");
    assert_eq!(targets(&r), [format!("t {s_} {} proved false", id(&p))]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spec_the_kernel_hasnt_rechecked_is_not_clean() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry", "verify": false}),
    )
    .await;
    create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl",
               "replaces": replacing(&s), "verify": true}),
    )
    .await;
    // The targets are the spec's records, which only the build reported.
    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], false, "{r}");
    // It names the spec, and how to verify it.
    assert_eq!(r["unverified"], json!([id(&s)]), "{r}");
    let reasons = &r["comparisons"][0]["reasons"];
    let how = format!(
        "{} isn't verified: POST /snapshots/{}/verify",
        id(&s),
        id(&s)
    );
    assert_eq!(reasons, &json!([how]), "{r}");
    let (status, verified) = c
        .post(&format!("/snapshots/{}/verify", id(&s)), json!({}))
        .await;
    assert_eq!(status, 200, "{verified}");
    let r = audit(&c, &[id(&s)]).await;
    assert!(r.get("unverified").is_none(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_head_whose_verified_statement_differs_from_the_spec_is_not_clean() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry", "verify": true}),
    )
    .await;
    // The build claims the spec's statement, so the link is recorded; the
    // code proves another.
    let p = create(
        &c,
        json!({"code": "import Foo\n\n-- reported-as: theorem t : 2 = 2 := rfl\ntheorem t : 3 = 3 := rfl",
               "replaces": replacing(&s), "verify": false}),
    )
    .await;
    let (status, v) = c
        .post(&format!("/snapshots/{}/verify", id(&p)), json!({}))
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["snapshot"]["verified"], true, "{v}");

    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], false, "{r}");
    assert_eq!(
        targets(&r),
        [format!("t {} {} proved true", id(&s), id(&p))]
    );
    assert_eq!(r["targets"][0]["accepted"], false, "{r}");
    // The audit compares the head with the spec from the verified records,
    // as the create would have.
    // Its signatures are the exports the builds printed, so the head's is
    // what it claimed; the hashes the comparison reads are the kernel's.
    assert_eq!(
        r["comparisons"],
        json!([{"spec": id(&s), "head": id(&p),
                "reasons": ["statement: t has a different type"],
                "changes": [{"name": "t", "change": "type",
                             "was": "t : 2 = 2", "now": "t : 2 = 2"}]}]),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_head_whose_verified_definition_differs_from_the_spec_is_not_clean() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 1\ntheorem t : f = f := sorry",
               "verify": true}),
    )
    .await;
    let p = create(
        &c,
        json!({"code": "import Foo\n\n-- reported-as: def f : Nat := 1\ndef f : Nat := 2\ntheorem t : f = f := rfl",
               "replaces": replacing(&s), "verify": false}),
    )
    .await;
    let (status, v) = c
        .post(&format!("/snapshots/{}/verify", id(&p)), json!({}))
        .await;
    assert_eq!(
        (status, &v["snapshot"]["verified"]),
        (200, &json!(true)),
        "{v}"
    );

    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], false, "{r}");
    assert_eq!(
        r["comparisons"][0]["reasons"],
        json!(["statement: f has a different value"]),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_definition_changed_under_a_links_allow_is_clean_only_if_the_audit_allows_it() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 1\ntheorem t : f = f := sorry",
               "verify": true}),
    )
    .await;
    let p = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 2\ntheorem t : f = f := rfl",
               "replaces": {"snapshot_id": id(&s), "allow": {"definitions": ["f"]}},
               "verify": true}),
    )
    .await;
    assert_eq!(p["snapshot"]["verified"], true, "{p}");

    // The submitter wrote that allow: the audit doesn't take its word.
    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], false, "{r}");
    assert_eq!(
        r["comparisons"][0]["reasons"],
        json!(["statement: f has a different value"]),
        "{r}"
    );
    assert_eq!(r["links"][0]["snapshot_id"], id(&p), "{r}");

    let r = audit_allowing(&c, &[id(&s)], &["f"]).await;
    assert_eq!(r["clean"], true, "{r}");
    assert_eq!(
        r["comparisons"][0]["replacement"]["changed_definitions"],
        json!(["f"]),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_fills_its_specs_holes_is_clean() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 1\ntheorem t : f = f := sorry",
               "holes": ["f"], "verify": true}),
    )
    .await;
    let p = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 2\ntheorem t : f = f := rfl",
               "replaces": replacing(&s), "verify": true}),
    )
    .await;
    assert_eq!(p["snapshot"]["verified"], true, "{p}");

    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], true, "{r}");
    assert_eq!(
        r["comparisons"][0]["replacement"]["changed_definitions"],
        json!(["f"]),
        "{r}"
    );
    // The link filled a hole without `allow`, and is listed for review.
    assert_eq!(
        r["links"],
        json!([{"snapshot_id": id(&p), "replaces": replacing(&s),
                "replacement": {"filled": ["t"], "changed_definitions": ["f"],
                                "affected": ["t"]}}]),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spec_without_targets_is_clean_only_if_its_head_keeps_it() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    // Only an assumption: no targets, but a statement all the same.
    let s = create(
        &c,
        json!({"code": "import Foo\n\naxiom a : 1 = 1", "verify": true}),
    )
    .await;
    let p = create(
        &c,
        json!({"code": "import Foo\n\n-- reported-as: axiom a : 1 = 1\naxiom a : 2 = 2",
               "replaces": replacing(&s), "verify": false}),
    )
    .await;
    let (status, v) = c
        .post(&format!("/snapshots/{}/verify", id(&p)), json!({}))
        .await;
    assert_eq!(
        (status, &v["snapshot"]["verified"]),
        (200, &json!(true)),
        "{v}"
    );

    let r = audit(&c, &[id(&s)]).await;
    assert!(r.get("targets").is_none(), "{r}");
    assert_eq!(
        r["comparisons"][0]["reasons"],
        json!(["statement: a has a different type"]),
        "{r}"
    );
    assert_eq!(r["clean"], false, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_base_swapped_under_the_run_is_compared_from_its_verified_record() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let b = create(
        &c,
        json!({"code": "import Foo\n\ndef g : Nat := 1", "verify": true}),
    )
    .await;
    let s = create(
        &c,
        json!({"code": on(&b, "theorem t : g = g := sorry"), "verify": true}),
    )
    .await;
    // The new base's build claims the old `g`; the head builds on it, and
    // its create compares against that claim.
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\n-- reported-as: def g : Nat := 1\ndef g : Nat := 2", "verify": false}),
    )
    .await;
    let p = create(
        &c,
        json!({"code": on(&b2, "theorem t : g = g := rfl"), "replaces": replacing(&s), "verify": false}),
    )
    .await;
    let (status, v) = c
        .post(&format!("/snapshots/{}/verify", id(&p)), json!({}))
        .await;
    assert_eq!(
        (status, &v["snapshot"]["verified"]),
        (200, &json!(true)),
        "{v}"
    );

    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], false, "{r}");
    let b_module = b["snapshot"]["module"].as_str().unwrap();
    assert_eq!(
        r["comparisons"][0]["reasons"],
        json!([format!(
            "statement: g, from {b_module}, has a different value"
        )]),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_proofs_fork_the_run() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
    )
    .await;
    let p = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl", "replaces": replacing(&s),
               "verify": true}),
    )
    .await;
    let q = create(
        &c,
        json!({"code": "import Foo\n\ntheorem h : 1 = 1 := rfl\ntheorem t : 2 = 2 := rfl",
               "replaces": replacing(&s), "verify": true, "fork": true}),
    )
    .await;

    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(
        r["forks"],
        json!([{"snapshot_id": id(&s), "replaced_by": [id(&p), id(&q)]}]),
        "{r}"
    );
    assert_eq!(r["clean"], false);
    assert_eq!(r["heads"], json!([id(&p), id(&q)]));
    let (s, p, q) = (id(&s), id(&p), id(&q));
    assert_eq!(
        targets(&r),
        [
            format!("t {s} {p} proved true"),
            format!("t {s} {q} proved true")
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn allowed_links_are_listed() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 1\ntheorem t : f = f := sorry"}),
    )
    .await;
    let allow = json!({"definitions": ["f"]});
    let p = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 2\ntheorem t : f = f := rfl",
               "replaces": {"snapshot_id": id(&s), "allow": allow}}),
    )
    .await;
    // A link that allows nothing isn't listed.
    let q = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 2\ntheorem t : f = f := rfl\ntheorem h : 1 = 1 := rfl",
               "replaces": replacing(&p)}),
    )
    .await;

    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["heads"], json!([id(&q)]), "{r}");
    assert_eq!(
        r["links"],
        json!([{"snapshot_id": id(&p), "replaces": {"snapshot_id": id(&s), "allow": allow},
                "replacement": {"filled": ["t"], "changed_definitions": ["f"],
                                "affected": ["t"]}}]),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuilt_specs_own_link_is_listed() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let p1 = json!({"p": "1"});
    let b = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 1\ntheorem b : f = f := sorry", "labels": p1}),
    )
    .await;
    let s = create(
        &c,
        json!({"code": on(&b, "theorem s : f = f := sorry"), "labels": p1}),
    )
    .await;
    let allow = json!({"definitions": ["f"]});
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 2\ntheorem b : f = f := rfl",
               "replaces": {"snapshot_id": id(&b), "allow": allow}, "labels": p1}),
    )
    .await;
    let body = json!({"from": id(&b), "to": id(&b2), "labels": p1});
    let (status, rebuilt) = c.post("/snapshots/rebuild", body).await;
    assert_eq!((status, &rebuilt["ok"]), (200, &json!(true)), "{rebuilt}");
    let s_ = rebuilt["created"][0]["new_snapshot_id"].as_str().unwrap();

    // The rebuilt spec replaces the old one under `to`'s `allow`.
    let r = audit(&c, &[s_]).await;
    assert_eq!(r["heads"], json!([s_]), "{r}");
    assert_eq!(
        r["links"],
        json!([{"snapshot_id": s_, "replaces": {"snapshot_id": id(&s), "allow": allow},
                "replacement": {"filled": [], "changed_definitions": ["f"],
                                "affected": ["s"]}}]),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spec_whose_original_was_retired_is_not_clean() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let p1 = json!({"p": "1"});
    let b = create(
        &c,
        json!({"code": "import Foo\n\ndef g : Nat := 1", "verify": true}),
    )
    .await;
    let s = create(
        &c,
        json!({"code": on(&b, "theorem t : g = g := rfl"), "labels": p1, "verify": true}),
    )
    .await;
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\ndef g : Nat := 0 + 1", "verify": true,
               "replaces": {"snapshot_id": id(&b), "allow": {"definitions": ["g"]}}}),
    )
    .await;
    let body = json!({"from": id(&b), "to": id(&b2), "labels": p1, "retire": true,
                      "verify": true});
    let (status, rebuilt) = c.post("/snapshots/rebuild", body).await;
    assert_eq!((status, &rebuilt["ok"]), (200, &json!(true)), "{rebuilt}");
    assert_eq!(rebuilt["retired"], json!([id(&s)]), "{rebuilt}");
    let s_ = made(&rebuilt, &s);

    // The copy proves its statement, but the spec it was copied from is
    // gone, so nothing can show that it states what the original did.
    let r = audit(&c, &[&s_]).await;
    assert_eq!(targets(&r), [format!("t {s_} {s_} proved true")]);
    assert_eq!(r["clean"], false, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn assumptions_permitted_by_the_spec() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\naxiom kun : 1 = 1\ntheorem t : 1 = 1 := sorry", "verify": true}),
    )
    .await;
    let p = create(
        &c,
        json!({"code": "import Foo\n\naxiom kun : 1 = 1\ntheorem t : 1 = 1 := kun",
               "replaces": replacing(&s), "verify": true}),
    )
    .await;
    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], true, "{r}");
    // The spec's axiom is no target.
    assert_eq!(
        targets(&r),
        [format!("t {} {} conditional true", id(&s), id(&p))]
    );
    let assumptions = r["assumptions"].as_array().unwrap();
    assert_eq!(assumptions.len(), 1, "{r}");
    assert_eq!(
        (&assumptions[0]["name"], &assumptions[0]["in_spec"]),
        (&json!("kun"), &json!(true))
    );

    // A proof that rests on an axiom of a snapshot it newly imports. Its
    // create can't replace the spec, so the link is written directly: the
    // audit reads records only, and judges what they say.
    let n = create(&c, json!({"code": "import Foo\n\naxiom extra : 2 = 2"})).await;
    let s2 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem w : 2 = 2 := sorry", "verify": true}),
    )
    .await;
    let proof = on(&n, "theorem w : 2 = 2 := extra");
    let (_, refused) = c
        .post(
            "/snapshots",
            json!({"code": proof, "replaces": replacing(&s2)}),
        )
        .await;
    assert!(refused.get("snapshot").is_none(), "{refused}");
    let q = create(&c, json!({"code": proof, "verify": true})).await;
    let bare = |id: &str| id.split_once('~').unwrap().1.to_string();
    let module = q["snapshot"]["module"].as_str().unwrap();
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let entry = NewEntry {
        labels: Default::default(),
        key: None,
        replaces: Some(Replaces {
            snapshot_id: bare(id(&s2)),
            allow: None,
        }),
        replacement: Some(Replacement {
            filled: vec!["w".into()],
            changed_definitions: Vec::new(),
            affected: Vec::new(),
        }),
        fork: false,
        holes: Vec::new(),
        amends: None,
        refutes: None,
        name: None,
        source: None,
        runs: Vec::new(),
    };
    let (linked, _) = catalog
        .add_entry(module.strip_prefix("PisaSnap.s").unwrap(), entry)
        .await
        .unwrap()
        .unwrap();

    let r = audit(&c, &[id(&s2)]).await;
    assert_eq!(r["clean"], false, "{r}");
    let head = format!("{}~{linked}", c.version("v"));
    assert_eq!(r["heads"], json!([head]));
    assert_eq!(
        targets(&r),
        [format!("w {} {head} conditional true", id(&s2))]
    );
    let assumptions = r["assumptions"].as_array().unwrap();
    assert_eq!(
        (&assumptions[0]["name"], &assumptions[0]["in_spec"]),
        (&json!("extra"), &json!(false)),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spec_over_two_snapshots_survives_a_retiring_rebuild() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let p1 = json!({"p": "1"});
    let s_a = create(
        &c,
        json!({"code": "import Foo\n\ntheorem ta : 1 = 1 := sorry", "labels": p1, "verify": true}),
    )
    .await;
    let s_b = create(
        &c,
        json!({"code": on(&s_a, "theorem tb : 2 = 2 := sorry"), "labels": p1, "verify": true}),
    )
    .await;
    let p_a = create(
        &c,
        json!({"code": "import Foo\n\ntheorem ta : 1 = 1 := rfl", "replaces": replacing(&s_a),
               "labels": p1, "verify": true}),
    )
    .await;
    let p_b = create(
        &c,
        json!({"code": on(&s_a, "theorem tb : 2 = 2 := rfl"), "replaces": replacing(&s_b),
               "labels": p1}),
    )
    .await;

    let body = json!({"from": id(&s_a), "to": id(&p_a), "labels": p1, "retire": true,
                      "verify": true});
    let (status, rebuilt) = c.post("/snapshots/rebuild", body).await;
    assert_eq!((status, &rebuilt["ok"]), (200, &json!(true)), "{rebuilt}");
    // Only the head P_b is copied and retired, so the spec keeps its ids.
    assert_eq!(rebuilt["retired"], json!([id(&p_b)]));
    let p_b_ = made(&rebuilt, &p_b);

    let r = audit(&c, &[id(&s_a), id(&s_b)]).await;
    assert_eq!(r["heads"], json!([id(&p_a), p_b_]), "{r}");
    assert_eq!(
        targets(&r),
        [
            format!("ta {} {} proved true", id(&s_a), id(&p_a)),
            format!("tb {} {p_b_} proved true", id(&s_b))
        ]
    );
    assert_eq!(r["clean"], true, "{r}");
}

/// The new snapshot `rebuilt` created for the old snapshot `old`.
fn made(rebuilt: &Value, old: &Value) -> String {
    let created = rebuilt["created"].as_array().unwrap();
    let made = created.iter().find(|m| m["snapshot_id"] == id(old));
    made.unwrap()["new_snapshot_id"].as_str().unwrap().into()
}

/// A snapshot's `replaces`, as `GET /snapshots/{id}` shows it.
async fn link(c: &Cluster, id: &str) -> Value {
    let (status, r) = get(c, &format!("/snapshots/{id}")).await;
    assert_eq!(status, 200, "{r}");
    r["replaces"].clone()
}

/// Deleting a snapshot in the middle of a run re-points what replaced it at
/// what it replaced, so the run is not cut.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_middle_snapshot_keeps_the_run() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s0 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
    )
    .await;
    let p1 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl", "replaces": replacing(&s0)}),
    )
    .await;
    let p2 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl\ntheorem h : 1 = 1 := rfl",
               "replaces": replacing(&p1)}),
    )
    .await;

    let deleted = c.client.delete(c.url(&format!("/snapshots/{}", id(&p1))));
    assert_eq!(deleted.send().await.unwrap().status(), 200);

    assert_eq!(link(&c, id(&p2)).await, replacing(&s0));
    let r = audit(&c, &[id(&s0)]).await;
    assert_eq!(r["heads"], json!([id(&p2)]), "{r}");
}

/// `DELETE /snapshots/{id}{query}`: the status and body.
async fn delete(c: &Cluster, id: &str, query: &str) -> (u16, Value) {
    let r = c
        .client
        .delete(c.url(&format!("/snapshots/{id}{query}")))
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

/// Two attempts at a target fork the run; dropping one with its branch
/// leaves the other as the run's only head.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_branch_resolves_a_fork() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s0 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
    )
    .await;
    let a1 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl", "replaces": replacing(&s0)}),
    )
    .await;
    let b1 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := by decide", "replaces": replacing(&s0),
               "fork": true}),
    )
    .await;
    let a2 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl\ntheorem h : 1 = 1 := rfl",
               "replaces": replacing(&a1)}),
    )
    .await;
    let r = audit(&c, &[id(&s0)]).await;
    assert_eq!(r["forks"].as_array().unwrap().len(), 1, "{r}");

    let (status, r) = delete(&c, id(&a1), "?with_replacers=true").await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["deleted"], json!([id(&a1), id(&a2)]), "{r}");
    assert_eq!(r["removed"].as_array().unwrap().len(), 2, "{r}");

    let r = audit(&c, &[id(&s0)]).await;
    assert!(r.get("forks").is_none(), "{r}");
    assert_eq!(r["heads"], json!([id(&b1)]), "{r}");
    assert_eq!(link(&c, id(&b1)).await, replacing(&s0));
    for gone in [&a1, &a2] {
        let (status, _) = get(&c, &format!("/snapshots/{}", id(gone))).await;
        assert_eq!(status, 404);
    }
}

/// A module that only a dropped branch kept goes with it, along with the
/// modules it imported that nothing else keeps.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_branch_cleans_up_what_it_imported() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let base = create(&c, json!({"code": "import Foo\n\ndef base_n : Nat := 1"})).await;
    let s0 = create(&c, json!({"code": on(&base, "theorem t : 2 = 2 := sorry")})).await;
    let p1 = create(
        &c,
        json!({"code": on(&base, "theorem t : 2 = 2 := rfl"), "replaces": replacing(&s0)}),
    )
    .await;
    // The base keeps no snapshot of its own: only what imports it keeps it.
    let (status, r) = delete(&c, id(&base), "").await;
    assert_eq!(
        (status, r["removed"].as_array().unwrap().len()),
        (200, 0),
        "{r}"
    );

    let (status, r) = delete(&c, id(&s0), "?with_replacers=true").await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["deleted"], json!([id(&s0), id(&p1)]), "{r}");
    let mut removed: Vec<&str> = r["removed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    removed.sort();
    let mut modules = [&base, &s0, &p1].map(|r| r["snapshot"]["module"].as_str().unwrap());
    modules.sort();
    assert_eq!(removed, modules, "{r}");
}

/// Without the option, a delete splices and lists the one snapshot it took.
#[tokio::test(flavor = "multi_thread")]
async fn a_plain_delete_lists_one_snapshot() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s0 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
    )
    .await;
    let p1 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl", "replaces": replacing(&s0)}),
    )
    .await;
    let p2 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl\ntheorem h : 1 = 1 := rfl",
               "replaces": replacing(&p1)}),
    )
    .await;
    let p3 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl\ntheorem h : 1 = 1 := rfl\ntheorem g : 1 = 1 := rfl",
               "replaces": replacing(&p2)}),
    )
    .await;

    let (status, r) = delete(&c, id(&p1), "").await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["deleted"], json!([id(&p1)]), "{r}");
    let (status, r) = delete(&c, id(&p2), "?with_replacers=false").await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["deleted"], json!([id(&p2)]), "{r}");
    assert_eq!(link(&c, id(&p3)).await, replacing(&s0));
    // A head has nothing replacing it, so the option takes it alone.
    let (status, r) = delete(&c, id(&p3), "?with_replacers=true").await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["deleted"], json!([id(&p3)]), "{r}");
    let r = audit(&c, &[id(&s0)]).await;
    assert_eq!(r["heads"], json!([id(&s0)]), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn with_replacers_must_be_a_boolean() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s0 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
    )
    .await;
    for query in [
        "?with_replacers=maybe",
        "?with_replacers=",
        "?with_replacers=1",
    ] {
        let (status, r) = delete(&c, id(&s0), query).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{query}: {r}"
        );
        let message = r["error"]["message"].as_str().unwrap();
        assert!(message.contains("with_replacers"), "{query}: {message}");
        assert!(message.contains("`true` or `false`"), "{query}: {message}");
    }
    // A repeated parameter is refused, not read as its last value.
    let query = "?with_replacers=true&with_replacers=false";
    let (status, r) = delete(&c, id(&s0), query).await;
    assert_eq!(
        (status, &r["error"]["kind"]),
        (400, &json!("bad_request")),
        "{query}: {r}"
    );
    // Nothing was deleted.
    let (status, _) = get(&c, &format!("/snapshots/{}", id(&s0))).await;
    assert_eq!(status, 200);
}

/// The acceptance flow: a skeleton on a base, a proof that changes one of
/// its definitions, then a base that changes one of its own, and a rebuild
/// that retires the old head. A rebuild copies only the run's head, the old
/// proof, and `retire` deletes it, which splices its link to the skeleton
/// into its copy's, so the new proof replaces the old skeleton under both
/// edits.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_survives_an_allowed_edit_below_and_a_retiring_rebuild() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let p1 = json!({"p": "1"});
    let b = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    let s0 = create(
        &c,
        json!({"code": on(&b, "def f : Nat := 0\ntheorem t : f = g := sorry"), "labels": p1,
               "holes": ["f"], "verify": true}),
    )
    .await;
    let p1_ = create(
        &c,
        json!({"code": on(&b, "def f : Nat := 1\ntheorem t : f = g := rfl"), "labels": p1,
               "replaces": replacing(&s0)}),
    )
    .await;
    let allow_g = json!({"definitions": ["g"]});
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\ndef g : Nat := 0 + 1",
               "replaces": {"snapshot_id": id(&b), "allow": allow_g}}),
    )
    .await;

    let body = json!({"from": id(&b), "to": id(&b2), "labels": p1, "retire": true,
                      "verify": true});
    let (status, rebuilt) = c.post("/snapshots/rebuild", body).await;
    assert_eq!((status, &rebuilt["ok"]), (200, &json!(true)), "{rebuilt}");
    // S0 is obsolete, so it is neither rebuilt nor retired.
    assert_eq!(rebuilt["retired"], json!([id(&p1_)]), "{rebuilt}");
    let p1_new = made(&rebuilt, &p1_);
    // The new proof replaces S0 across both edits: its own, which fills the
    // hole `f`, and B′'s, which `allow`s `g`.
    let p1_link = json!({"snapshot_id": id(&s0), "allow": allow_g});
    assert_eq!(link(&c, &p1_new).await, p1_link);

    let r = audit(&c, &[id(&s0)]).await;
    assert_eq!(r["heads"], json!([p1_new]), "{r}");
    assert_eq!(
        r["links"],
        json!([
            {"snapshot_id": p1_new, "replaces": p1_link,
             "replacement": {"filled": ["t"], "changed_definitions": ["f", "g"],
                             "affected": ["t"]}}
        ]),
        "{r}"
    );
    // `f` is a hole, and `g` changed by the leader's edit, which the audit
    // must allow.
    assert_eq!(r["clean"], false, "{r}");
    let r = audit_allowing(&c, &[id(&s0)], &["g"]).await;
    assert_eq!(r["clean"], true, "{r}");
    // The copy kept the holes.
    let (_, copy) = get(&c, &format!("/snapshots/{p1_new}")).await;
    assert_eq!(copy["holes"], json!(["f"]), "{copy}");
}

/// A rebuild onto a base that changes a statement the skeleton uses gives
/// the new proof no link, so `retire` keeps the old proof, and the run stays
/// on it.
#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_that_changes_a_statement_leaves_the_run_on_the_old_proof() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let p1 = json!({"p": "1"});
    let b = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    let s0 = create(
        &c,
        json!({"code": on(&b, "def f : Nat := 0\ntheorem t : f = g := sorry"), "labels": p1}),
    )
    .await;
    let p1_ = create(
        &c,
        json!({"code": on(&b, "def f : Nat := 1\ntheorem t : f = g := rfl"), "labels": p1,
               "replaces": {"snapshot_id": id(&s0), "allow": {"definitions": ["f"]}}}),
    )
    .await;
    let b2 = create(&c, json!({"code": "import Foo\n\ndef g : Int := 1"})).await;

    let body = json!({"from": id(&b), "to": id(&b2), "labels": p1, "allow_changes": true,
                      "retire": true});
    let (status, rebuilt) = c.post("/snapshots/rebuild", body).await;
    assert_eq!((status, &rebuilt["ok"]), (200, &json!(true)), "{rebuilt}");
    assert_eq!(
        rebuilt["changes"],
        json!(["statement: g has a different type"]),
        "{rebuilt}"
    );
    let plan = rebuilt["plan"].as_array().unwrap();
    assert_eq!(plan.len(), 1, "{rebuilt}");
    assert_eq!(plan[0]["snapshot_id"], id(&p1_), "{rebuilt}");
    assert!(plan[0].get("replaces").is_none(), "{rebuilt}");
    // The new proof has no link, so the old one is kept.
    assert!(rebuilt.get("retired").is_none(), "{rebuilt}");
    let r = audit(&c, &[id(&s0)]).await;
    assert_eq!(r["heads"], json!([id(&p1_)]), "{r}");
}

/// A proof P1 that also imports a helper H, which its skeleton S0 doesn't:
/// a rebuild from H copies P1, the copy replacing it, and once `retire`
/// deletes P1 the copy replaces S0 itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_whose_skeleton_is_outside_the_plan_survives_a_retiring_rebuild() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let p1 = json!({"p": "1"});
    let a = create(
        &c,
        json!({"code": "import Foo\n\ntheorem a : 1 = 1 := rfl"}),
    )
    .await;
    let h = create(&c, json!({"code": "import Foo\n\ndef hd : Nat := 1"})).await;
    let s0 = create(
        &c,
        json!({"code": on(&a, "theorem t : 2 = 2 := sorry"), "labels": p1, "verify": true}),
    )
    .await;
    let both = format!(
        "import Foo\nimport {}\nimport {}\n\ntheorem t : 2 = 2 := rfl",
        a["snapshot"]["module"].as_str().unwrap(),
        h["snapshot"]["module"].as_str().unwrap()
    );
    let p1_ = create(
        &c,
        json!({"code": both, "replaces": replacing(&s0), "labels": p1}),
    )
    .await;
    // H′ changes `hd`, which P1's statements don't use, so P1′'s link has
    // no `allow`.
    let h2 = create(
        &c,
        json!({"code": "import Foo\n\ndef hd : Nat := 0 + 1",
               "replaces": {"snapshot_id": id(&h), "allow": {"definitions": ["hd"]}}}),
    )
    .await;

    let mut body = json!({"from": id(&h), "to": id(&h2), "labels": p1, "verify": true});
    let (status, rebuilt) = c.post("/snapshots/rebuild", body.clone()).await;
    assert_eq!((status, &rebuilt["ok"]), (200, &json!(true)), "{rebuilt}");
    assert_eq!(rebuilt["plan"][0]["replaces"], replacing(&p1_), "{rebuilt}");
    let p1_new = made(&rebuilt, &p1_);
    assert_eq!(link(&c, &p1_new).await, replacing(&p1_));

    // Without `retire`, the old proof stays, and the run goes on through it
    // to the new one: no fork.
    let r = audit(&c, &[id(&s0)]).await;
    assert_eq!(r["heads"], json!([p1_new]), "{r}");
    assert!(r.get("forks").is_none(), "{r}");

    // Sent again with `retire`, it deletes the old proof, and the new one
    // replaces S0.
    body["retire"] = json!(true);
    let (status, rebuilt) = c.post("/snapshots/rebuild", body).await;
    assert_eq!((status, &rebuilt["ok"]), (200, &json!(true)), "{rebuilt}");
    assert_eq!(rebuilt["retired"], json!([id(&p1_)]));
    assert_eq!(link(&c, &p1_new).await, replacing(&s0));
    let r = audit(&c, &[id(&s0)]).await;
    assert_eq!(r["heads"], json!([p1_new]), "{r}");
    assert!(r.get("forks").is_none(), "{r}");
    assert_eq!(r["clean"], true, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_queries() {
    let c = Cluster::start(&[("w0", "va", 1), ("w1", "vb", 1)]).await;
    let code = "import Foo\n\ntheorem t : 2 = 2 := sorry";
    let a = create(&c, json!({"code": code, "version": "va"})).await;
    let b = create(&c, json!({"code": code, "version": "vb"})).await;
    let bad = |path: String| {
        let c = &c;
        async move {
            let (status, r) = get(c, &path).await;
            assert_eq!(
                (status, &r["error"]["kind"]),
                (400, &json!("bad_request")),
                "{path}: {r}"
            );
            r["error"]["message"].as_str().unwrap().to_string()
        }
    };

    let message = bad(format!(
        "/snapshots/audit?against={}&against={}",
        id(&a),
        id(&b)
    ))
    .await;
    for v in [c.version("va"), c.version("vb")] {
        assert!(message.contains(&v), "{v}: {message}");
    }
    bad(format!("/snapshots/audit?against={}&label=p:1", id(&a))).await;
    // `allow` is the leader's, for a run.
    bad("/snapshots/audit?label=p:1&allow=f".into()).await;
    bad(format!("/snapshots/audit?against={}&allow=", id(&a))).await;
    bad("/snapshots/audit?against=nope".into()).await;
    // A version beside the ids must be theirs.
    let (va, vb) = (c.version("va"), c.version("vb"));
    let message = bad(format!("/snapshots/audit?against={}&version={vb}", id(&a))).await;
    for v in [&va, &vb] {
        assert!(message.contains(v.as_str()), "{v}: {message}");
    }
    let (status, r) = get(
        &c,
        &format!("/snapshots/audit?against={}&version={va}", id(&a)),
    )
    .await;
    assert_eq!(status, 200, "{r}");

    let r = c
        .client
        .delete(c.url(&format!("/snapshots/{}", id(&a))))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let (status, r) = get(&c, &format!("/snapshots/audit?against={}", id(&a))).await;
    assert_eq!(status, 404, "{r}");
    assert_eq!(
        r["error"]["message"],
        format!("unknown snapshot {}", id(&a))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_review_lists_what_the_specs_statements_use_on_each_head() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let base = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    let spec = on(
        &base,
        "def cost : Nat := sorry\ndef good : Prop := g = g\ntheorem t : good = cost := sorry",
    );
    let s = create(&c, json!({"code": spec, "holes": ["cost"], "verify": true})).await;
    let filled = on(
        &base,
        "def three : Nat := 3\ndef cost : Nat := three\ndef good : Prop := g = g\n\
         theorem t : good = cost := rfl",
    );
    let p = create(
        &c,
        json!({"code": filled, "replaces": replacing(&s), "verify": true}),
    )
    .await;

    let (status, r) = get(
        &c,
        &format!("/snapshots/audit?against={}&review=true", id(&s)),
    )
    .await;
    assert_eq!(status, 200, "{r}");
    let review = &r["review"][0];
    assert_eq!(
        (&review["spec"], &review["head"]),
        (&json!(id(&s)), &json!(id(&p)))
    );
    let listed: Vec<String> = review["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            format!(
                "{} {} {} {}",
                d["name"].as_str().unwrap(),
                d["origin"].as_str().unwrap(),
                d.get("snapshot_id").map_or("-".into(), |s| s.to_string()),
                d.get("value").map_or("-".into(), |v| v.to_string()),
            )
        })
        .collect();
    // The spec's declarations in its order, the head's `three`, which the
    // hole's value now uses, then `g` from the snapshot below; each hole
    // with its value on the head, and `three` with its own, as the hole's
    // value hides behind it.
    assert_eq!(
        listed,
        [
            "cost spec - \"three\"".to_string(),
            "good spec - -".into(),
            "t spec - -".into(),
            "three head - \"3\"".into(),
            format!("g snapshot \"{}\" -", id(&base)),
        ]
    );
    let cost = &review["declarations"][0];
    assert_eq!(
        (&cost["hole"], &cost["signature"]),
        (&json!(true), &json!("cost : Nat"))
    );
    let library = review["library"].as_array().unwrap();
    assert!(
        library.contains(&json!("Nat")) && library.contains(&json!("Prop")),
        "{review}"
    );

    // Only on request, and only against a spec.
    let r = audit(&c, &[id(&s)]).await;
    assert!(r.get("review").is_none(), "{r}");

    // A hole left as `sorry` reads `sorry`, and one whose value names
    // another hole says so.
    let spec = "import Foo\n\ndef cost : Nat := sorry\ndef budget : Nat := sorry\n\
                theorem fits : cost = budget := sorry";
    let s = create(
        &c,
        json!({"code": spec, "holes": ["cost", "budget"], "verify": true}),
    )
    .await;
    let filled = "import Foo\n\ndef cost : Nat := sorry\ndef budget : Nat := cost\n\
                  theorem fits : cost = budget := rfl";
    create(
        &c,
        json!({"code": filled, "replaces": replacing(&s), "verify": true}),
    )
    .await;
    let (status, r) = get(
        &c,
        &format!("/snapshots/audit?against={}&review=true", id(&s)),
    )
    .await;
    assert_eq!(status, 200, "{r}");
    let by_name = |name: &str| {
        let declarations = r["review"][0]["declarations"].as_array().unwrap();
        declarations
            .iter()
            .find(|d| d["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(by_name("cost")["value"], "sorry");
    let budget = by_name("budget");
    assert_eq!(
        (&budget["value"], &budget["holes"]),
        (&json!("cost"), &json!(["cost"]))
    );
    for query in ["review=true", &format!("against={}&review=yes", id(&s))] {
        let (status, e) = get(&c, &format!("/snapshots/audit?{query}")).await;
        assert_eq!(
            (status, &e["error"]["kind"]),
            (400, &json!("bad_request")),
            "{query}: {e}"
        );
    }
}

/// The review entries as `name origin value`.
fn reviewed(r: &Value) -> Vec<String> {
    let declarations = r["review"][0]["declarations"].as_array().unwrap();
    declarations
        .iter()
        .map(|d| {
            format!(
                "{} {} {}",
                d["name"].as_str().unwrap(),
                d["origin"].as_str().unwrap(),
                d.get("value").map_or("-".into(), |v| v.to_string()),
            )
        })
        .collect()
}

/// A hole's value can hide behind a definition of a helper snapshot the
/// head imports, or behind the head's own definition when the hole is a
/// definition of a snapshot the spec builds on: both are shown by value.
#[tokio::test(flavor = "multi_thread")]
async fn the_review_follows_a_holes_value_into_helpers_and_below() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = "import Foo\n\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry";
    let s = create(&c, json!({"code": spec, "holes": ["cost"], "verify": true})).await;
    let helper = create(&c, json!({"code": "import Foo\n\ndef three : Nat := 3"})).await;
    let filled = on(
        &helper,
        "def cost : Nat := three\ntheorem t : cost = cost := rfl",
    );
    create(
        &c,
        json!({"code": filled, "replaces": replacing(&s), "verify": true}),
    )
    .await;
    let (_, r) = get(
        &c,
        &format!("/snapshots/audit?against={}&review=true", id(&s)),
    )
    .await;
    let listed = reviewed(&r);
    assert!(
        listed.contains(&"three snapshot \"3\"".to_string()),
        "{listed:?}"
    );

    let base = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    let spec = on(&base, "theorem u : g = g := sorry");
    let s = create(&c, json!({"code": spec, "holes": ["g"], "verify": true})).await;
    let own = "import Foo\n\ndef two : Nat := 2\ndef g : Nat := two\ntheorem u : g = g := rfl";
    create(
        &c,
        json!({"code": own, "replaces": replacing(&s), "verify": true}),
    )
    .await;
    let (_, r) = get(
        &c,
        &format!("/snapshots/audit?against={}&review=true", id(&s)),
    )
    .await;
    let listed = reviewed(&r);
    assert!(listed.contains(&"two head \"2\"".to_string()), "{listed:?}");
}

/// A value the review list should show but can't says why: it didn't print,
/// or its module has no export.
#[tokio::test(flavor = "multi_thread")]
async fn the_review_says_why_a_value_is_missing() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = "import Foo\n\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry";
    let s = create(&c, json!({"code": spec, "holes": ["cost"], "verify": true})).await;
    let unprintable =
        "import Foo\n\ndef cost : Nat := 3 -- UNPRINTABLE\ntheorem t : cost = cost := rfl";
    create(
        &c,
        json!({"code": unprintable, "replaces": replacing(&s), "verify": true}),
    )
    .await;
    let (_, r) = get(
        &c,
        &format!("/snapshots/audit?against={}&review=true", id(&s)),
    )
    .await;
    let cost = &r["review"][0]["declarations"][0];
    assert_eq!(cost["name"], "cost", "{r}");
    let why = cost["unprinted"]
        .as_str()
        .unwrap_or_else(|| panic!("{cost}"));
    assert!(why.contains("ran out of heartbeats"), "{why}");

    let other = "import Foo\n\ndef cost : Nat := sorry\ntheorem u : cost = cost := sorry";
    let s = create(
        &c,
        json!({"code": other, "holes": ["cost"], "verify": true}),
    )
    .await;
    let unexported =
        "import Foo\n\n-- NO_EXPORT\ndef cost : Nat := 3\ntheorem u : cost = cost := rfl";
    create(
        &c,
        json!({"code": unexported, "replaces": replacing(&s), "verify": true}),
    )
    .await;
    let (_, r) = get(
        &c,
        &format!("/snapshots/audit?against={}&review=true", id(&s)),
    )
    .await;
    let cost = &r["review"][0]["declarations"][0];
    let why = cost["unprinted"]
        .as_str()
        .unwrap_or_else(|| panic!("{cost}"));
    assert!(why.contains("no export"), "{why}");
}
