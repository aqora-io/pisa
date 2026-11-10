//! Rebuilding what builds on a snapshot: its dependents, and the plan to
//! create them again on a replacement. The library is a diamond: B under C and D,
//! both under E, in project `p:1`, with a module of project `p:2` on B too.

mod common;

use std::time::Duration;

use common::Cluster;
use pisa_client::{Client, CreateSnapshotOptions};
use pisa_protocol::snapshot;
use serde_json::{Value, json};

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert!(r["snapshot"]["id"].is_string(), "{r}");
    r
}

fn entry(r: &Value) -> &str {
    r["snapshot"]["id"].as_str().unwrap()
}

fn module(r: &Value) -> &str {
    r["snapshot"]["module"].as_str().unwrap()
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

struct Library {
    b: Value,
    c: Value,
    d: Value,
    e: Value,
    other: Value,
}

async fn library(c: &Cluster) -> Library {
    let p1 = json!({"p": "1"});
    let b = create(
        c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := sorry", "labels": p1}),
    )
    .await;
    let c_ = create(
        c,
        json!({"code": on(&b, "theorem c : 2 = 2 := rfl\nneeds b"), "labels": p1}),
    )
    .await;
    let d = create(
        c,
        json!({"code": on(&b, "theorem d : 3 = 3 := rfl"), "labels": p1}),
    )
    .await;
    let merged = format!(
        "import Foo\nimport {}\nimport {}\n\ntheorem e : 4 = 4 := rfl\nneeds c\nneeds d",
        module(&c_),
        module(&d)
    );
    let e = create(c, json!({"code": merged, "labels": p1})).await;
    let other = create(
        c,
        json!({"code": on(&b, "theorem other : 5 = 5 := rfl"), "labels": {"p": "2"}}),
    )
    .await;
    Library {
        b,
        c: c_,
        d,
        e,
        other,
    }
}

fn entries(list: &Value) -> Vec<&str> {
    let snapshots = list["snapshots"].as_array().unwrap();
    snapshots
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn dependents_come_parents_first() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let lib = library(&c).await;
    let (status, all) = get(&c, &format!("/snapshots/{}/dependents", entry(&lib.b))).await;
    assert_eq!(status, 200, "{all}");
    assert_eq!(
        entries(&all),
        [
            entry(&lib.c),
            entry(&lib.d),
            entry(&lib.other),
            entry(&lib.e)
        ]
    );
    assert_eq!(all["snapshots"][3]["header"], lib.e["snapshot"]["header"]);

    let path = format!("/snapshots/{}/dependents?label=p:1", entry(&lib.b));
    let (_, p1) = get(&c, &path).await;
    assert_eq!(entries(&p1), [entry(&lib.c), entry(&lib.d), entry(&lib.e)]);
    let (_, above_e) = get(&c, &format!("/snapshots/{}/dependents", entry(&lib.e))).await;
    assert!(entries(&above_e).is_empty());

    let unknown = format!("{}~{}", c.version("v"), "0".repeat(32));
    let (status, r) = get(&c, &format!("/snapshots/{unknown}/dependents")).await;
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));
    let path = format!("/snapshots/{}/dependents?label=nocolon", entry(&lib.b));
    let (status, _) = get(&c, &path).await;
    assert_eq!(status, 400);
}

async fn plan(c: &Cluster, body: Value) -> (u16, Value) {
    c.post("/snapshots/rebuild", body).await
}

/// B with its `sorry` proved and a helper, replacing B.
async fn proved(c: &Cluster, lib: &Library) -> Value {
    let code = "import Foo\n\ntheorem b : 1 = 1 := rfl\ntheorem b2 : 6 = 6 := rfl";
    let r = create(
        c,
        json!({"code": code, "replaces": {"snapshot_id": entry(&lib.b)}, "labels": {"p": "1"}}),
    )
    .await;
    assert!(r["replacement"].is_object(), "{r}");
    r
}

fn items<'a>(plan: &'a Value, field: &str) -> Vec<&'a Value> {
    let items = plan["plan"].as_array().unwrap();
    items.iter().map(|i| &i[field]).collect()
}

