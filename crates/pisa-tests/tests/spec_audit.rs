//! The audit against a spec: from its spec snapshots, a run is walked
//! forward through `replaces` links to its heads, and each spec declaration
//! is judged on each head it reaches.
//! fake-lean's theorems use `sorryAx` when their code has a `sorry`, and
//! every axiom their own code declares.

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
    let query: Vec<String> = specs.iter().map(|s| format!("against={s}")).collect();
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
        json!({"code": on(&a, "theorem t : 2 = 2 := sorry\ntheorem u : 3 = 3 := sorry")}),
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
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := sorry"}),
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
        json!({"code": "import Foo\n\ntheorem t : 2 = 2 := rfl", "replaces": replacing(&s)}),
    )
    .await;
    let r = audit(&c, &[id(&s)]).await;
    assert_eq!(r["clean"], false, "{r}");
    assert_eq!(targets(&r), [format!("t {s_} {} proved false", id(&p))]);
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
               "replaces": replacing(&s), "verify": true}),
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
        json!([{"snapshot_id": id(&p), "replaces": id(&s), "allow": allow,
                "changed_definitions": ["f"], "affected": ["t"]}]),
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
        json!([{"snapshot_id": s_, "replaces": id(&s), "allow": allow,
                "changed_definitions": ["f"], "affected": ["s"]}]),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn assumptions_permitted_by_the_spec() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let s = create(
        &c,
        json!({"code": "import Foo\n\naxiom kun : 1 = 1\ntheorem t : 1 = 1 := sorry"}),
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
        json!({"code": "import Foo\n\ntheorem w : 2 = 2 := sorry"}),
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
        json!({"code": "import Foo\n\ntheorem ta : 1 = 1 := sorry", "labels": p1}),
    )
    .await;
    let s_b = create(
        &c,
        json!({"code": on(&s_a, "theorem tb : 2 = 2 := sorry"), "labels": p1}),
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
    assert_eq!(rebuilt["retired"], json!([id(&p_b), id(&s_b)]));
    let made = |old: &Value| -> String {
        let created = rebuilt["created"].as_array().unwrap();
        let made = created.iter().find(|m| m["snapshot_id"] == id(old));
        made.unwrap()["new_snapshot_id"].as_str().unwrap().into()
    };
    let (s_b_, p_b_) = (made(&s_b), made(&p_b));

    let r = audit(&c, &[id(&s_a), &s_b_]).await;
    assert_eq!(r["heads"], json!([id(&p_a), p_b_]), "{r}");
    assert_eq!(
        targets(&r),
        [
            format!("ta {} {} proved true", id(&s_a), id(&p_a)),
            format!("tb {s_b_} {p_b_} proved true")
        ]
    );
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
    bad("/snapshots/audit?against=nope".into()).await;

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
