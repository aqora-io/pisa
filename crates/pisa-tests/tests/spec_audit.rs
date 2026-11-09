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

/// The acceptance flow: a skeleton on a base, a proof that changes one of
/// its definitions, then a base that changes one of its own, and a rebuild
/// that retires the old run. A rebuild copies only the run's head, and
/// deleting the skeleton splices its link into the proof's, so the new proof
/// replaces the old skeleton under both edits.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "Tasks 2 and 3: splices and head-only rebuilds"]
async fn a_run_survives_an_allowed_edit_below_and_a_retiring_rebuild() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let p1 = json!({"p": "1"});
    let b = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    let s0 = create(
        &c,
        json!({"code": on(&b, "def f : Nat := 0\ntheorem t : f = g := sorry"), "labels": p1}),
    )
    .await;
    let allow_f = json!({"definitions": ["f"]});
    let p1_ = create(
        &c,
        json!({"code": on(&b, "def f : Nat := 1\ntheorem t : f = g := rfl"), "labels": p1,
               "replaces": {"snapshot_id": id(&s0), "allow": allow_f}}),
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
    // The new proof replaces S0 across both edits: its own, and B′'s.
    let allow_f_g = json!({"definitions": ["f", "g"]});
    let p1_link = json!({"snapshot_id": id(&s0), "allow": allow_f_g});
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
    assert_eq!(r["clean"], true, "{r}");
}

/// A rebuild onto a base that changes a statement the skeleton uses gives
/// the new proof no link, so `retire` keeps the old proof, and the run stays
/// on it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "Task 3: head-only rebuilds and retire"]
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
/// a rebuild from H rebuilds P1 but not S0, and P1′ replaces S0 itself.
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
        json!({"code": on(&a, "theorem t : 2 = 2 := sorry"), "labels": p1}),
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
    // H′ changes `hd`. S0 doesn't build on H, so P1′'s link leaves H′'s
    // `allow` out: S0's comparison would refuse it as naming nothing there.
    let h2 = create(
        &c,
        json!({"code": "import Foo\n\ndef hd : Nat := 0 + 1",
               "replaces": {"snapshot_id": id(&h), "allow": {"definitions": ["hd"]}}}),
    )
    .await;

    let mut body = json!({"from": id(&h), "to": id(&h2), "labels": p1, "verify": true});
    let (status, rebuilt) = c.post("/snapshots/rebuild", body.clone()).await;
    assert_eq!((status, &rebuilt["ok"]), (200, &json!(true)), "{rebuilt}");
    assert_eq!(rebuilt["plan"][0]["replaces"], replacing(&s0), "{rebuilt}");
    let p1_new = made(&rebuilt, &p1_);
    assert_eq!(link(&c, &p1_new).await, replacing(&s0));

    // Without `retire`, the old proof stays: the run forks at S0.
    let r = audit(&c, &[id(&s0)]).await;
    assert_eq!(
        r["forks"],
        json!([{"snapshot_id": id(&s0), "replaced_by": [id(&p1_), p1_new]}]),
        "{r}"
    );
    assert_eq!(r["clean"], false);

    // Sent again with `retire`, it deletes the old proof, and the run goes
    // on through the new one.
    body["retire"] = json!(true);
    let (status, rebuilt) = c.post("/snapshots/rebuild", body).await;
    assert_eq!((status, &rebuilt["ok"]), (200, &json!(true)), "{rebuilt}");
    assert_eq!(rebuilt["retired"], json!([id(&p1_)]));
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