fn id(snapshot: &Value) -> &str {
    &module(snapshot)["PisaSnap.s".len()..]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_rebuilds_the_project_on_the_replacement() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let lib = library(&c).await;
    let b2 = proved(&c, &lib).await;
    let body =
        json!({"from": entry(&lib.b), "to": entry(&b2), "labels": {"p": "1"}, "dry_run": true});
    let (status, p) = plan(&c, body).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!((&p["ok"], &p["changes"]), (&json!(true), &json!([])), "{p}");
    assert_eq!(items(&p, "level"), [&json!(1), &json!(1), &json!(2)]);
    let old = [&lib.c, &lib.d, &lib.e];
    assert_eq!(items(&p, "snapshot_id"), old.map(|e| &e["snapshot"]["id"]));
    assert_eq!(items(&p, "module"), old.map(|e| &e["snapshot"]["module"]));
    let plan = p["plan"].as_array().unwrap();
    for item in plan {
        let code = item["code"].as_str().unwrap();
        assert_eq!(item["new_module"], snapshot::module(&snapshot::id(code)));
        assert_eq!(item["labels"], json!({"p": "1"}));
        assert!(
            item["key"].as_str().unwrap().starts_with("rebuild:"),
            "{item}"
        );
        // Each replaces the snapshot it rebuilds, as `to` replaces `from`.
        assert_eq!(
            item["replaces"],
            json!({"snapshot_id": item["snapshot_id"]})
        );
        assert!(!code.contains(id(&lib.b)), "{code}");
    }
    // C, written on B, is the same code on B′.
    let c_code = on(&b2, "theorem c : 2 = 2 := rfl\nneeds b");
    assert_eq!(plan[0]["code"], c_code);
    let e_code = plan[2]["code"].as_str().unwrap();
    for new in [&plan[0], &plan[1]] {
        assert!(
            e_code.contains(new["new_module"].as_str().unwrap()),
            "{e_code}"
        );
    }

    // A client runs it level by level, through jobs.
    let client = Client::new(&c.url("/")).unwrap();
    let run = || async {
        let mut made = Vec::new();
        for level in [1, 2] {
            let mut jobs = Vec::new();
            for item in plan.iter().filter(|i| i["level"] == level) {
                let opts = CreateSnapshotOptions {
                    labels: serde_json::from_value(item["labels"].clone()).unwrap(),
                    key: Some(item["key"].as_str().unwrap().into()),
                    replaces: serde_json::from_value(item["replaces"].clone()).unwrap(),
                    ..Default::default()
                };
                let code = item["code"].as_str().unwrap();
                jobs.push((
                    item,
                    client
                        .submit_create_snapshot_with(code, &opts)
                        .await
                        .unwrap(),
                ));
            }
            for (item, job) in jobs {
                let made_one = client.wait_job(&job).await.unwrap().snapshot.unwrap();
                assert_eq!(Some(made_one.module.as_str()), item["new_module"].as_str());
                made.push(made_one.id);
            }
        }
        made
    };
    let made = run().await;
    for (made, item) in made.iter().zip(plan) {
        let (_, info) = get(&c, &format!("/snapshots/{made}")).await;
        let link = json!({"snapshot_id": item["snapshot_id"]});
        assert_eq!(info["replaces"], link, "{info}");
    }
    let top = &plan[2];
    let r = c
        .post(
            "/check",
            json!({"code": format!("{}\n\nneeds b2\nneeds c\nneeds e", top["new_header"].as_str().unwrap())}),
        )
        .await;
    assert_eq!(r.1["ok"], true, "{}", r.1);
    // Again, the keys replay what is done.
    assert_eq!(run().await, made);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_takes_labels_and_a_replacement_beside_from() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let lib = library(&c).await;
    let b2 = proved(&c, &lib).await;
    let ok =
        json!({"from": entry(&lib.b), "to": entry(&b2), "labels": {"p": "1"}, "dry_run": true});
    let with = |field: &str, value: Value| {
        let mut body = ok.clone();
        body[field] = value;
        body
    };
    let again = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := sorry"}),
    )
    .await;
    let unknown = format!("{}~{}", c.version("v"), "0".repeat(32));
    for (body, status) in [
        (with("labels", json!({})), 400),
        (with("to", json!(entry(&lib.c))), 400),
        (with("to", json!(entry(&again))), 400),
        (with("to", json!("nope")), 400),
        (with("from", json!("0".repeat(32))), 400),
        (with("from", json!(unknown)), 404),
    ] {
        let (got, r) = plan(&c, body.clone()).await;
        assert_eq!(got, status, "{body}: {r}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn from_and_to_must_share_a_version() {
    let c = Cluster::start(&[("w0", "va", 1), ("w1", "vb", 1)]).await;
    let code = "import Foo\n\ntheorem b : 1 = 1 := sorry";
    let from = create(&c, json!({"code": code, "version": "va"})).await;
    let to = create(&c, json!({"code": code, "version": "vb"})).await;
    for dry_run in [true, false] {
        let body = json!({"from": entry(&from), "to": entry(&to), "labels": {"p": "1"}, "dry_run": dry_run});
        let (status, r) = plan(&c, body).await;
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
}

#[tokio::test(flavor = "multi_thread")]
async fn a_to_that_changes_statements_needs_allow_changes() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let lib = library(&c).await;
    let other = create(
        &c,
        json!({"code": "import Foo\n\ntheorem not_b : 1 = 1 := rfl"}),
    )
    .await;
    let body =
        json!({"from": entry(&lib.b), "to": entry(&other), "labels": {"p": "1"}, "dry_run": true});
    let (status, p) = plan(&c, body.clone()).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(p["ok"], false);
    assert_eq!(p["changes"], json!(["statement: b is missing"]));
    assert_eq!(p["plan"], json!([]));
    let mut allowed = body;
    allowed["allow_changes"] = json!(true);
    let (_, p) = plan(&c, allowed).await;
    assert_eq!(
        (&p["ok"], &p["changes"]),
        (&json!(true), &json!(["statement: b is missing"]))
    );
    assert_eq!(p["plan"].as_array().unwrap().len(), 3);
}

/// The modules of the snapshots listed for `query` (such as `?label=p:1`, or
/// none), sorted.
async fn listed_modules(c: &Cluster, query: &str) -> Vec<String> {
    let (_, r) = get(c, &format!("/snapshots{query}")).await;
    let snapshots = r["snapshots"].as_array().unwrap();
    let mut modules: Vec<String> = snapshots
        .iter()
        .map(|s| s["module"].as_str().unwrap().into())
        .collect();
    modules.sort();
    modules
}

/// Whether `module` (as `PisaSnap.s<hash>`) has a row in `v`.
async fn stored(c: &Cluster, module: &str) -> bool {
    let row = vec!["modules".to_string(), module["PisaSnap.s".len()..].into()];
    c.stored_rows("v").await.contains(&row)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_module_between_is_rebuilt_as_a_support_module() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let b = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := sorry"}),
    )
    .await;
    let m = create(&c, json!({"code": on(&b, "theorem m : 2 = 2 := rfl")})).await;
    let n = create(
        &c,
        json!({"code": on(&m, "theorem n : 3 = 3 := rfl"), "labels": {"p": "1"}}),
    )
    .await;
    let code = "import Foo\n\ntheorem b : 1 = 1 := rfl";
    let b2 = create(
        &c,
        json!({"code": code, "replaces": {"snapshot_id": entry(&b)}}),
    )
    .await;
    let r = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(r["ok"], true, "{r}");
    // M has no snapshot to copy and no labels, and N's copy replaces N.
    assert_eq!(
        items(&r, "snapshot_id"),
        [&Value::Null, &n["snapshot"]["id"]],
        "{r}"
    );
    assert_eq!(
        items(&r, "module"),
        [&m["snapshot"]["module"], &n["snapshot"]["module"]]
    );
    assert_eq!(items(&r, "labels"), [&json!({}), &json!({"p": "1"})], "{r}");
    assert_eq!(
        items(&r, "replaces"),
        [&Value::Null, &json!({"snapshot_id": entry(&n)})]
    );
    let key = format!("rebuild:{}:{}:{}", id(&b), id(&b2), id(&m));
    assert_eq!(r["plan"][0]["key"], key);

    // M′ is created and deleted: `created` names its module only.
    let created = r["created"].as_array().unwrap();
    assert_eq!(created.len(), 2, "{r}");
    assert_eq!(created[0]["module"], m["snapshot"]["module"], "{r}");
    assert_eq!(created[0]["new_module"], r["plan"][0]["new_module"], "{r}");
    assert!(created[0].get("snapshot_id").is_none(), "{r}");
    assert!(created[0].get("new_snapshot_id").is_none(), "{r}");
    assert_eq!(created[1]["snapshot_id"], entry(&n), "{r}");
    let n_ = created[1]["new_snapshot_id"].as_str().unwrap();

    // Only N and its copy are snapshots of `p:1`, and M′ is none at all.
    let mut expected = vec![
        module(&n).to_string(),
        r["plan"][1]["new_module"].as_str().unwrap().into(),
    ];
    expected.sort();
    assert_eq!(listed_modules(&c, "?label=p:1").await, expected);
    let new_m = r["plan"][0]["new_module"].as_str().unwrap();
    assert!(!listed_modules(&c, "").await.iter().any(|m| m == new_m));
    let (status, info) = get(&c, &format!("/snapshots/{n_}")).await;
    assert_eq!(status, 200, "{info}");

    // The copy still builds on it.
    assert!(stored(&c, new_m).await);
    let header = r["plan"][1]["new_header"].as_str().unwrap();
    let (_, checked) = c
        .post(
            "/check",
            json!({"code": format!("{header}\n\nneeds n\nneeds m")}),
        )
        .await;
    assert_eq!(checked["ok"], true, "{checked}");

    // Sent again, it replays N′ and leaves no snapshot of M′ behind.
    let again = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(again["plan"], r["plan"]);
    assert_eq!(new_entries(&again), new_entries(&r));
    assert!(!listed_modules(&c, "").await.iter().any(|m| m == new_m));
    assert!(stored(&c, new_m).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_obsolete_helper_a_head_imports_is_a_support_module() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let p1 = json!({"p": "1"});
    let b = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := sorry"}),
    )
    .await;
    let h = create(
        &c,
        json!({"code": on(&b, "theorem h : 2 = 2 := sorry"), "labels": p1}),
    )
    .await;
    // H2 replaces H, and carries no labels.
    let h2 = create(
        &c,
        json!({"code": on(&b, "theorem h : 2 = 2 := rfl"),
               "replaces": {"snapshot_id": entry(&h)}}),
    )
    .await;
    // P, a head of `p:1`, still imports H.
    let p = create(
        &c,
        json!({"code": on(&h, "theorem p : 3 = 3 := rfl\nneeds h"), "labels": p1}),
    )
    .await;
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := rfl",
               "replaces": {"snapshot_id": entry(&b)}}),
    )
    .await;
    let r = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(r["ok"], true, "{r}");
    // H′ is a support item: H isn't copied, as H2 replaces it, and H2 has no
    // labels.
    assert_eq!(
        items(&r, "snapshot_id"),
        [&Value::Null, &p["snapshot"]["id"]],
        "{r}"
    );
    assert_eq!(
        items(&r, "module"),
        [&h["snapshot"]["module"], &p["snapshot"]["module"]]
    );
    assert_eq!(items(&r, "labels"), [&json!({}), &p1], "{r}");
    assert_eq!(
        r["replaced"],
        json!([{"snapshot_id": entry(&h), "replaced_by": [entry(&h2)]}]),
        "{r}"
    );

    // Only P′ is a snapshot of the copies.
    let created = r["created"].as_array().unwrap();
    assert_eq!(created.len(), 2, "{r}");
    assert!(created[0].get("new_snapshot_id").is_none(), "{r}");
    assert_eq!(created[1]["snapshot_id"], entry(&p), "{r}");
    let new_h = r["plan"][0]["new_module"].as_str().unwrap();
    let new_p = r["plan"][1]["new_module"].as_str().unwrap();
    let listed = listed_modules(&c, "").await;
    assert!(listed.iter().any(|m| m == new_p), "{listed:?}");
    assert!(!listed.iter().any(|m| m == new_h), "{listed:?}");
    // P′ still builds on it.
    assert!(stored(&c, new_h).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_support_module_whose_dependents_failed_goes() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let b = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := rfl"}),
    )
    .await;
    let m = create(&c, json!({"code": on(&b, "theorem m : 2 = 2 := rfl")})).await;
    // Q needs `b`, which `to` doesn't declare.
    let q = create(
        &c,
        json!({"code": on(&m, "theorem q : 3 = 3 := rfl\nneeds b"), "labels": {"p": "1"}}),
    )
    .await;
    let to = create(
        &c,
        json!({"code": "import Foo\n\ntheorem not_b : 1 = 1 := rfl"}),
    )
    .await;
    let r = rebuild(&c, &b, &to, json!({"allow_changes": true})).await;
    assert_eq!(r["ok"], false, "{r}");
    // M′ built, and Q′ didn't.
    let created = r["created"].as_array().unwrap();
    assert_eq!(created.len(), 1, "{r}");
    assert_eq!(created[0]["module"], m["snapshot"]["module"], "{r}");
    assert!(created[0].get("new_snapshot_id").is_none(), "{r}");
    let failed = r["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "{r}");
    assert_eq!(failed[0]["snapshot_id"], entry(&q), "{r}");
    assert!(r.get("skipped").is_none(), "{r}");

    // Nothing imports M′, so it went with its snapshot.
    let new_m = created[0]["new_module"].as_str().unwrap();
    assert!(!stored(&c, new_m).await, "{:?}", c.stored_rows("v").await);
    assert!(stored(&c, module(&m)).await);
}

