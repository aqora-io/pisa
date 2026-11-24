//! Snapshots, compiled by fake-lean and imported by fake-repl. A `needs NAME`
//! line fails unless `NAME` is declared, so it shows what an import provides.

mod common;

use std::time::{Duration, Instant};

use common::{Cluster, bwrap_works};
use object_store::ObjectStoreExt;
use object_store::path::Path;
use pisa_protocol::api::ProofStatus;
use pisa_worker::catalog::Catalog;
use serde_json::{Value, json};

async fn create(c: &Cluster, code: &str) -> Value {
    let (status, r) = c.post("/snapshots", json!({"code": code})).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// Creates a snapshot that must be kept and returns its response.
async fn kept(c: &Cluster, code: &str) -> Value {
    let r = create(c, code).await;
    assert!(r["snapshot"].is_object(), "{code}: {r}");
    r
}

async fn check(c: &Cluster, code: &str) -> Value {
    let (status, r) = c.post("/check", json!({"code": code})).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// `header`, a blank line, then `body`.
fn on(snapshot: &Value, body: &str) -> String {
    format!(
        "{}\n\n{body}",
        snapshot["snapshot"]["header"].as_str().unwrap()
    )
}

fn names(result: &Value) -> Vec<&str> {
    result["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect()
}

/// The snapshot blobs and the rows of version `v`.
async fn stored(c: &Cluster) -> (Vec<String>, Vec<Vec<String>>) {
    (c.stored_blobs().await, c.stored_rows("v").await)
}

fn entry(snapshot: &Value) -> &str {
    snapshot["snapshot"]["id"].as_str().unwrap()
}

/// Where the store keeps file `ext` of a snapshot's module: under the build
/// its record names.
async fn blob(c: &Cluster, snapshot: &Value, ext: &str) -> String {
    let id = &module(snapshot)["PisaSnap.s".len()..];
    let v = c.version("v");
    let meta = Catalog::new(c.db.clone(), v.clone())
        .module(id)
        .await
        .unwrap();
    let build = &meta.modules.last().unwrap().build;
    format!("snapshots/{v}/{id}.{build}.{ext}.zst")
}

async fn request(c: &Cluster, method: reqwest::Method, path: &str) -> (u16, Value) {
    let r = c.client.request(method, c.url(path)).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

const ROOT: &str = "import Foo\n\ntheorem base : 1 = 1 := rfl";

#[tokio::test(flavor = "multi_thread")]
async fn code_on_a_snapshot_sees_it_and_reports_only_itself() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = kept(&c, ROOT).await;
    let snapshot = &root["snapshot"];
    let module = snapshot["module"].as_str().unwrap();
    assert!(module.starts_with("PisaSnap.s"), "{root}");
    assert_eq!(snapshot["header"], format!("import Foo\nimport {module}"));
    assert_eq!(snapshot["version"], c.version("v"));
    assert_eq!(snapshot["status"], "proved");
    assert_eq!(root["check"]["ok"], true, "{root}");
    assert_eq!(names(&root["check"]), ["base"]);

    let r = check(
        &c,
        &on(&root, "theorem t : 2 = 2 := rfl\nneeds base\n#check Nat"),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(names(&r), ["t"], "only the new declarations");
    assert_eq!(r["messages"][0]["pos"]["line"], 6, "{r}");

    // Without the import, `base` is unknown.
    let r = check(&c, "import Foo\n\nneeds base").await;
    assert_eq!(r["ok"], false, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_chain_and_merge() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let root = kept(&c, ROOT).await;
    let left = kept(&c, &on(&root, "theorem l : 2 = 2 := rfl\nneeds base")).await;
    let right = kept(&c, &on(&root, "theorem r : 3 = 3 := rfl")).await;
    assert_eq!(names(&left["check"]), ["l"], "the report covers the chunk");

    let merge = format!(
        "import Foo\nimport {}\nimport {}\n\ntheorem m : 4 = 4 := rfl\nneeds base\nneeds l\nneeds r",
        module(&left),
        module(&right),
    );
    let merged = kept(&c, &merge).await;
    assert_eq!(merged["check"]["ok"], true, "{merged}");
    assert_eq!(names(&merged["check"]), ["m"]);
    let header = format!("import Foo\nimport {}", module(&merged));
    assert_eq!(merged["snapshot"]["header"], header);

    let r = check(&c, &on(&merged, "needs base\nneeds l\nneeds r\nneeds m")).await;
    assert_eq!(r["ok"], true, "{r}");

    // A parent's private declarations stay private.
    let private = kept(&c, &on(&root, "private theorem hidden : 1 = 1 := rfl")).await;
    let r = check(&c, &on(&private, "needs hidden")).await;
    assert_eq!(r["ok"], false, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_import_the_header_split_misses_is_refused() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = kept(&c, ROOT).await;
    // Lean reads the import after the comment, and the worker has the module
    // cached, so the code builds; the line-by-line split misses it, so the
    // record wouldn't say what the module builds on.
    let code = format!(
        "import Foo\n/- c -/ import {}\n\ntheorem t : 2 = 2 := rfl\nneeds base",
        module(&root)
    );
    let r = create(&c, &code).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(r["check"]["error"]["kind"], "bad_request", "{r}");
    let message = r["check"]["error"]["message"].as_str().unwrap();
    assert!(message.contains("its own line"), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_code_is_not_kept() {
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |mut w| {
        w.import_timeout_secs = 1;
        w
    })
    .await;
    let r = create(&c, "import Foo\n\ntheorem t : 1 = 1 := error_here").await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(r["check"]["ok"], false);
    assert_eq!(r["check"]["messages"][0]["severity"], "error");
    assert_eq!(r["check"]["messages"][0]["pos"]["line"], 3);
    assert_eq!(
        r["check"]["reasons"],
        json!(["error: Lean reported errors"])
    );

    // The policy rejects it before anything runs: CRASH would kill fake-lean.
    let r = create(&c, "#eval 1\nCRASH").await;
    assert_eq!(r["check"]["error"]["kind"], "policy", "{r}");
    assert_eq!(r["check"]["reasons"], json!(["forbidden: #eval"]));

    let (status, r) = c
        .post(
            "/snapshots",
            json!({"code": "theorem t : True := trivial\nSLEEP 5000", "timeout": 1}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(r["check"]["error"]["kind"], "timeout", "{r}");

    let r = create(&c, "theorem t : True := trivial\nCRASH").await;
    assert_eq!(r["check"]["error"]["kind"], "crash", "{r}");

    let r = create(&c, "import PisaSnap.oops\n\ntheorem t : True := trivial").await;
    assert_eq!(r["check"]["error"]["kind"], "bad_request", "{r}");

    assert_eq!(stored(&c).await, (vec![], vec![]));
    // The slot still works.
    kept(&c, ROOT).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sorry_is_kept_as_sorry_and_same_code_shares_a_module() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let code = "import Foo\n\ntheorem d : 1 = 1 := by sorry";
    let sorry = kept(&c, code).await;
    assert_eq!(sorry["snapshot"]["status"], "sorry", "{sorry}");
    assert!(sorry["snapshot"].get("dirty").is_none(), "{sorry}");
    assert_eq!(sorry["check"]["ok"], false);
    assert_eq!(sorry["check"]["reasons"], json!(["axiom: d uses sorryAx"]));
    assert_eq!(sorry["check"]["messages"][0]["severity"], "warning");

    let slow = "import Foo\n\ntheorem s : 1 = 1 := rfl\n-- SLEEP 500";
    let start = Instant::now();
    let first = kept(&c, slow).await;
    assert!(start.elapsed() >= Duration::from_millis(500));
    let start = Instant::now();
    let again = kept(&c, slow).await;
    assert!(
        start.elapsed() < Duration::from_millis(400),
        "not built again"
    );
    assert_eq!(module(&first), module(&again));
    // The check is the stored build's, so that of the second create too;
    // `timing` is this request's.
    assert_eq!(first["check"], again["check"]);
    assert!(first["check"]["time"].as_f64().unwrap() >= 0.5, "{first}");
    assert!(first["check"]["timing"]["execution"].as_f64().unwrap() >= 0.5);
    assert_eq!(
        (&first["cached"], &again["cached"]),
        (&json!(false), &json!(true))
    );
    assert!(
        first["timing"]["execution"].as_f64().unwrap() >= 0.5,
        "{first}"
    );
    assert!(first["timing"]["total"].as_f64().unwrap() >= 0.5);
    assert_eq!(again["timing"]["execution"], 0.0, "{again}");
    assert_eq!(again["timing"]["verification"], 0.0, "{again}");
    let total = again["timing"]["total"].as_f64().unwrap();
    assert!(total > 0.0 && total < 0.4, "{again}");
    assert_ne!(entry(&first), entry(&again));
}

#[tokio::test(flavor = "multi_thread")]
async fn labels_belong_to_entries() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let mut created = Vec::new();
    for run in ["1", "2"] {
        let body = json!({"code": ROOT, "labels": {"run": run}});
        let (status, r) = c.post("/snapshots", body).await;
        assert_eq!(status, 200, "{r}");
        assert_eq!(r["snapshot"]["labels"], json!({"run": run}), "{r}");
        created.push(r);
    }
    for (snapshot, run) in created.iter().zip(["1", "2"]) {
        let i = info(&c, snapshot).await;
        assert_eq!(i["id"], entry(snapshot), "{i}");
        assert_eq!(i["labels"], json!({"run": run}), "{i}");
        assert_eq!(i["module"], module(&created[0]), "{i}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_labels_are_rejected() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let body = json!({"code": ROOT, "labels": {"bad key": "x"}});
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["check"]["error"]["kind"], "bad_request", "{r}");
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(stored(&c).await, (vec![], vec![]));
}

async fn create_keyed(c: &Cluster, code: &str, key: &str, labels: Value) -> Value {
    let body = json!({"code": code, "key": key, "labels": labels});
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// Whether an entry has the idempotency key `k`.
async fn has_key(c: &Cluster, k: &str) -> bool {
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    catalog.keyed(k).await.unwrap().is_some()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_key_returns_the_same_entry() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let code = "import Foo\n\ntheorem s : 1 = 1 := rfl\n-- SLEEP 500";
    let first = create_keyed(&c, code, "k1", json!({"a": "1"})).await;
    assert!(first["snapshot"].is_object(), "{first}");
    assert_eq!(first["snapshot"]["labels"], json!({"a": "1"}), "{first}");

    let start = Instant::now();
    let again = create_keyed(&c, code, "k1", json!({"a": "2"})).await;
    assert!(start.elapsed() < Duration::from_millis(400), "{again}");
    assert_eq!(again["snapshot"], first["snapshot"], "{again}");
    assert_eq!(again["snapshot"]["labels"], json!({"a": "1"}), "{again}");
    assert_eq!(info(&c, &first).await["labels"], json!({"a": "1"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_with_different_code_is_refused() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    create_keyed(&c, ROOT, "k1", json!({})).await;
    let before = stored(&c).await;

    let other = "import Foo\n\ntheorem other : 2 = 2 := rfl";
    let r = create_keyed(&c, other, "k1", json!({})).await;
    assert_eq!(r["check"]["error"]["kind"], "bad_request", "{r}");
    assert_eq!(r["check"]["ok"], false, "{r}");
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(stored(&c).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_create_leaves_its_key_unused() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let r = create_keyed(
        &c,
        "import Foo\n\ntheorem t : 1 = 1 := error_here",
        "k2",
        json!({}),
    )
    .await;
    assert_eq!(r["check"]["ok"], false, "{r}");
    assert!(r.get("snapshot").is_none(), "{r}");
    assert!(!has_key(&c, "k2").await);

    let r = create_keyed(&c, ROOT, "k2", json!({})).await;
    assert!(r["snapshot"]["id"].is_string(), "{r}");
    assert!(has_key(&c, "k2").await);
}

/// A kept module that fails the re-check the request asked for gets no entry,
/// so the request's key stays unused.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_re_check_leaves_its_key_unused() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    unverified(&c, FORGED).await;
    let body = json!({"code": FORGED, "verify": true, "key": "k5"});
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert_rejected_by_kernel(&r);
    assert!(r.get("snapshot").is_none(), "{r}");
    assert!(!has_key(&c, "k5").await);

    let body = json!({"code": FORGED, "verify": false, "key": "k5"});
    let (_, r) = c.post("/snapshots", body).await;
    assert!(r["snapshot"]["id"].is_string(), "{r}");
    assert!(has_key(&c, "k5").await);
}

fn creating(c: &Cluster, code: &str, key: &str) -> tokio::task::JoinHandle<Value> {
    let body = json!({"code": code, "key": key});
    let (client, url) = (c.client.clone(), c.url("/snapshots"));
    tokio::spawn(async move {
        let r = client.post(url).json(&body).send().await.unwrap();
        r.json::<Value>().await.unwrap()
    })
}

/// Creates that overlap on one key make one entry. On the same code the later
/// one gets the first's entry. On other code it is refused after building, and
/// leaves no files behind.
#[tokio::test(flavor = "multi_thread")]
async fn creates_racing_on_a_key_make_one_entry() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let code = "import Foo\n\ntheorem s : 1 = 1 := rfl\n-- SLEEP 500";
    let (a, b) = tokio::join!(creating(&c, code, "k6"), creating(&c, code, "k6"));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(a["snapshot"]["id"].is_string(), "{a}");
    assert_eq!(a["snapshot"]["id"], b["snapshot"]["id"], "{a} {b}");
    delete_all(&c, &[&a]).await;

    let other = "import Foo\n\ntheorem t : 2 = 2 := rfl\n-- SLEEP 500";
    let (a, b) = tokio::join!(creating(&c, code, "k7"), creating(&c, other, "k7"));
    let (a, b) = (a.unwrap(), b.unwrap());
    let (won, lost) = if a.get("snapshot").is_some() {
        (a, b)
    } else {
        (b, a)
    };
    assert!(won["snapshot"]["id"].is_string(), "{won}");
    assert_eq!(lost["check"]["error"]["kind"], "bad_request", "{lost}");
    assert!(lost.get("snapshot").is_none(), "{lost}");
    delete_all(&c, &[&won]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_is_freed_when_its_entry_is_deleted() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let first = create_keyed(&c, ROOT, "k3", json!({})).await;
    assert!(has_key(&c, "k3").await);
    delete(&c, &first).await;
    assert!(!has_key(&c, "k3").await);

    let second = create_keyed(&c, ROOT, "k3", json!({})).await;
    assert!(second["snapshot"].is_object(), "{second}");
    assert_ne!(entry(&second), entry(&first));
    assert_eq!(info(&c, &second).await["id"], entry(&second));
    delete_all(&c, &[&second]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_keys_are_rejected() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for key in ["has space", "", &"k".repeat(257)] {
        let r = create_keyed(&c, ROOT, key, json!({})).await;
        assert_eq!(r["check"]["error"]["kind"], "bad_request", "{key:?}: {r}");
        assert!(r.get("snapshot").is_none(), "{r}");
    }
    assert_eq!(stored(&c).await, (vec![], vec![]));
}

/// Checks on a snapshot go to the worker that built it, which owns its
/// header and keeps it warm. The stored module is corrupted, so any other
/// worker would refuse to load it.
#[tokio::test(flavor = "multi_thread")]
async fn checks_go_to_the_builder_and_corrupt_modules_are_refused() {
    let c = Cluster::start(&[("w0", "v", 1), ("w1", "v", 1)]).await;
    for i in 0..3 {
        let root = kept(&c, &format!("import Foo\n\ntheorem base{i} : 1 = 1 := rfl")).await;
        let path = Path::from(blob(&c, &root, "olean").await);
        let garbage = zstd::encode_all(&b"garbage"[..], 3).unwrap();
        c.store.put(&path, garbage.into()).await.unwrap();
        for _ in 0..5 {
            let r = check(&c, &on(&root, &format!("needs base{i}"))).await;
            assert_eq!(r["ok"], true, "{r}");
        }
        // Code with other imports misses, and the other worker refuses it.
        let r = check(&c, &format!("import Bar\nimport {}\n", module(&root))).await;
        let refused = r["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("corrupt"));
        let served = r["ok"] == true;
        assert!(refused || served, "{r}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn other_workers_load_snapshots_from_the_store() {
    let mut c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = kept(&c, ROOT).await;
    let child = kept(&c, &on(&root, "theorem c : 2 = 2 := rfl\nneeds base")).await;

    c.add_worker(common::worker_config("w1", "v", 1));
    c.wait_for_workers(2).await;
    c.kill_worker(0).await;

    let r = check(&c, &on(&child, "needs base\nneeds c")).await;
    assert_eq!(r["ok"], true, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_snapshots_are_not_found() {
    let c = Cluster::start_beam(&[("w0", "v", 1)]).await;
    let unknown = format!(
        "import PisaSnap.s{}\n\ntheorem t : True := trivial",
        "0".repeat(32)
    );
    let r = check(&c, &unknown).await;
    assert_eq!(r["error"]["kind"], "not_found", "{r}");
    let (status, r) = c.post("/sessions", json!({"code": unknown})).await;
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));
    let r = check(&c, "import PisaSnap.oops\n\ntheorem t : True := trivial").await;
    assert_eq!(r["error"]["kind"], "bad_request", "{r}");
    let r = check(&c, "import Foo\n\ntheorem t : True := trivial").await;
    assert_eq!(r["ok"], true, "the slot still works: {r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_run_on_snapshots_and_keep_their_imports() {
    let mut c = Cluster::start_beam(&[("w0", "v", 1)]).await;
    let root = kept(&c, ROOT).await;
    let other = kept(&c, "import Foo\n\ntheorem base : 1 = 1 := rfl\n-- other").await;
    let code = on(&root, "needs base\ntheorem t : 1 = 1 := by sorry");
    let (status, s) = c.post("/sessions", json!({"code": code})).await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["states"][0]["pos"]["line"], 5);

    c.add_worker(common::beam_worker_config("w1", "v", 1));
    c.wait_for_workers(2).await;
    c.kill_worker(0).await;

    let state = s["states"][0]["id"].as_str().unwrap();
    let (status, step) = c
        .post(&format!("/states/{state}/tactic"), json!({"tactic": "rfl"}))
        .await;
    assert_eq!(status, 200, "{step}");
    assert_eq!(step["complete"], true);

    let session = s["id"].as_str().unwrap();
    let path = format!("/sessions/{session}/check");
    let proof = "needs base\ntheorem t : 1 = 1 := by rfl";
    let (_, ok) = c.post(&path, json!({"code": on(&root, proof)})).await;
    assert_eq!(ok["ok"], true, "{ok}");
    let (_, moved) = c.post(&path, json!({"code": on(&other, proof)})).await;
    assert_eq!(
        moved["reasons"],
        json!(["statement: the imports differ from the session's"]),
        "{moved}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn entries_hold_their_modules_until_the_last_reference_goes() {
    let c = Cluster::start_with(
        common::router_config(),
        &[("w0", "v", 1), ("w1", "v", 1)],
        |mut w| {
            w.snapshot_confirm_secs = 0;
            w
        },
    )
    .await;
    let root = kept(&c, ROOT).await;
    let child = kept(&c, &on(&root, "theorem c : 2 = 2 := rfl")).await;
    // Both workers have used both.
    for _ in 0..4 {
        let r = check(&c, &on(&child, "needs c")).await;
        assert_eq!(r["ok"], true, "{r}");
    }

    let i = info(&c, &child).await;
    assert_eq!(i["id"], entry(&child));
    assert_eq!(i["module"], module(&child));
    assert_eq!(i["imports"], json!([module(&root)]));
    assert_eq!(i["header"], child["snapshot"]["header"]);
    assert_eq!(i["status"], "proved");
    assert_eq!(names(&i["check"]), ["c"]);
    assert_eq!(i["exports"][0]["name"], "c", "{i}");
    assert!(i.get("decls").is_none(), "{i}");
    assert!(
        i["code"]
            .as_str()
            .unwrap()
            .ends_with("theorem c : 2 = 2 := rfl")
    );

    let gone = delete(&c, &root).await;
    assert_eq!(gone, json!({"removed": [], "deleted": [entry(&root)]}));
    let root_path = format!("/snapshots/{}", entry(&root));
    let (status, r) = request(&c, reqwest::Method::GET, &root_path).await;
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));
    let (status, _) = request(&c, reqwest::Method::DELETE, &root_path).await;
    assert_eq!(status, 404);
    for _ in 0..4 {
        let r = check(&c, &on(&child, "needs c")).await;
        assert_eq!(r["ok"], true, "the child still imports the root: {r}");
    }

    let gone = delete(&c, &child).await;
    assert_eq!(gone["removed"], json!([module(&child), module(&root)]));
    assert_eq!(stored(&c).await, (vec![], vec![]));
    for _ in 0..4 {
        let r = check(&c, &on(&child, "needs c")).await;
        assert_eq!(r["error"]["kind"], "not_found", "{r}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_one_of_two_entries_keeps_the_module() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let first = kept(&c, ROOT).await;
    let second = kept(&c, ROOT).await;
    assert_eq!(module(&first), module(&second));

    assert_eq!(delete(&c, &first).await["removed"], json!([]));
    let r = check(&c, &on(&second, "needs base")).await;
    assert_eq!(r["ok"], true, "{r}");

    let gone = delete(&c, &second).await;
    assert_eq!(gone["removed"], json!([module(&second)]));
    assert_eq!(stored(&c).await, (vec![], vec![]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cleaned_up_module_can_be_created_again() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let first = kept(&c, ROOT).await;
    let first_files = c.stored_blobs().await;
    let gone = delete(&c, &first).await;
    assert_eq!(gone["removed"], json!([module(&first)]));

    let again = kept(&c, ROOT).await;
    assert_eq!(module(&again), module(&first));
    let r = check(&c, &on(&again, "needs base")).await;
    assert_eq!(r["ok"], true, "{r}");
    // A new build, whose files a late delete of the first one can't touch.
    let files = c.stored_blobs().await;
    assert!(
        files.contains(&blob(&c, &again, "olean").await),
        "{files:?}"
    );
    assert!(
        files.iter().all(|f| !first_files.contains(f)),
        "{files:?} {first_files:?}"
    );
}

/// The parent is deleted and built again while the child builds on its old
/// build, whose files are gone: the child isn't stored naming them.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_built_on_a_parent_built_again_meanwhile_is_not_kept() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let root = kept(&c, ROOT).await;
    let code = on(&root, "theorem c : 2 = 2 := rfl\n-- SLEEP 1500");
    let building = tokio::spawn({
        let (client, url) = (c.client.clone(), c.url("/snapshots"));
        async move {
            let r = client.post(url).json(&json!({"code": code})).send().await;
            r.unwrap().json::<Value>().await.unwrap()
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    delete(&c, &root).await;
    let again = kept(&c, ROOT).await;

    let r = building.await.unwrap();
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(r["check"]["error"]["kind"], "not_found", "{r}");
    let message = r["check"]["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("{} was deleted and built again", module(&root))),
        "{r}"
    );
    // Built again on the parent as it is now, it is kept.
    let child = kept(&c, &on(&again, "theorem c : 2 = 2 := rfl\n-- SLEEP 1500")).await;
    let r = check(&c, &on(&child, "needs base\nneeds c")).await;
    assert_eq!(r["ok"], true, "{r}");
}

/// Deletes a snapshot's entry and returns the response.
async fn delete(c: &Cluster, snapshot: &Value) -> Value {
    let path = format!("/snapshots/{}", entry(snapshot));
    let (status, r) = request(c, reqwest::Method::DELETE, &path).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// Deletes every entry in `snapshots` and expects nothing to be left.
async fn delete_all(c: &Cluster, snapshots: &[&Value]) {
    for snapshot in snapshots {
        delete(c, snapshot).await;
    }
    assert_eq!(stored(c).await, (vec![], vec![]));
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_work_in_the_sandbox() {
    if !bwrap_works() {
        eprintln!("skipping: bubblewrap cannot create namespaces here");
        return;
    }
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |mut w| {
        w.sandbox = pisa_worker::config::Sandbox::Bwrap;
        w
    })
    .await;
    let root = kept(&c, ROOT).await;
    let child = kept(&c, &on(&root, "theorem c : 2 = 2 := rfl\nneeds base")).await;
    let r = check(&c, &on(&child, "needs base\nneeds c")).await;
    assert_eq!(r["ok"], true, "{r}");
}

const MODULE_ROOT: &str = "module\npublic import Foo\n\npublic theorem mbase : 1 = 1 := rfl\ntheorem mhidden : 2 = 2 := rfl";

/// The builder imports a module-system snapshot as code on top of it does,
/// which sees only what the module exports, so the kernel verifier, which
/// reads all of it, describes its declarations.
#[tokio::test(flavor = "multi_thread")]
async fn module_system_snapshots_are_re_checked() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for body in [json!({}), json!({"verify": false})] {
        let mut body = body;
        body["code"] = json!(format!("{MODULE_ROOT}\n-- {body}"));
        let (status, r) = c.post("/snapshots", body).await;
        assert_eq!(status, 200, "{r}");
        assert_eq!(r["snapshot"]["verified"], true, "{r}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn module_system_snapshots_keep_all_their_files() {
    let mut c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = kept(&c, MODULE_ROOT).await;
    let name = module(&root);
    assert_eq!(
        root["snapshot"]["header"],
        format!("module\npublic import Foo\npublic import {name}")
    );
    let objects = c.stored_blobs().await;
    for ext in ["olean", "olean.server", "olean.private", "ir", "ir.sig"] {
        let object = blob(&c, &root, ext).await;
        assert!(objects.contains(&object), "{objects:?}");
    }
    let child = kept(
        &c,
        &on(&root, "public theorem mchild : 3 = 3 := rfl\nneeds mbase"),
    )
    .await;
    let child_name = module(&child);
    assert_eq!(
        child["snapshot"]["header"],
        format!("module\npublic import Foo\npublic import {child_name}")
    );

    // Another worker must fetch every file: a module-system module without
    // its .olean.private imports as empty.
    c.add_worker(common::worker_config("w1", "v", 1));
    c.wait_for_workers(2).await;
    c.kill_worker(0).await;
    let r = check(&c, &on(&child, "needs mbase\nneeds mchild")).await;
    assert_eq!(r["ok"], true, "{r}");
    let r = check(&c, &on(&child, "needs mhidden")).await;
    assert_eq!(r["ok"], false, "only public declarations are visible: {r}");
    // Plain code can import module-system snapshots.
    let r = check(&c, &format!("import {child_name}\n\nneeds mchild")).await;
    assert_eq!(r["ok"], true, "{r}");

    delete_all(&c, &[&root, &child]).await;
}

/// fake-pisa's verifier rejects any module whose code contains `forge`, so
/// whether this code is kept shows whether the verifier ran.
const FORGED: &str = "import Foo\n\ntheorem forge : False := by trivial";
const MACRO: &str = "import Foo\n\nmacro \"twice\" : term => `(2)\ndef m : Nat := twice";

async fn create_with(c: &Cluster, code: &str, verify: bool) -> Value {
    let (status, r) = c
        .post("/snapshots", json!({"code": code, "verify": verify}))
        .await;
    assert_eq!(status, 200, "{r}");
    r
}

/// Creates a snapshot without the kernel re-check, which must be kept.
async fn unverified(c: &Cluster, code: &str) -> Value {
    let r = create_with(c, code, false).await;
    assert!(r["snapshot"].is_object(), "{code}: {r}");
    assert_eq!(r["snapshot"]["verified"], false, "{r}");
    r
}

fn module(snapshot: &Value) -> &str {
    snapshot["snapshot"]["module"].as_str().unwrap()
}

async fn info(c: &Cluster, snapshot: &Value) -> Value {
    let path = format!("/snapshots/{}", entry(snapshot));
    let (status, r) = request(c, reqwest::Method::GET, &path).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// A `GET /snapshots/{id}` answer as the snapshot it details, without its
/// code, check and exports.
fn snapshot_of(detail: &Value) -> Value {
    let mut snapshot = detail.clone();
    for field in ["code", "check", "exports"] {
        snapshot.as_object_mut().unwrap().remove(field);
    }
    snapshot
}

async fn verify(c: &Cluster, snapshot: &Value) -> Value {
    let path = format!("/snapshots/{}/verify", entry(snapshot));
    let (status, r) = request(c, reqwest::Method::POST, &path).await;
    assert_eq!(status, 200, "{r}");
    r
}

fn assert_rejected_by_kernel(r: &Value) {
    assert_eq!(r["check"]["ok"], false, "{r}");
    assert!(
        r["check"]["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("kernel re-check")),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_are_re_checked_by_default_unless_the_request_opts_out() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let before = stored(&c).await;
    let r = create(&c, FORGED).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_rejected_by_kernel(&r);
    assert_eq!(stored(&c).await, before);
    let sorry = "import Foo\n\ntheorem d : 1 = 1 := by sorry";
    let checked = kept(&c, sorry).await;
    assert_eq!(checked["snapshot"]["verified"], true, "{checked}");
    assert_eq!(checked["snapshot"]["status"], "sorry", "{checked}");

    // Opted out, the build's own report stands.
    let forged = unverified(&c, FORGED).await;
    assert_eq!(forged["check"]["ok"], true, "{forged}");
    assert_eq!(names(&forged["check"]), ["forge"]);
    assert_eq!(info(&c, &forged).await["verified"], false);
    // The axiom report still runs, so `sorry` is still caught.
    let sorry = unverified(&c, &format!("{sorry}\n-- opted out")).await;
    assert_eq!(sorry["snapshot"]["status"], "sorry", "{sorry}");
    assert_eq!(sorry["check"]["reasons"], json!(["axiom: d uses sorryAx"]));
}

/// Code outside the module system with no macros whose re-check runs out of
/// time is kept, with the build's statuses, unverified.
#[tokio::test(flavor = "multi_thread")]
async fn plain_code_whose_re_check_runs_out_of_time_is_kept_unverified() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let (status, r) = c
        .post("/snapshots", json!({"code": SLOW, "timeout": 1}))
        .await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["snapshot"]["verified"], false, "{r}");
    assert_eq!(r["check"]["ok"], true, "{r}");
    assert_eq!(r["unverified"]["module"], module(&r), "{r}");
    let reason = r["unverified"]["reason"].as_str().unwrap();
    assert!(reason.contains("more than 1s"), "{reason}");
    assert_eq!(verify(&c, &r).await["snapshot"]["verified"], true);
}

/// Kept code that isn't verified, as a create with `verify: false` or an
/// earlier release leaves it, is re-checked by a create that doesn't opt
/// out, with the fallback new code has.
#[tokio::test(flavor = "multi_thread")]
async fn kept_code_that_isnt_verified_is_re_checked_by_a_default_create() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let code = "import Foo\n\ntheorem k : 1 = 1 := rfl";
    unverified(&c, code).await;
    // Opting out adds an entry as it is.
    let r = create_with(&c, code, false).await;
    assert_eq!(r["snapshot"]["verified"], false, "{r}");
    let r = kept(&c, code).await;
    assert_eq!(r["snapshot"]["verified"], true, "{r}");

    // Out of time, plain code gets its entry, unverified, and says why.
    let slow = unverified(&c, SLOW).await;
    let (status, r) = c
        .post("/snapshots", json!({"code": SLOW, "timeout": 1}))
        .await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["snapshot"]["verified"], false, "{r}");
    assert_eq!(r["unverified"]["module"], module(&slow), "{r}");
    assert_eq!(r["unverified"]["kind"], "timeout", "{r}");

    // A rejection or a crash adds none.
    unverified(&c, FORGED).await;
    let r = create(&c, FORGED).await;
    assert!(r["snapshot"].is_null(), "{r}");
    assert_eq!(r["check"]["error"]["kind"], "lean", "{r}");
    let breaking = "import Foo\n\ntheorem b : 1 = 1 := rfl\n-- BREAK_KERNEL";
    unverified(&c, breaking).await;
    let r = create(&c, breaking).await;
    assert!(r["snapshot"].is_null(), "{r}");
    assert_eq!(r["check"]["error"]["kind"], "crash", "{r}");
}

/// A re-check that fails, rather than run out of time or memory, may be
/// the code's doing: nothing is kept, and the error is a crash that says
/// what the verifier said.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_check_that_fails_keeps_nothing_and_says_why() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for (marker, said) in [
        ("BREAK_KERNEL", "the fake kernel fell over"),
        ("FAIL_KERNEL", "the fake describe failed"),
    ] {
        let code = format!("import Foo\n\ntheorem s : 1 = 1 := rfl\n-- {marker}");
        let r = create(&c, &code).await;
        assert!(r["snapshot"].is_null(), "{r}");
        assert_eq!(r["check"]["error"]["kind"], "crash", "{r}");
        let message = r["check"]["error"]["message"].as_str().unwrap();
        assert!(message.contains(said), "{message}");
    }
    assert_eq!(stored(&c).await, (vec![], vec![]));
}

/// A verify's failure says whether the kernel rejected the module or the
/// re-check didn't finish, and a create that asked to verify an import out
/// of time is a timeout, not the kernel's verdict.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_verify_says_what_kind_of_failure_it_was() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let forged = unverified(&c, FORGED).await;
    let r = verify(&c, &forged).await;
    assert_eq!(r["failure"]["kind"], "lean", "{r}");
    let crashing = unverified(
        &c,
        "import Foo\n\ntheorem s : 1 = 1 := rfl\n-- BREAK_KERNEL",
    )
    .await;
    let r = verify(&c, &crashing).await;
    assert_eq!(r["failure"]["kind"], "crash", "{r}");

    let slow = unverified(&c, SLOW).await;
    let (status, r) = c
        .post(
            "/snapshots",
            json!({"code": on(&slow, "theorem t : 2 = 2 := rfl"), "verify": true, "timeout": 1}),
        )
        .await;
    assert_eq!(status, 200, "{r}");
    assert!(r["snapshot"].is_null(), "{r}");
    assert_eq!(r["check"]["error"]["kind"], "timeout", "{r}");
    let message = r["check"]["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(module(&slow)) && message.contains("more than 1s"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn macros_are_re_checked_unless_the_request_opts_out() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let macro_snap = kept(&c, MACRO).await;
    assert_eq!(macro_snap["snapshot"]["verified"], true, "{macro_snap}");
    assert_eq!(macro_snap["check"]["ok"], true, "{macro_snap}");

    // A rejected chunk stores nothing.
    let forged_macro = format!("{MACRO}\ntheorem forge : False := by trivial");
    let before = stored(&c).await;
    let r = create(&c, &forged_macro).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_rejected_by_kernel(&r);
    assert_eq!(stored(&c).await, before);

    // A child can expand its parent's macros without any macro keyword.
    let r = create(&c, &on(&macro_snap, "theorem forge : 1 = 1 := twice")).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_rejected_by_kernel(&r);
    let child = kept(&c, &on(&macro_snap, "def n : Nat := twice")).await;
    assert_eq!(child["snapshot"]["verified"], true, "{child}");

    let opted_out = create_with(&c, &forged_macro, false).await;
    assert!(opted_out["snapshot"].is_object(), "{opted_out}");
    assert_eq!(opted_out["snapshot"]["verified"], false, "{opted_out}");
    let code = on(&macro_snap, "theorem forge : 2 = 2 := twice");
    let child = create_with(&c, &code, false).await;
    assert!(child["snapshot"].is_object(), "{child}");
    assert_eq!(child["snapshot"]["verified"], false, "{child}");

    // elab runs code at elaboration time, so opting out doesn't allow it.
    let elab = "import Foo\n\nelab \"x\" : term => pure default";
    let r = create_with(&c, elab, false).await;
    assert_eq!(r["check"]["error"]["kind"], "policy", "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn metaprogramming_is_re_checked_on_a_pool_that_allows_it() {
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 1)], |mut w| {
        w.allow_metaprogramming = true;
        w
    })
    .await;
    let code = "import Foo\n\nelab \"x\" : term => pure default\ntheorem forge : 1 = 1 := rfl";
    let r = create(&c, code).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_rejected_by_kernel(&r);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_macro_chunk_on_unverified_imports_is_not_verified_yet() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = unverified(&c, ROOT).await;
    let chunk = kept(&c, &on(&root, "macro \"twice\" : term => `(2)")).await;
    assert_eq!(chunk["snapshot"]["verified"], false, "{chunk}");

    let r = verify(&c, &chunk).await;
    assert_eq!(r["snapshot"]["verified"], true, "{r}");
    assert_eq!(r["checked"], json!([module(&root), module(&chunk)]));
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_true_re_checks_the_chunk_and_its_imports() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let r = create_with(&c, FORGED, true).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_rejected_by_kernel(&r);
    assert_eq!(stored(&c).await, (vec![], vec![]));

    let parent = unverified(&c, ROOT).await;
    let child = create_with(&c, &on(&parent, "theorem c : 2 = 2 := rfl"), true).await;
    assert_eq!(child["snapshot"]["verified"], true, "{child}");
    assert_eq!(info(&c, &parent).await["verified"], true);

    // A forged import stops it, and the chunk is not built.
    let forged = unverified(&c, FORGED).await;
    let before = stored(&c).await;
    let r = create_with(&c, &on(&forged, "theorem c : 2 = 2 := rfl"), true).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_rejected_by_kernel(&r);
    let message = r["check"]["error"]["message"].as_str().unwrap();
    assert!(message.contains(module(&forged)), "{r}");
    assert_eq!(stored(&c).await, before);

    // Asking again for code that is already kept verifies what is there.
    let code = "import Foo\n\ntheorem existing : 1 = 1 := rfl";
    let first = unverified(&c, code).await;
    let again = create_with(&c, code, true).await;
    assert_eq!(module(&again), module(&first));
    assert_eq!(again["snapshot"]["verified"], true, "{again}");

    // A kept module that fails the re-check gets no new entry, as new code
    // that fails it isn't kept: the answer has no snapshot, and the reason is
    // in the check.
    let before = c.stored_rows("v").await;
    let r = create_with(&c, FORGED, true).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_rejected_by_kernel(&r);
    assert_eq!(c.stored_rows("v").await, before);
    assert_eq!(info(&c, &forged).await["verified"], false, "still kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_checks_imports_first_and_remembers() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = unverified(&c, ROOT).await;
    let mid = unverified(&c, &on(&root, "theorem mid : 2 = 2 := rfl")).await;
    let leaf = unverified(&c, &on(&mid, "theorem leaf : 3 = 3 := rfl")).await;

    let r = verify(&c, &leaf).await;
    assert_eq!(
        r,
        json!({
            "snapshot": snapshot_of(&info(&c, &leaf).await),
            "checked": [module(&root), module(&mid), module(&leaf)],
        })
    );
    assert_eq!(
        (&r["snapshot"]["verified"], &r["snapshot"]["status"]),
        (&json!(true), &json!("proved"))
    );
    for snapshot in [&root, &mid, &leaf] {
        assert_eq!(info(&c, snapshot).await["verified"], true);
    }

    let r = verify(&c, &leaf).await;
    assert_eq!(r["snapshot"]["verified"], true, "{r}");
    assert_eq!(r["checked"], json!([]));

    // A new chunk starts unverified, and only it is checked.
    let next = unverified(&c, &on(&leaf, "theorem next : 4 = 4 := rfl\nneeds base")).await;
    assert_eq!(verify(&c, &next).await["checked"], json!([module(&next)]));
    assert_eq!(info(&c, &next).await["verified"], true);

    // Verifying doesn't get in the way of using it, or of deleting it.
    let r = check(&c, &on(&next, "needs base\nneeds next")).await;
    assert_eq!(r["ok"], true, "{r}");
    delete_all(&c, &[&root, &mid, &leaf, &next]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_checks_a_merged_import_once() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = unverified(&c, ROOT).await;
    let left = unverified(&c, &on(&root, "theorem l : 2 = 2 := rfl")).await;
    let right = unverified(&c, &on(&root, "theorem r : 3 = 3 := rfl")).await;
    let merge = format!(
        "import Foo\nimport {}\nimport {}\n\ntheorem m : 4 = 4 := rfl",
        module(&left),
        module(&right),
    );
    let merged = unverified(&c, &merge).await;
    assert_eq!(
        verify(&c, &merged).await["checked"],
        json!([
            module(&root),
            module(&left),
            module(&right),
            module(&merged)
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_reports_a_forged_module_and_stops_there() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = unverified(&c, ROOT).await;
    let forged = unverified(&c, &on(&root, "theorem forge : 2 = 2 := rfl")).await;
    let leaf = unverified(&c, &on(&forged, "theorem leaf : 3 = 3 := rfl")).await;

    let r = verify(&c, &leaf).await;
    assert_eq!(r["snapshot"]["verified"], false, "{r}");
    assert_eq!(r["checked"], json!([module(&root)]));
    assert_eq!(r["failure"]["module"], module(&forged));
    assert!(
        r["failure"]["reason"]
            .as_str()
            .is_some_and(|m| m.contains("kernel re-check")),
        "{r}"
    );
    assert_eq!(info(&c, &root).await["verified"], true);
    assert_eq!(info(&c, &forged).await["verified"], false);
    assert_eq!(info(&c, &leaf).await["verified"], false);

    // A retry doesn't repeat what passed, and everything stays usable.
    let r = verify(&c, &leaf).await;
    assert_eq!(r["checked"], json!([]));
    assert_eq!(r["failure"]["module"], module(&forged));
    let r = check(&c, &on(&leaf, "needs leaf")).await;
    assert_eq!(r["ok"], true, "{r}");
}

/// The stored report is rewritten as a dishonest build could have left it.
#[tokio::test(flavor = "multi_thread")]
async fn verify_takes_the_axioms_from_the_kernel_re_check() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let snapshot = unverified(&c, "import Foo\n\ntheorem d : 1 = 1 := by sorry").await;
    let id = &module(&snapshot)["PisaSnap.s".len()..];
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
    let forged = catalog.update_module(id, &build, |meta| {
        meta.status = ProofStatus::Proved;
        meta.result.ok = true;
        meta.result.reasons = Vec::new();
        for d in &mut meta.result.declarations {
            d.axioms.clear();
            d.status = ProofStatus::Proved;
        }
    });
    assert!(forged.await.unwrap().is_some());
    assert_eq!(info(&c, &snapshot).await["status"], "proved");

    let r = verify(&c, &snapshot).await;
    assert_eq!(r["snapshot"]["verified"], true, "{r}");
    assert_eq!(r["snapshot"]["status"], "sorry", "{r}");
    let i = info(&c, &snapshot).await;
    assert_eq!(i["status"], "sorry", "{i}");
    assert_eq!(i["check"]["ok"], false, "{i}");
    assert_eq!(i["check"]["reasons"], json!(["axiom: d uses sorryAx"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_runs_on_a_worker_that_never_saw_the_snapshot() {
    let mut c = Cluster::start(&[("w0", "v", 1)]).await;
    // Plain code: module-system code is re-checked when it is created.
    let root = unverified(&c, ROOT).await;
    let child = unverified(&c, &on(&root, "theorem mchild : 3 = 3 := rfl")).await;

    c.add_worker(common::worker_config("w1", "v", 1));
    c.wait_for_workers(2).await;
    c.kill_worker(0).await;

    let r = verify(&c, &child).await;
    assert_eq!(r["snapshot"]["verified"], true, "{r}");
    assert_eq!(r["checked"], json!([module(&root), module(&child)]));
    // Read from the fetched module: a missing file would report nothing.
    assert_eq!(names(&info(&c, &child).await["check"]), ["mchild"]);
}

/// Starts verifying a snapshot whose code has a `SLOW_KERNEL` marker and gives
/// the re-check time to begin.
async fn verifying(c: &Cluster, snapshot: &Value) -> tokio::task::JoinHandle<(u16, Value)> {
    let url = c.url(&format!("/snapshots/{}/verify", entry(snapshot)));
    let client = c.client.clone();
    let running = tokio::spawn(async move {
        let r = client.post(url).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap())
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    running
}

const SLOW: &str = "import Foo\n\ntheorem s : 1 = 1 := rfl\n-- SLOW_KERNEL 3000";

#[tokio::test(flavor = "multi_thread")]
async fn checks_on_a_snapshot_dont_wait_for_its_verification() {
    // With no confirmation window, every check looks the snapshot up again.
    let c = Cluster::start_with(common::router_config(), &[("w0", "v", 2)], |mut w| {
        w.snapshot_confirm_secs = 0;
        w
    })
    .await;
    let snapshot = unverified(&c, SLOW).await;
    let running = verifying(&c, &snapshot).await;

    let start = Instant::now();
    let r = check(&c, &on(&snapshot, "needs s")).await;
    assert_eq!(r["ok"], true, "{r}");
    assert!(
        start.elapsed() < Duration::from_millis(1500),
        "the check waited {:?}",
        start.elapsed()
    );
    let (status, r) = running.await.unwrap();
    assert_eq!(
        (status, &r["snapshot"]["verified"]),
        (200, &json!(true)),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_deleted_during_verification_stays_deleted() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let snapshot = unverified(&c, SLOW).await;
    let running = verifying(&c, &snapshot).await;

    delete_all(&c, &[&snapshot]).await;

    let (status, r) = running.await.unwrap();
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));
    assert_eq!(stored(&c).await, (vec![], vec![]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verify_that_runs_out_of_time_stops_at_the_module() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let snapshot = unverified(&c, SLOW).await;
    let path = format!("/snapshots/{}/verify?timeout=1", entry(&snapshot));
    let (status, r) = request(&c, reqwest::Method::POST, &path).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["failure"]["module"], module(&snapshot), "{r}");
    let reason = r["failure"]["reason"].as_str().unwrap();
    assert!(reason.contains("more than 1s"), "{reason}");
    assert_eq!(r["snapshot"]["verified"], false, "{r}");
    // With the default time, it passes.
    assert_eq!(verify(&c, &snapshot).await["snapshot"]["verified"], true);
}

/// A create's re-check has the job's time. Running out of it refuses code
/// whose statuses only the kernel can vouch for: module-system code, code
/// with macros, and code asked to be verified.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_check_out_of_time_refuses_what_only_the_kernel_vouches_for() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let slow = "\n-- SLOW_KERNEL 3000";
    for (code, verify) in [
        (format!("{MODULE_ROOT}{slow}"), None),
        (format!("{MACRO}{slow}"), None),
        (SLOW.to_string(), Some(true)),
    ] {
        let mut body = json!({"code": code, "timeout": 1});
        if let Some(verify) = verify {
            body["verify"] = json!(verify);
        }
        let (status, r) = c.post("/snapshots", body).await;
        assert_eq!(status, 200, "{r}");
        assert!(r["snapshot"].is_null(), "{code}: {r}");
        assert_eq!(r["check"]["error"]["kind"], "timeout", "{code}: {r}");
        let message = r["check"]["error"]["message"].as_str().unwrap();
        assert!(message.contains("more than 1s"), "{message}");
    }
    assert_eq!(stored(&c).await, (vec![], vec![]));
}

/// A create that asks to verify a kept module whose last entry is deleted
/// during the re-check starts over, and builds the module again.
#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_deleted_while_a_create_verifies_it_is_built_again() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let snapshot = unverified(&c, SLOW).await;
    let creating = tokio::spawn({
        let (client, url) = (c.client.clone(), c.url("/snapshots"));
        async move {
            let body = json!({"code": SLOW, "verify": true});
            let r = client.post(url).json(&body).send().await.unwrap();
            (r.status().as_u16(), r.json::<Value>().await.unwrap())
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        delete(&c, &snapshot).await["removed"],
        json!([module(&snapshot)])
    );

    let (status, r) = creating.await.unwrap();
    assert_eq!(status, 200, "{r}");
    assert!(r["check"].get("error").is_none(), "{r}");
    assert_eq!(module(&r), module(&snapshot), "{r}");
    assert_eq!(r["snapshot"]["verified"], true, "{r}");
    assert_ne!(entry(&r), entry(&snapshot));
    assert_eq!(info(&c, &r).await["verified"], true);
}

/// The requests to every endpoint that takes the snapshot id `id`, with no
/// `?version=`, ending with its delete.
fn by_id(id: &str) -> Vec<(reqwest::Method, String)> {
    let path = format!("/snapshots/{id}");
    vec![
        (reqwest::Method::GET, path.clone()),
        (reqwest::Method::PATCH, path.clone()),
        (reqwest::Method::POST, format!("{path}/verify")),
        (reqwest::Method::GET, format!("{path}/dependents")),
        (reqwest::Method::DELETE, path),
    ]
}

/// [`request`], with a label to set as a PATCH's body.
async fn send(c: &Cluster, method: reqwest::Method, path: &str) -> (u16, Value) {
    let r = c.client.request(method.clone(), c.url(path));
    let r = match method {
        reqwest::Method::PATCH => r.json(&json!({"labels": {"k": "a"}})),
        _ => r,
    };
    let r = r.send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

/// Sends each of [`by_id`]'s requests for `snapshot`, which must find it.
async fn found_by_its_id(c: &Cluster, snapshot: &Value) {
    for (method, path) in by_id(entry(snapshot)) {
        let (status, r) = send(c, method.clone(), &path).await;
        assert_eq!(status, 200, "{method} {path}: {r}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_ids_carry_their_version() {
    let c = Cluster::start(&[("w0", "a", 1), ("w1", "b", 1)]).await;
    let (status, snapshot) = c
        .post("/snapshots", json!({"code": ROOT, "version": "b"}))
        .await;
    assert_eq!(status, 200, "{snapshot}");
    let id = entry(&snapshot);
    let prefix = format!("{}~", c.version("b"));
    assert!(id.starts_with(&prefix), "{snapshot}");

    // A bare id, as ids were before they carried their version.
    let bare = &id[prefix.len()..];
    for (method, path) in by_id(bare) {
        let (status, r) = send(&c, method.clone(), &path).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{method} {path}: {r}"
        );
        let message = r["error"]["message"].as_str().unwrap();
        assert!(message.contains("<version>~"), "{message}");
    }

    found_by_its_id(&c, &snapshot).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_alias_resolves_into_the_id() {
    let mut router = common::router_config();
    router.alias.push(("stable".into(), "b".into()));
    let c = Cluster::start_with(router, &[("w0", "a", 1), ("w1", "b", 1)], |w| w).await;
    let (status, snapshot) = c
        .post("/snapshots", json!({"code": ROOT, "version": "stable"}))
        .await;
    assert_eq!(status, 200, "{snapshot}");
    let prefix = format!("{}~", c.version("b"));
    assert!(entry(&snapshot).starts_with(&prefix), "{snapshot}");
    assert_eq!(info(&c, &snapshot).await["id"], entry(&snapshot));

    found_by_its_id(&c, &snapshot).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_rejects_unknown_snapshots_and_other_modules() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let unknown = format!("/snapshots/{}~{}/verify", c.version("v"), "0".repeat(32));
    let (status, r) = request(&c, reqwest::Method::POST, &unknown).await;
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));

    // Modules aren't snapshots, even kept ones.
    let snapshot = kept(&c, ROOT).await;
    let path = format!("/snapshots/{}", module(&snapshot));
    for (method, path) in [
        (reqwest::Method::POST, format!("{path}/verify")),
        (reqwest::Method::GET, path.clone()),
        (reqwest::Method::DELETE, path),
    ] {
        let (status, r) = request(&c, method.clone(), &path).await;
        assert_eq!(
            (status, &r["error"]["kind"]),
            (400, &json!("bad_request")),
            "{method} {path}"
        );
        let message = r["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("<version>~") && message.contains("module"),
            "{message}"
        );
    }
    let (status, _) = request(&c, reqwest::Method::POST, "/snapshots/Mathlib/verify").await;
    assert_eq!(status, 400);
}

/// A worker that lives less than the sweep's interval still sweeps, what is
/// older than the sweep's grace.
#[tokio::test(flavor = "multi_thread")]
async fn a_worker_sweeps_what_a_stopped_delete_left_as_it_starts() {
    let mut c = Cluster::start(&[]).await;
    let stop = c.add_drainable_worker(common::worker_config("w0", "v", 1));
    c.wait_for_workers(1).await;
    let parent = kept(&c, ROOT).await;
    let kid = kept(&c, &on(&parent, "theorem c : 2 = 2 := rfl")).await;
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let parent_id = &module(&parent)["PisaSnap.s".len()..];
    // The parent's delete keeps its module for the child; the child's
    // stops once its entry and record are gone.
    let (status, r) = request(
        &c,
        reqwest::Method::DELETE,
        &format!("/snapshots/{}", entry(&parent)),
    )
    .await;
    assert_eq!(status, 200, "{r}");
    let kid_entry = pisa_protocol::api::SnapshotId::parse(entry(&kid))
        .unwrap()
        .entry;
    catalog.remove_entry(&kid_entry).await.unwrap();
    assert!(catalog.module(parent_id).await.is_ok());
    // A module that was left to nothing long ago, as a build kept with no
    // entry is.
    let old_id = pisa_protocol::snapshot::id("an old build");
    let mut old = catalog.module(parent_id).await.unwrap();
    old.modules = vec![pisa_worker::catalog::Module {
        id: old_id.clone(),
        build: "b".into(),
        files: Vec::new(),
    }];
    old.created = 0;
    catalog.commit_module(&old_id, &old, None).await.unwrap();

    stop.send(()).unwrap();
    c.wait_for_workers(0).await;
    c.add_worker(common::worker_config("w0", "v", 1));
    let swept = tokio::time::timeout(Duration::from_secs(10), async {
        while catalog.module(&old_id).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    assert!(swept.await.is_ok(), "the old module is still there");
    // The parent goes once it is older than the sweep's grace.
    assert!(catalog.module(parent_id).await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_again_describes_a_verified_chain_again() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let base = create(&c, "import Foo\ntheorem base_again : 1 = 1 := rfl").await;
    let header = base["snapshot"]["header"].as_str().unwrap().to_string();
    let top = create(&c, &format!("{header}\ntheorem top_again : 2 = 2 := rfl")).await;
    assert_eq!(top["snapshot"]["verified"], true, "{top}");
    // Verified already: nothing to re-check.
    assert_eq!(verify(&c, &top).await["checked"], json!([]));
    let path = format!("/snapshots/{}/verify?again=true", entry(&top));
    let (status, r) = request(&c, reqwest::Method::POST, &path).await;
    assert_eq!(status, 200, "{r}");
    let modules = [&base, &top].map(|s| s["snapshot"]["module"].clone());
    assert_eq!(r["checked"], json!(modules), "{r}");
    assert_eq!(r["snapshot"]["verified"], true);
}

/// A record marked verified that the kernel rejects when asked again loses
/// `verified`, and so does what builds on it.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_check_that_rejects_a_verified_module_clears_verified() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let forged = unverified(&c, FORGED).await;
    let id = &module(&forged)["PisaSnap.s".len()..];
    let catalog = Catalog::new(c.db.clone(), c.version("v"));
    let build = catalog
        .module(id)
        .await
        .unwrap()
        .modules
        .pop()
        .unwrap()
        .build;
    // As though its record had been marked verified wrongly.
    let marked = catalog.update_module(id, &build, |meta| meta.verified = true);
    assert!(marked.await.unwrap().is_some());
    let above = create_with(&c, &on(&forged, "theorem c : 2 = 2 := rfl"), true).await;
    assert_eq!(above["snapshot"]["verified"], true, "{above}");
    let path = format!("/snapshots/{}/verify?again=true", entry(&forged));
    let (status, r) = request(&c, reqwest::Method::POST, &path).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["failure"]["kind"], "lean", "{r}");
    assert_eq!(info(&c, &forged).await["verified"], false);
    assert_eq!(info(&c, &above).await["verified"], false);
}
