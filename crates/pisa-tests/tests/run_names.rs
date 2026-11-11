//! Run names: a create that starts a run may name it, and code may then
//! `import PisaRun.<name>`, which the server resolves to the run's head
//! before anything runs. The fake Lean's `needs NAME` fails unless an import
//! declares `NAME`, so it shows which module a name resolved to.

mod common;

use common::Cluster;
use pisa_protocol::snapshot;
use serde_json::{Value, json};

const LEMMA: &str = "import Foo\n\ntheorem lemma1 : 1 = 1 := sorry";
const PROVED: &str = "import Foo\n\ntheorem lemma1 : 1 = 1 := rfl\ntheorem proved : 2 = 2 := rfl";
const USES: &str =
    "import PisaRun.proj.lemma -- the lemma's run\n\nneeds lemma1\ntheorem t : 1 = 1 := lemma1";

async fn create(c: &Cluster, body: Value) -> Value {
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    r
}

fn id(r: &Value) -> &str {
    r["snapshot"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no snapshot: {r}"))
}

fn module(r: &Value) -> &str {
    r["snapshot"]["module"].as_str().unwrap()
}

async fn get(c: &Cluster, path: &str) -> Value {
    let r = c.client.get(c.url(path)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

async fn check(c: &Cluster, code: &str) -> Value {
    let (status, r) = c.post("/check", json!({"code": code})).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// What `import PisaRun.<name>` resolved to, as a legend gives it.
fn run(name: &str, head: &Value) -> Value {
    json!([{"name": name, "module": module(head), "snapshot_id": id(head)}])
}

/// The lemma's run, named `proj.lemma`, with its spec.
async fn lemma(c: &Cluster) -> Value {
    let spec = create(c, json!({"code": LEMMA, "name": "proj.lemma"})).await;
    assert_eq!(spec["snapshot"]["run"], "proj.lemma", "{spec}");
    spec
}

#[tokio::test(flavor = "multi_thread")]
async fn code_imports_a_run_by_name_and_follows_its_head() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = lemma(&c).await;

    let r = check(&c, USES).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["runs"], run("proj.lemma", &spec), "{r}");
    // The module hashes the code as resolved; the snapshot keeps both.
    let made = create(&c, json!({"code": USES})).await;
    assert_eq!(made["runs"], run("proj.lemma", &spec), "{made}");
    let resolved = USES.replace("PisaRun.proj.lemma", module(&spec));
    assert_eq!(
        made["snapshot"]["module"],
        snapshot::module(&snapshot::id(&resolved))
    );
    assert!(made["snapshot"].get("run").is_none(), "{made}");
    let detail = get(&c, &format!("/snapshots/{}", id(&made))).await;
    assert_eq!(
        (&detail["code"], &detail["source"]),
        (&json!(resolved), &json!(USES))
    );

    // The lemma is proved: the same text now builds on the proof.
    let proof = json!({"code": PROVED, "replaces": {"snapshot_id": id(&spec)}});
    let proof = create(&c, proof).await;
    assert_eq!(proof["snapshot"]["run"], "proj.lemma", "{proof}");
    let r = check(&c, &format!("{USES}\nneeds proved")).await;
    assert_eq!(
        (&r["ok"], &r["runs"]),
        (&json!(true), &run("proj.lemma", &proof)),
        "{r}"
    );

    // The same check in a batch, resolved once for both items.
    let items = json!({"items": [{"code": USES}, {"code": format!("{USES}\nneeds proved")}]});
    let (status, batch) = c.post("/check/batch", items).await;
    assert_eq!(status, 200, "{batch}");
    for r in batch["results"].as_array().unwrap() {
        assert_eq!(
            (&r["ok"], &r["runs"]),
            (&json!(true), &run("proj.lemma", &proof)),
            "{r}"
        );
    }

    // An agent resends its text after a conflict, imports unchanged.
    let target = "import PisaRun.proj.lemma\n\ntheorem main : 1 = 1 := sorry";
    let target = create(&c, json!({"code": target, "name": "proj.main"})).await;
    let replaces = json!({"snapshot_id": id(&target)});
    let first = "import PisaRun.proj.lemma\n\ntheorem main : 1 = 1 := lemma1";
    let first = create(&c, json!({"code": first, "replaces": replaces})).await;
    let mine = "import PisaRun.proj.lemma\n\ntheorem main : 1 = 1 := by exact lemma1";
    let refused = create(&c, json!({"code": mine, "replaces": replaces})).await;
    assert_eq!(refused["check"]["error"]["kind"], "conflict", "{refused}");
    assert_eq!(refused["heads"], json!([id(&first)]), "{refused}");
    let replaces = json!({"snapshot_id": id(&first)});
    let kept = create(&c, json!({"code": mine, "replaces": replaces})).await;
    assert_eq!(kept["snapshot"]["run"], "proj.main", "{kept}");

    // Its root deleted, the run keeps its name.
    let gone = c.client.delete(c.url(&format!("/snapshots/{}", id(&spec))));
    assert_eq!(gone.send().await.unwrap().status(), 200);
    let r = check(&c, USES).await;
    assert_eq!(r["runs"], run("proj.lemma", &proof), "{r}");
    let listed = get(&c, "/snapshots?heads=true").await;
    let named: Vec<&Value> = listed["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["run"])
        .collect();
    assert!(named.contains(&&json!("proj.lemma")), "{listed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_an_unknown_run_and_a_bad_name_are_refused_in_band() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = lemma(&c).await;
    let replaces = json!({"snapshot_id": id(&spec)});
    let a = create(
        &c,
        json!({"code": PROVED, "replaces": replaces, "fork": true}),
    )
    .await;
    let b = format!("{PROVED}\n-- another");
    let b = create(&c, json!({"code": b, "replaces": replaces, "fork": true})).await;

    let r = check(&c, USES).await;
    assert_eq!(r["error"]["kind"], "conflict", "{r}");
    let message = r["error"]["message"].as_str().unwrap();
    assert!(message.contains(id(&a)) && message.contains(id(&b)), "{r}");
    let made = create(&c, json!({"code": USES})).await;
    assert_eq!(made["check"]["error"]["kind"], "conflict", "{made}");
    assert!(made.get("snapshot").is_none(), "{made}");

    let r = check(&c, "import PisaRun.nope\n\ntheorem t : True := trivial").await;
    assert_eq!(r["error"]["kind"], "not_found", "{r}");
    let r = check(&c, "import PisaRun.end\n\ntheorem t : True := trivial").await;
    assert_eq!(r["error"]["kind"], "bad_request", "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_is_one_runs_until_an_amendment_takes_it_over() {
    const SPEC: &str = "import Foo\n\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry\ntheorem w : 1 = 2 := sorry";
    const AMENDED: &str = "import Foo\n\ndef cost : Nat := sorry\ntheorem t : cost = cost := sorry";
    const HEAD: &str = "import Foo\n\ndef cost : Nat := 1\ntheorem t : cost = cost := rfl\ntheorem w : 1 = 2 := sorry";
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = create(&c, json!({"code": SPEC, "holes": ["cost"], "name": "cost"})).await;
    let replaces = json!({"snapshot_id": id(&spec)});
    let head = create(&c, json!({"code": HEAD, "replaces": replaces})).await;

    // Taken, by a run with snapshots: refused before any build.
    let other = create(&c, json!({"code": AMENDED, "name": "cost"})).await;
    assert_eq!(other["check"]["error"]["kind"], "conflict", "{other}");
    let message = other["check"]["error"]["message"].as_str().unwrap();
    assert!(message.contains(id(&spec)), "{other}");
    for bad in [
        json!({"code": AMENDED, "name": "x", "replaces": replaces}),
        json!({"code": AMENDED, "name": "theorem"}),
        json!({"code": AMENDED, "name": "a-b"}),
    ] {
        let (status, r) = c.post("/snapshots", bad).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{r}"
        );
    }

    // The amending spec takes it over, and a resubmit carries the run there.
    let amends = json!({"snapshot_id": id(&spec), "reason": "w is false"});
    let body = json!({"code": AMENDED, "holes": ["cost"], "amends": amends, "name": "cost"});
    let amended = create(&c, body).await;
    assert_eq!(amended["snapshot"]["run"], "cost", "{amended}");
    let old = get(&c, &format!("/snapshots/{}", id(&head))).await;
    assert!(old.get("run").is_none(), "{old}");
    let uses = "import PisaRun.cost\n\ntheorem u : cost = cost := rfl";
    assert_eq!(check(&c, uses).await["runs"], run("cost", &amended));
    let ask = json!({"from": id(&spec), "to": id(&amended)});
    let (status, done) = c.post("/snapshots/resubmit", ask).await;
    assert_eq!((status, &done["ok"]), (200, &json!(true)), "{done}");
    let carried = done["items"][0]["new_snapshot_id"].as_str().unwrap();
    let r = check(&c, uses).await;
    assert_eq!(r["runs"][0]["snapshot_id"], carried, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_replay_returns_its_snapshot_after_the_run_moved() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = lemma(&c).await;
    let first = create(&c, json!({"code": USES, "key": "uses-1"})).await;
    let replaces = json!({"snapshot_id": id(&spec)});
    create(&c, json!({"code": PROVED, "replaces": replaces})).await;
    let again = create(&c, json!({"code": USES, "key": "uses-1"})).await;
    assert_eq!(again["snapshot"], first["snapshot"], "{again}");
    let other = format!("{USES}\n-- other");
    let other = create(&c, json!({"code": other, "key": "uses-1"})).await;
    assert_eq!(other["check"]["error"]["kind"], "bad_request", "{other}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_keeps_its_runs_and_a_document_follows_them() {
    let c = Cluster::start_beam(&[("w0", "v", 1)]).await;
    let spec = lemma(&c).await;
    let code = "import PisaRun.proj.lemma\n\nneeds lemma1\ntheorem t : 1 = 1 := by sorry";
    let (status, s) = c.post("/sessions", json!({"code": code})).await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["runs"], run("proj.lemma", &spec), "{s}");
    let (status, doc) = c.post("/documents", json!({"code": code})).await;
    assert_eq!(status, 200, "{doc}");
    assert_eq!(doc["runs"], run("proj.lemma", &spec), "{doc}");

    let replaces = json!({"snapshot_id": id(&spec)});
    let proof = create(&c, json!({"code": PROVED, "replaces": replaces})).await;
    // The session's check imports what the session did.
    let session = s["id"].as_str().unwrap();
    let path = format!("/sessions/{session}/check");
    let done = "import PisaRun.proj.lemma\n\nneeds lemma1\ntheorem t : 1 = 1 := by rfl";
    let (status, r) = c.post(&path, json!({"code": done})).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(
        (&r["ok"], &r["runs"]),
        (&json!(true), &run("proj.lemma", &spec)),
        "{r}"
    );
    // A document's next revision resolves again.
    let edit = json!({"code": code, "revision": doc["revision"]});
    let r = c
        .client
        .patch(c.url(&format!("/documents/{}", doc["id"].as_str().unwrap())))
        .json(&edit)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let edited: Value = r.json().await.unwrap();
    assert_eq!(edited["runs"], run("proj.lemma", &proof), "{edited}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_async_check_answers_with_what_its_names_resolved_to() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let spec = lemma(&c).await;
    let r = c
        .client
        .post(c.url("/check"))
        .header("prefer", "respond-async")
        .json(&json!({"code": USES}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let job: Value = r.json().await.unwrap();
    let path = format!("/jobs/{}?wait=30", job["id"].as_str().unwrap());
    let r = get(&c, &path).await;
    assert_eq!(
        (&r["ok"], &r["runs"]),
        (&json!(true), &run("proj.lemma", &spec)),
        "{r}"
    );
}