/// B, defining `f`, with C defining `g` from it on top and E using `g` on C,
/// all in `p:1`; and B′, which changes `f`, replacing B with `f` allowed.
async fn redefined(c: &Cluster) -> (Value, Value) {
    let p1 = json!({"p": "1"});
    let b = create(
        c,
        json!({"code": "import Foo\n\ndef f : Nat := 1\ntheorem b : f = f := sorry", "labels": p1}),
    )
    .await;
    let c_ = create(
        c,
        json!({"code": on(&b, "def g : Nat := f\ntheorem c : 2 = 2 := rfl"), "labels": p1}),
    )
    .await;
    create(
        c,
        json!({"code": on(&c_, "theorem e : g = g := rfl"), "labels": p1}),
    )
    .await;
    let fixed = "import Foo\n\ndef f : Nat := 2\ntheorem b : f = f := rfl";
    let replaces = json!({"snapshot_id": entry(&b), "allow": {"definitions": ["f"]}});
    let b2 = create(
        c,
        json!({"code": fixed, "replaces": replaces, "labels": p1}),
    )
    .await;
    (b, b2)
}

#[tokio::test(flavor = "multi_thread")]
async fn theorems_that_use_a_changed_definition_are_listed_through_the_dependents() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let p1 = json!({"p": "1"});
    let (b, b2) = redefined(&c).await;
    let body = json!({"from": entry(&b), "to": entry(&b2), "labels": p1, "dry_run": true});
    let (_, p) = plan(&c, body).await;
    assert_eq!((&p["ok"], &p["changes"]), (&json!(true), &json!([])), "{p}");
    assert_eq!(p["changed_definitions"], json!(["f"]));
    assert_eq!(items(&p, "affected"), [&Value::Null, &json!(["e"])], "{p}");

    // Each create replaces its old snapshot the same way, and finds the same.
    let mut found = Vec::new();
    for item in p["plan"].as_array().unwrap() {
        let body = json!({"code": item["code"], "labels": item["labels"], "key": item["key"],
                          "replaces": item["replaces"]});
        let r = create(&c, body).await;
        assert_eq!(r["snapshot"]["module"], item["new_module"], "{r}");
        found.push(r["replacement"]["affected"].clone());
    }
    assert_eq!(found, [Value::Null, json!(["e"])]);
}

