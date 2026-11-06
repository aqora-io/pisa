//! Rebuilding what builds on an entry: its dependents, and the plan to create
//! them again on a replacement. The library is a diamond: B under C and D,
//! both under E, in project `p:1`, with a module of project `p:2` on B too.

mod common;

use common::Cluster;
use pisa_client::{Client, SnapshotOptions};
use pisa_protocol::snapshot;
use serde_json::{Value, json};

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert!(r["entry"].is_string(), "{r}");
    r
}

fn entry(r: &Value) -> &str {
    r["entry"].as_str().unwrap()
}

fn module(r: &Value) -> &str {
    r["snapshot"].as_str().unwrap()
}

/// `header`, a blank line, then `body`.
fn on(snapshot: &Value, body: &str) -> String {
    format!("{}\n\n{body}", snapshot["header"].as_str().unwrap())
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
    let entries = list["entries"].as_array().unwrap();
    entries
        .iter()
        .map(|e| e["entry"].as_str().unwrap())
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
    assert_eq!(all["entries"][3]["header"], lib.e["header"]);

    let path = format!("/snapshots/{}/dependents?label=p:1", entry(&lib.b));
    let (_, p1) = get(&c, &path).await;
    assert_eq!(entries(&p1), [entry(&lib.c), entry(&lib.d), entry(&lib.e)]);
    let (_, above_e) = get(&c, &format!("/snapshots/{}/dependents", entry(&lib.e))).await;
    assert!(entries(&above_e).is_empty());

    let (status, r) = get(&c, &format!("/snapshots/{}/dependents", "0".repeat(32))).await;
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
        json!({"code": code, "replaces": entry(&lib.b), "labels": {"p": "1"}}),
    )
    .await;
    assert!(r["replaces"].is_object(), "{r}");
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
    assert_eq!(items(&p, "entry"), old.map(|e| &e["entry"]));
    assert_eq!(items(&p, "snapshot"), old.map(|e| &e["snapshot"]));
    let plan = p["plan"].as_array().unwrap();
    for item in plan {
        let code = item["code"].as_str().unwrap();
        assert_eq!(item["new_snapshot"], snapshot::module(&snapshot::id(code)));
        assert_eq!(item["labels"], json!({"p": "1"}));
        assert!(
            item["key"].as_str().unwrap().starts_with("rebuild:"),
            "{item}"
        );
        // Each replaces the entry it rebuilds, as `to` replaces `from`.
        assert_eq!(item["replaces"], json!({"entry": item["entry"]}));
        assert!(!code.contains(id(&lib.b)), "{code}");
    }
    // C, written on B, is the same code on B′.
    let c_code = on(&b2, "theorem c : 2 = 2 := rfl\nneeds b");
    assert_eq!(plan[0]["code"], c_code);
    let e_code = plan[2]["code"].as_str().unwrap();
    for new in [&plan[0], &plan[1]] {
        assert!(
            e_code.contains(new["new_snapshot"].as_str().unwrap()),
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
                let opts = SnapshotOptions {
                    labels: serde_json::from_value(item["labels"].clone()).unwrap(),
                    key: Some(item["key"].as_str().unwrap().into()),
                    replaces: serde_json::from_value(item["replaces"].clone()).unwrap(),
                    ..Default::default()
                };
                let code = item["code"].as_str().unwrap();
                jobs.push((
                    item,
                    client.submit_snapshot_with(code, &opts).await.unwrap(),
                ));
            }
            for (item, job) in jobs {
                let made_one = client.wait_job(&job).await.unwrap();
                assert_eq!(made_one.snapshot.as_deref(), item["new_snapshot"].as_str());
                made.push(made_one.entry.unwrap());
            }
        }
        made
    };
    let made = run().await;
    for (made, item) in made.iter().zip(plan) {
        let (_, info) = get(&c, &format!("/snapshots/{made}")).await;
        assert_eq!(info["replaces"], json!({"entry": item["entry"]}), "{info}");
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
    for (body, status) in [
        (with("labels", json!({})), 400),
        (with("to", json!(entry(&lib.c))), 400),
        (with("to", json!(entry(&again))), 400),
        (with("to", json!("nope")), 400),
        (with("from", json!("0".repeat(32))), 404),
    ] {
        let (got, r) = plan(&c, body.clone()).await;
        assert_eq!(got, status, "{body}: {r}");
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

#[tokio::test(flavor = "multi_thread")]
async fn a_module_that_links_the_labelled_ones_is_rebuilt_with_the_labels() {
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
    let b2 = create(&c, json!({"code": code, "replaces": entry(&b)})).await;
    let body = json!({"from": entry(&b), "to": entry(&b2), "labels": {"p": "1"}, "dry_run": true});
    let (_, p) = plan(&c, body).await;
    assert_eq!(items(&p, "entry"), [&Value::Null, &n["entry"]], "{p}");
    assert_eq!(items(&p, "snapshot"), [&m["snapshot"], &n["snapshot"]]);
    assert_eq!(p["plan"][0]["labels"], json!({"p": "1"}));
    let key = format!("rebuild:{}:{}:{}", id(&b), id(&b2), id(&m));
    assert_eq!(p["plan"][0]["key"], key);
}

#[tokio::test(flavor = "multi_thread")]
async fn theorems_that_use_a_changed_definition_are_listed_through_the_dependents() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let p1 = json!({"p": "1"});
    let b = create(
        &c,
        json!({"code": "import Foo\n\ndef f : Nat := 1\ntheorem b : f = f := sorry", "labels": p1}),
    )
    .await;
    let c_ = create(
        &c,
        json!({"code": on(&b, "def g : Nat := f\ntheorem c : 2 = 2 := rfl"), "labels": p1}),
    )
    .await;
    create(
        &c,
        json!({"code": on(&c_, "theorem e : g = g := rfl"), "labels": p1}),
    )
    .await;
    let fixed = "import Foo\n\ndef f : Nat := 2\ntheorem b : f = f := rfl";
    let replaces = json!({"entry": entry(&b), "allow": "definitions"});
    let b2 = create(
        &c,
        json!({"code": fixed, "replaces": replaces, "labels": p1}),
    )
    .await;
    let body = json!({"from": entry(&b), "to": entry(&b2), "labels": p1, "dry_run": true});
    let (_, p) = plan(&c, body).await;
    assert_eq!((&p["ok"], &p["changes"]), (&json!(true), &json!([])), "{p}");
    assert_eq!(p["changed_definitions"], json!(["f"]));
    assert_eq!(items(&p, "affected"), [&Value::Null, &json!(["e"])], "{p}");

    // Each create replaces its old entry the same way, and finds the same.
    let mut found = Vec::new();
    for item in p["plan"].as_array().unwrap() {
        let body = json!({"code": item["code"], "labels": item["labels"], "key": item["key"],
                          "replaces": item["replaces"]});
        let r = create(&c, body).await;
        assert_eq!(r["snapshot"], item["new_snapshot"], "{r}");
        found.push(r["replaces"]["affected"].clone());
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
        .map(|c| &c["new_entry"])
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
            (&made["entry"], &made["new_snapshot"]),
            (&item["entry"], &item["new_snapshot"])
        );
        let (_, info) = get(
            &c,
            &format!("/snapshots/{}", made["new_entry"].as_str().unwrap()),
        )
        .await;
        assert_eq!(info["replaces"], json!({"entry": item["entry"]}), "{info}");
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
            l.iter().map(|i| i["entry"].clone()).collect()
        })
    };
    assert_eq!(of("created"), [lib.d["entry"].clone()]);
    assert_eq!(of("failed"), [lib.c["entry"].clone()]);
    assert_eq!(of("skipped"), [lib.e["entry"].clone()]);
    assert_eq!(r["failed"][0]["result"]["ok"], false);
    assert!(r.get("retired").is_none());

    let again = rebuild(&c, &lib.b, &other, json!({"allow_changes": true})).await;
    assert_eq!(new_entries(&again), new_entries(&r), "D replays");
    assert_eq!(again["failed"][0]["entry"], lib.c["entry"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn retire_deletes_the_old_entries_and_from() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let lib = library(&c).await;
    let b2 = proved(&c, &lib).await;
    let r = rebuild(&c, &lib.b, &b2, json!({"retire": true})).await;
    assert_eq!(r["ok"], true, "{r}");
    let old = [&lib.e, &lib.d, &lib.c, &lib.b].map(|e| e["entry"].clone());
    assert_eq!(r["retired"], json!(old));
    for e in &old {
        let (status, _) = get(&c, &format!("/snapshots/{}", e.as_str().unwrap())).await;
        assert_eq!(status, 404, "{e}");
    }
    let mut now = listed(&c, "p:1").await;
    now.sort();
    let mut expected: Vec<String> = new_entries(&r)
        .iter()
        .map(|e| e.as_str().unwrap().into())
        .collect();
    expected.push(entry(&b2).into());
    expected.sort();
    assert_eq!(now, expected);
    // The other project keeps its entry, and with it B's module.
    assert_eq!(listed(&c, "p:2").await, [entry(&lib.other)]);
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
    assert!(slow["entry"].is_string());
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