/// Runs a rebuild of `from` on `to` for project `p:1`, with `extra` fields.
async fn rebuild(c: &Cluster, from: &Value, to: &Value, extra: Value) -> Value {
    let mut body = json!({"from": entry(from), "to": entry(to), "labels": {"p": "1"}});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let (status, r) = c.post("/snapshots/rebuild", body).await;
    assert_eq!(status, 200, "{r}");
    r
}

fn new_entries(r: &Value) -> Vec<&Value> {
    r["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["new_snapshot_id"])
        .collect()
}

async fn listed(c: &Cluster, label: &str) -> Vec<String> {
    let (_, r) = get(c, &format!("/snapshots?label={label}")).await;
    entries(&r).into_iter().map(String::from).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_runs_its_plan_and_replays_it_when_sent_again() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let lib = library(&c).await;
    let b2 = proved(&c, &lib).await;
    let r = rebuild(&c, &lib.b, &b2, json!({})).await;
    assert_eq!(r["ok"], true, "{r}");
    let plan = r["plan"].as_array().unwrap();
    let created = r["created"].as_array().unwrap();
    assert_eq!(created.len(), 3, "{r}");
    for (made, item) in created.iter().zip(plan) {
        assert_eq!(
            (&made["snapshot_id"], &made["module"], &made["new_module"]),
            (&item["snapshot_id"], &item["module"], &item["new_module"])
        );
        let (_, info) = get(
            &c,
            &format!("/snapshots/{}", made["new_snapshot_id"].as_str().unwrap()),
        )
        .await;
        let link = json!({"snapshot_id": item["snapshot_id"]});
        assert_eq!(info["replaces"], link, "{info}");
    }
    let top = format!(
        "{}\n\nneeds b2\nneeds e",
        plan[2]["new_header"].as_str().unwrap()
    );
    let (_, checked) = c.post("/check", json!({"code": top})).await;
    assert_eq!(checked["ok"], true, "{checked}");

    // Again: what is done replays, and nothing is added.
    let before = listed(&c, "p:1").await;
    let again = rebuild(&c, &lib.b, &b2, json!({})).await;
    assert_eq!(new_entries(&again), new_entries(&r));
    assert_eq!(listed(&c, "p:1").await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_chunk_skips_only_what_builds_on_it() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let lib = library(&c).await;
    // No `b`, which C needs; D doesn't, and E builds on both.
    let other = create(
        &c,
        json!({"code": "import Foo\n\ntheorem not_b : 1 = 1 := rfl"}),
    )
    .await;
    let r = rebuild(&c, &lib.b, &other, json!({"allow_changes": true})).await;
    assert_eq!(r["ok"], false, "{r}");
    let of = |list: &str| -> Vec<Value> {
        r[list].as_array().map_or(Vec::new(), |l| {
            l.iter().map(|i| i["snapshot_id"].clone()).collect()
        })
    };
    assert_eq!(of("created"), [lib.d["snapshot"]["id"].clone()]);
    assert_eq!(of("failed"), [lib.c["snapshot"]["id"].clone()]);
    assert_eq!(of("skipped"), [lib.e["snapshot"]["id"].clone()]);
    assert_eq!(r["failed"][0]["check"]["ok"], false);
    assert_eq!(r["failed"][0]["module"], lib.c["snapshot"]["module"]);
    assert_eq!(r["skipped"][0]["module"], lib.e["snapshot"]["module"]);
    assert!(r.get("retired").is_none());

    let again = rebuild(&c, &lib.b, &other, json!({"allow_changes": true})).await;
    assert_eq!(new_entries(&again), new_entries(&r), "D replays");
    assert_eq!(again["failed"][0]["snapshot_id"], lib.c["snapshot"]["id"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_runs_under_a_named_allow() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let (b, b2) = redefined(&c).await;
    let r = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(items(&r, "level"), [&json!(1), &json!(2)]);
    // E′, on C′, no longer builds on B, whose `f` its link allows.
    let allow = json!({"definitions": ["f"]});
    let created = r["created"].as_array().unwrap();
    assert_eq!(created.len(), 2, "{r}");
    for made in created {
        let path = format!("/snapshots/{}", made["new_snapshot_id"].as_str().unwrap());
        let (_, info) = get(&c, &path).await;
        let link = json!({"snapshot_id": made["snapshot_id"], "allow": allow});
        assert_eq!(info["replaces"], link, "{info}");
    }
}

/// A skeleton S0 on B, with a `sorry`, and a proof P1 replacing it, also on
/// B, both in `p:1`.
async fn skeleton_and_proof(c: &Cluster, b: &Value) -> (Value, Value) {
    let p1 = json!({"p": "1"});
    let s0 = create(
        c,
        json!({"code": on(b, "theorem s : 2 = 2 := sorry"), "labels": p1}),
    )
    .await;
    let replaces = json!({"snapshot_id": entry(&s0)});
    let p1_ = create(
        c,
        json!({"code": on(b, "theorem s : 2 = 2 := rfl"), "replaces": replaces, "labels": p1}),
    )
    .await;
    (s0, p1_)
}

/// The ids of the snapshots in `p:1` that replace `id`.
async fn replacers(c: &Cluster, id: &str) -> Vec<String> {
    let (_, r) = get(c, "/snapshots?label=p:1").await;
    let snapshots = r["snapshots"].as_array().unwrap();
    snapshots
        .iter()
        .filter(|s| s["replaces"]["snapshot_id"] == id)
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_skeleton_and_its_proof_are_rebuilt_as_a_run() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let b = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := sorry"}),
    )
    .await;
    let (s0, p1) = skeleton_and_proof(&c, &b).await;
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := rfl", "replaces": {"snapshot_id": entry(&b)}}),
    )
    .await;
    let r = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(r["ok"], true, "{r}");
    // S0 is replaced, so only the run's head is copied, and the copy
    // replaces it.
    assert_eq!(items(&r, "snapshot_id"), [&json!(entry(&p1))]);
    assert_eq!(items(&r, "replaces"), [&json!({"snapshot_id": entry(&p1)})]);
    let p1_ = new_entries(&r)[0].as_str().unwrap().to_string();
    // The run goes on through the copy: no fork.
    assert_eq!(replacers(&c, entry(&s0)).await, [entry(&p1)]);
    assert_eq!(replacers(&c, entry(&p1)).await, std::slice::from_ref(&p1_));

    // Again with `retire`: the old proof goes, and the copy replaces the
    // skeleton. S0 and B stay.
    let r = rebuild(&c, &b, &b2, json!({"retire": true})).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["retired"], json!([entry(&p1)]));
    for kept in [&s0, &b] {
        let (status, _) = get(&c, &format!("/snapshots/{}", entry(kept))).await;
        assert_eq!(status, 200);
    }
    let (_, info) = get(&c, &format!("/snapshots/{p1_}")).await;
    assert_eq!(
        info["replaces"],
        json!({"snapshot_id": entry(&s0)}),
        "{info}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proof_whose_skeleton_isnt_rebuilt_still_replaces_it() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let b = create(&c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    // S0 is another project's, so the plan doesn't rebuild it; its proof
    // P1, which changes `f`, is in `p:1`.
    let s0 = create(
        &c,
        json!({"code": on(&b, "def f : Nat := 0\ntheorem s : f = g := sorry"),
               "labels": {"p": "2"}}),
    )
    .await;
    let replaces = json!({"snapshot_id": entry(&s0), "allow": {"definitions": ["f"]}});
    let p1 = create(
        &c,
        json!({"code": on(&b, "def f : Nat := 1\ntheorem s : f = g := rfl"),
               "replaces": replaces, "labels": {"p": "1"}}),
    )
    .await;
    let replaces = json!({"snapshot_id": entry(&b), "allow": {"definitions": ["g"]}});
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\ndef g : Nat := 0 + 1", "replaces": replaces}),
    )
    .await;
    let r = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(items(&r, "snapshot_id"), [&json!(entry(&p1))]);
    // P1′ replaces P1, so stays on S0's run, and says what changed under
    // it: B′'s `g`, which P1's statement uses.
    let link = json!({"snapshot_id": entry(&p1), "allow": {"definitions": ["g"]}});
    assert_eq!(items(&r, "replaces"), [&link]);
    let made = r["created"][0]["new_snapshot_id"].as_str().unwrap();
    let (_, info) = get(&c, &format!("/snapshots/{made}")).await;
    assert_eq!(info["replaces"], link, "{info}");
}

/// B defining `g`, an unlabelled skeleton S0 on B whose statement uses `g`,
/// and its proof P1 on B in `p:1`; then the run, with `allow_changes`, that
/// rebuilds P1 on B′, which declares `g` by `def_g` instead and doesn't
/// replace B. Returns S0, P1 and the run.
async fn rebuilt_on_another_g(c: &Cluster, def_g: &str) -> (Value, Value, Value) {
    let b = create(c, json!({"code": "import Foo\n\ndef g : Nat := 1"})).await;
    let s0 = create(c, json!({"code": on(&b, "theorem s : g = g := sorry")})).await;
    let p1 = create(
        c,
        json!({"code": on(&b, "theorem s : g = g := rfl"),
               "replaces": {"snapshot_id": entry(&s0)}, "labels": {"p": "1"}}),
    )
    .await;
    let b2 = create(c, json!({"code": format!("import Foo\n\n{def_g}")})).await;
    let r = rebuild(c, &b, &b2, json!({"allow_changes": true})).await;
    assert_eq!(items(&r, "snapshot_id"), [&json!(entry(&p1))], "{r}");
    (s0, p1, r)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_allow_changes_rebuild_keeps_a_run_when_only_values_change() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let (_, p1, r) = rebuilt_on_another_g(&c, "def g : Nat := 2").await;
    assert_eq!(r["changed_definitions"], json!(["g"]), "{r}");
    // P1′ replaces P1, with the definition of B′ its statement uses.
    let link = json!({"snapshot_id": entry(&p1), "allow": {"definitions": ["g"]}});
    assert_eq!(items(&r, "replaces"), [&link], "{r}");
    assert_eq!(r["ok"], true, "{r}");
    let made = r["created"][0]["new_snapshot_id"].as_str().unwrap();
    let (_, info) = get(&c, &format!("/snapshots/{made}")).await;
    assert_eq!(info["replaces"], link, "{info}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_allow_changes_rebuild_unlinks_a_run_whose_statements_change() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let (_, p1, r) = rebuilt_on_another_g(&c, "def g : Int := 1").await;
    assert_eq!(
        r["changes"],
        json!(["statement: g has a different type"]),
        "{r}"
    );
    // P1's statement changed meaning, so P1′ can't replace it.
    assert_eq!(items(&r, "replaces"), [&Value::Null], "{r}");
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["created"][0]["snapshot_id"], p1["snapshot"]["id"], "{r}");
    let made = r["created"][0]["new_snapshot_id"].as_str().unwrap();
    let (_, info) = get(&c, &format!("/snapshots/{made}")).await;
    assert!(info.get("replaces").is_none(), "{info}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_allow_changes_rebuild_keeps_a_run_its_changes_cant_reach() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let a = create(&c, json!({"code": "import Foo\n\ndef a : Nat := 1"})).await;
    let h = create(&c, json!({"code": "import Foo\n\ndef h : Nat := 1"})).await;
    let s0 = create(
        &c,
        json!({"code": on(&a, "def f : Nat := 0\ntheorem s : f = a := sorry")}),
    )
    .await;
    // P1, which changes `f`, also imports H; S0 doesn't.
    let code = format!(
        "import Foo\nimport {}\nimport {}\n\ndef f : Nat := 1\ntheorem s : f = a := rfl",
        module(&a),
        module(&h)
    );
    let link = json!({"snapshot_id": entry(&s0), "allow": {"definitions": ["f"]}});
    let p1 = create(
        &c,
        json!({"code": code, "replaces": link, "labels": {"p": "1"}}),
    )
    .await;
    let h2 = create(&c, json!({"code": "import Foo\n\ndef h : Int := 1"})).await;
    let r = rebuild(&c, &h, &h2, json!({"allow_changes": true})).await;
    assert_eq!(
        r["changes"],
        json!(["statement: h has a different type"]),
        "{r}"
    );
    assert_eq!(items(&r, "snapshot_id"), [&json!(entry(&p1))], "{r}");
    // P1's statements use nothing of H, so H′ can't change what they state.
    let link = json!({"snapshot_id": entry(&p1)});
    assert_eq!(items(&r, "replaces"), [&link], "{r}");
    assert_eq!(r["ok"], true, "{r}");
    let made = r["created"][0]["new_snapshot_id"].as_str().unwrap();
    let (_, info) = get(&c, &format!("/snapshots/{made}")).await;
    assert_eq!(info["replaces"], link, "{info}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proof_whose_helper_uses_a_changed_type_is_unlinked() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let a = create(&c, json!({"code": "import Foo\n\ndef a : Nat := 1"})).await;
    let h = create(&c, json!({"code": "import Foo\n\ndef h : Nat := 1"})).await;
    let s0 = create(
        &c,
        json!({"code": on(&a, "def f : Nat := 0\ntheorem s : f = a := sorry")}),
    )
    .await;
    // P1's helper lemma states something about H's `h`; S0 doesn't use H.
    let code = format!(
        "import Foo\nimport {}\nimport {}\n\ndef f : Nat := 1\ntheorem s : f = a := rfl\n\
         theorem aux : h = h := rfl",
        module(&a),
        module(&h)
    );
    let link = json!({"snapshot_id": entry(&s0), "allow": {"definitions": ["f"]}});
    let p1 = create(
        &c,
        json!({"code": code, "replaces": link, "labels": {"p": "1"}}),
    )
    .await;
    let h2 = create(&c, json!({"code": "import Foo\n\ndef h : Int := 1"})).await;
    let r = rebuild(&c, &h, &h2, json!({"allow_changes": true})).await;
    assert_eq!(items(&r, "snapshot_id"), [&json!(entry(&p1))], "{r}");
    // `aux` would state something else, so P1′ can't replace P1.
    assert_eq!(items(&r, "replaces"), [&Value::Null], "{r}");
    assert_eq!(r["ok"], true, "{r}");
    let made = r["created"][0]["new_snapshot_id"].as_str().unwrap();
    let (_, info) = get(&c, &format!("/snapshots/{made}")).await;
    assert!(info.get("replaces").is_none(), "{info}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_is_compared_through_what_to_no_longer_builds_on() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let a = create(&c, json!({"code": "import Foo\n\ndef a : Nat := 1"})).await;
    let a2 = create(&c, json!({"code": "import Foo\n\ndef a : Int := 1"})).await;
    let b = create(&c, json!({"code": on(&a, "theorem b : 1 = 1 := rfl")})).await;
    let b2 = create(&c, json!({"code": on(&a2, "theorem b : 1 = 1 := rfl")})).await;
    // P builds on A only through B, and states something about A's `a`.
    let p = create(
        &c,
        json!({"code": on(&b, "theorem p : a = a := rfl"), "labels": {"p": "1"}}),
    )
    .await;
    let r = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(r["changes"], json!([]), "{r}");
    assert_eq!(items(&r, "snapshot_id"), [&json!(entry(&p))], "{r}");
    // B′ builds on A2 instead, so P′'s `a` is another one: no link.
    assert_eq!(items(&r, "replaces"), [&Value::Null], "{r}");
    assert_eq!(r["ok"], true, "{r}");
    let made = r["created"][0]["new_snapshot_id"].as_str().unwrap();
    let (_, info) = get(&c, &format!("/snapshots/{made}")).await;
    assert!(info.get("replaces").is_none(), "{info}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resent_rebuild_replays_the_copy_that_replaced_its_original() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let lib = library(&c).await;
    let b2 = proved(&c, &lib).await;
    let r = rebuild(&c, &lib.b, &b2, json!({})).await;
    assert_eq!(r["ok"], true, "{r}");
    // The originals are replaced now, by their own copies: they stay items.
    let again = rebuild(&c, &lib.b, &b2, json!({})).await;
    assert_eq!(again["plan"], r["plan"]);
    assert_eq!(new_entries(&again), new_entries(&r));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_above_a_rebuilt_module_keeps_its_link() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let p1 = json!({"p": "1"});
    let b = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := sorry", "labels": p1}),
    )
    .await;
    // A chain whose statements use what the one below declares.
    let c_ = create(
        &c,
        json!({"code": on(&b, "def c : Nat := 2"), "labels": p1}),
    )
    .await;
    let d = create(
        &c,
        json!({"code": on(&c_, "def d : Nat := c"), "labels": p1}),
    )
    .await;
    let e = create(
        &c,
        json!({"code": on(&d, "theorem e : d = d := rfl"), "labels": p1}),
    )
    .await;
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := rfl", "replaces": {"snapshot_id": entry(&b)}}),
    )
    .await;
    let r = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(r["ok"], true, "{r}");
    let old = [&c_, &d, &e];
    assert_eq!(items(&r, "snapshot_id"), old.map(|s| &s["snapshot"]["id"]));
    // What E and D use is in the copies below them, which declare the same.
    let links = old.map(|s| json!({"snapshot_id": entry(s)}));
    assert_eq!(items(&r, "replaces"), links.each_ref(), "{r}");
    for (made, link) in new_entries(&r).into_iter().zip(&links) {
        let (_, info) = get(&c, &format!("/snapshots/{}", made.as_str().unwrap())).await;
        assert_eq!(&info["replaces"], link, "{info}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_leaves_the_labels_is_reported() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let b = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := sorry"}),
    )
    .await;
    let s0 = create(
        &c,
        json!({"code": on(&b, "theorem s : 2 = 2 := sorry"), "labels": {"p": "2"}}),
    )
    .await;
    let p1 = create(
        &c,
        json!({"code": on(&b, "theorem s : 2 = 2 := rfl"), "labels": {"p": "1"},
               "replaces": {"snapshot_id": entry(&s0)}}),
    )
    .await;
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := rfl", "replaces": {"snapshot_id": entry(&b)}}),
    )
    .await;
    // `p:2`'s run went on in `p:1`, so a `p:2` rebuild leaves it out, and
    // says so.
    let replaced = json!([{"snapshot_id": entry(&s0), "replaced_by": [entry(&p1)]}]);
    for dry_run in [true, false] {
        let body = json!({"from": entry(&b), "to": entry(&b2), "labels": {"p": "2"},
                          "dry_run": dry_run});
        let (status, r) = plan(&c, body).await;
        assert_eq!((status, &r["ok"]), (200, &json!(true)), "{r}");
        assert_eq!(r["plan"], json!([]), "{r}");
        assert_eq!(r["replaced"], replaced, "{r}");
    }
    let r = rebuild(&c, &b, &b2, json!({"dry_run": true})).await;
    assert_eq!(items(&r, "snapshot_id"), [&json!(entry(&p1))], "{r}");
    assert!(r.get("replaced").is_none(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn retire_keeps_from() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let lib = library(&c).await;
    let b2 = proved(&c, &lib).await;
    let r = rebuild(&c, &lib.b, &b2, json!({"retire": true})).await;
    assert_eq!(r["ok"], true, "{r}");
    let old = [&lib.e, &lib.d, &lib.c].map(|e| e["snapshot"]["id"].clone());
    assert_eq!(r["retired"], json!(old));
    for e in &old {
        let (status, _) = get(&c, &format!("/snapshots/{}", e.as_str().unwrap())).await;
        assert_eq!(status, 404, "{e}");
    }
    let (status, from) = get(&c, &format!("/snapshots/{}", entry(&lib.b))).await;
    assert_eq!(status, 200, "{from}");
    let mut now = listed(&c, "p:1").await;
    now.sort();
    let mut expected: Vec<String> = new_entries(&r)
        .iter()
        .map(|e| e.as_str().unwrap().into())
        .collect();
    expected.extend([entry(&lib.b).into(), entry(&b2).into()]);
    expected.sort();
    assert_eq!(now, expected);
    // The other project keeps its snapshot, and with it B's module.
    assert_eq!(listed(&c, "p:2").await, [entry(&lib.other)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_item_that_loses_to_an_agent_fails_and_a_resend_heals() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let p1 = json!({"p": "1"});
    let b = create(
        &c,
        json!({"code": "import Foo\n\ntheorem b : 1 = 1 := sorry"}),
    )
    .await;
    // Two levels: L, which is slow to build, and the skeleton X on it.
    let l = create(
        &c,
        json!({"code": on(&b, "theorem l : 2 = 2 := rfl\nSLEEP 800"), "labels": p1}),
    )
    .await;
    let x = create(
        &c,
        json!({"code": on(&l, "theorem x : 3 = 3 := sorry"), "labels": p1}),
    )
    .await;
    let code = "import Foo\n\ntheorem b : 1 = 1 := rfl";
    let b2 = create(
        &c,
        json!({"code": code, "replaces": {"snapshot_id": entry(&b)}}),
    )
    .await;

    let rebuilding = tokio::spawn({
        let (client, url) = (c.client.clone(), c.url("/snapshots/rebuild"));
        let body = json!({"from": entry(&b), "to": entry(&b2), "labels": p1});
        async move {
            let r = client.post(url).json(&body).send().await.unwrap();
            r.json::<Value>().await.unwrap()
        }
    });
    // While L's copy builds, an agent proves X on L.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let a = create(
        &c,
        json!({"code": on(&l, "theorem x : 3 = 3 := rfl"), "labels": p1,
               "replaces": {"snapshot_id": entry(&x)}}),
    )
    .await;

    let r = rebuilding.await.unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(new_entries(&r).len(), 1, "{r}");
    let failed = r["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "{r}");
    assert_eq!(failed[0]["snapshot_id"], entry(&x), "{r}");
    assert_eq!(failed[0]["check"]["error"]["kind"], "conflict", "{r}");
    // The agent's proof is X's only replacer: no fork.
    assert_eq!(replacers(&c, entry(&x)).await, [entry(&a)]);

    // Sent again, the rebuild plans A, which replaced X, and not X.
    let again = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(
        items(&again, "snapshot_id"),
        [&json!(entry(&l)), &json!(entry(&a))]
    );
    assert_eq!(new_entries(&again)[0], new_entries(&r)[0]);
    assert_eq!(replacers(&c, entry(&x)).await, [entry(&a)]);
    assert_eq!(replacers(&c, entry(&a)).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_runs_as_a_job_that_any_server_reads() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let other_server = c.start_server().await;
    let lib = library(&c).await;
    let b2 = proved(&c, &lib).await;
    // A slow chunk on top, so the job is still running when first read.
    let slow = create(
        &c,
        json!({"code": on(&lib.e, "theorem slow : 6 = 6 := rfl\nSLEEP 800"), "labels": {"p": "1"}}),
    )
    .await;
    assert!(slow["snapshot"]["id"].is_string());
    let body = json!({"from": entry(&lib.b), "to": entry(&b2), "labels": {"p": "1"}});
    let r = c
        .client
        .post(c.url("/snapshots/rebuild"))
        .header("prefer", "respond-async")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let accepted: Value = r.json().await.unwrap();
    assert_eq!(
        (&accepted["kind"], &accepted["status"]),
        (&json!("rebuild_snapshots"), &json!("running"))
    );
    let job = accepted["id"].as_str().unwrap();

    let look = |wait: &str| {
        let url = common::url_of(&other_server, &format!("/jobs/{job}?wait={wait}"));
        let client = c.client.clone();
        async move {
            let r = client.get(url).send().await.unwrap();
            (r.status().as_u16(), r.json::<Value>().await.unwrap())
        }
    };
    let (status, running) = look("0").await;
    assert_eq!(
        (status, &running["status"]),
        (202, &json!("running")),
        "{running}"
    );
    let (status, done) = look("30").await;
    assert_eq!(status, 200, "{done}");
    assert_eq!(
        (
            done["ok"].clone(),
            done["created"].as_array().unwrap().len()
        ),
        (json!(true), 4)
    );

    // A dry run answers at once, preferred async or not.
    let mut dry = body;
    dry["dry_run"] = json!(true);
    let r = c
        .client
        .post(c.url("/snapshots/rebuild"))
        .header("prefer", "respond-async")
        .json(&dry)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_onto_a_structure_with_another_field_allows_the_structure() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let b = create(
        &c,
        json!({"code": "import Foo\n\nstructure S where x : Nat"}),
    )
    .await;
    let s = create(
        &c,
        json!({"code": on(&b, "theorem t (s : S) : s = s := rfl"), "labels": {"p": "1"}}),
    )
    .await;
    let b2 = create(
        &c,
        json!({"code": "import Foo\n\nstructure S where x : Nat y : Nat",
               "replaces": {"snapshot_id": entry(&b), "allow": {"definitions": ["S"]}}}),
    )
    .await;
    let r = rebuild(&c, &b, &b2, json!({})).await;
    assert_eq!(r["ok"], true, "{r}");
    // The copy replaces the original under an allow of the structure: its
    // constructor isn't something `allow` can name.
    let allow = json!({"definitions": ["S"]});
    assert_eq!(
        items(&r, "replaces"),
        [&json!({"snapshot_id": entry(&s), "allow": allow})],
        "{r}"
    );
}
