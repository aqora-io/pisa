//! Stage 4: environment snapshots, compiled by fake-lean and imported by
//! fake-repl. A `needs NAME` line fails unless `NAME` is declared, so it shows
//! what an import provides.

mod common;

use std::time::{Duration, Instant};

use common::Cluster;
use object_store::ObjectStoreExt;
use object_store::path::Path;
use pisa_worker::kv::{ROOT as KV_ROOT, key};
use serde_json::{Value, json};

async fn create(c: &Cluster, code: &str) -> Value {
    let (status, r) = c.post("/snapshots", json!({"code": code})).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// Creates a snapshot that must be kept and returns its response.
async fn kept(c: &Cluster, code: &str) -> Value {
    let r = create(c, code).await;
    assert!(r["snapshot"].is_string(), "{code}: {r}");
    r
}

async fn check(c: &Cluster, code: &str) -> Value {
    let (status, r) = c.post("/check", json!({"code": code})).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// `header`, a blank line, then `body`.
fn on(snapshot: &Value, body: &str) -> String {
    format!("{}\n\n{body}", snapshot["header"].as_str().unwrap())
}

fn names(result: &Value) -> Vec<&str> {
    result["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect()
}

/// The snapshot blobs and the KV keys of version `v`.
async fn stored(c: &Cluster) -> (Vec<String>, Vec<Vec<String>>) {
    (c.stored_blobs().await, c.stored_keys("v").await)
}

fn entry(snapshot: &Value) -> &str {
    snapshot["entry"].as_str().unwrap()
}

/// Where the store keeps file `ext` of a snapshot's module: under the build
/// its record names.
async fn blob(c: &Cluster, snapshot: &Value, ext: &str) -> String {
    let id = &module(snapshot)["PisaSnap.s".len()..];
    let v = c.version("v");
    let record = key(&[KV_ROOT, &v, "mod", id]);
    let bytes = c.kv.begin().await.unwrap().get(&record).await.unwrap();
    let meta: Value = serde_json::from_slice(&bytes.unwrap()).unwrap();
    let build = meta["modules"].as_array().unwrap().last().unwrap()["build"].clone();
    format!("snapshots/{v}/{id}.{}.{ext}.zst", build.as_str().unwrap())
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
    let module = root["snapshot"].as_str().unwrap();
    assert!(module.starts_with("PisaSnap.s"), "{root}");
    assert_eq!(root["header"], format!("import Foo\nimport {module}"));
    assert_eq!(root["version"], c.version("v"));
    assert_eq!(root["dirty"], false);
    assert_eq!(root["result"]["ok"], true, "{root}");
    assert_eq!(names(&root["result"]), ["base"]);

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
    assert_eq!(names(&left["result"]), ["l"], "the report covers the chunk");

    let merge = format!(
        "import Foo\nimport {}\nimport {}\n\ntheorem m : 4 = 4 := rfl\nneeds base\nneeds l\nneeds r",
        left["snapshot"].as_str().unwrap(),
        right["snapshot"].as_str().unwrap(),
    );
    let merged = kept(&c, &merge).await;
    assert_eq!(merged["result"]["ok"], true, "{merged}");
    assert_eq!(names(&merged["result"]), ["m"]);
    let module = merged["snapshot"].as_str().unwrap();
    assert_eq!(merged["header"], format!("import Foo\nimport {module}"));

    let r = check(&c, &on(&merged, "needs base\nneeds l\nneeds r\nneeds m")).await;
    assert_eq!(r["ok"], true, "{r}");

    // A parent's private declarations stay private.
    let private = kept(&c, &on(&root, "private theorem hidden : 1 = 1 := rfl")).await;
    let r = check(&c, &on(&private, "needs hidden")).await;
    assert_eq!(r["ok"], false, "{r}");
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
    assert!(r.get("header").is_none(), "{r}");
    assert_eq!(r["result"]["ok"], false);
    assert_eq!(r["result"]["messages"][0]["severity"], "error");
    assert_eq!(r["result"]["messages"][0]["pos"]["line"], 3);
    assert_eq!(
        r["result"]["reasons"],
        json!(["error: Lean reported errors"])
    );

    // The policy rejects it before anything runs: CRASH would kill fake-lean.
    let r = create(&c, "#eval 1\nCRASH").await;
    assert_eq!(r["result"]["error"]["kind"], "policy", "{r}");
    assert_eq!(r["result"]["reasons"], json!(["forbidden: #eval"]));

    let (status, r) = c
        .post(
            "/snapshots",
            json!({"code": "theorem t : True := trivial\nSLEEP 5000", "timeout": 1}),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(r["result"]["error"]["kind"], "timeout", "{r}");

    let r = create(&c, "theorem t : True := trivial\nCRASH").await;
    assert_eq!(r["result"]["error"]["kind"], "crash", "{r}");

    let r = create(&c, "import PisaSnap.oops\n\ntheorem t : True := trivial").await;
    assert_eq!(r["result"]["error"]["kind"], "bad_request", "{r}");

    assert_eq!(stored(&c).await, (vec![], vec![]));
    // The slot still works.
    kept(&c, ROOT).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sorry_is_kept_but_dirty_and_same_code_shares_a_module() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let code = "import Foo\n\ntheorem d : 1 = 1 := by sorry";
    let dirty = kept(&c, code).await;
    assert_eq!(dirty["dirty"], true);
    assert_eq!(dirty["result"]["ok"], false);
    assert_eq!(dirty["result"]["reasons"], json!(["axiom: d uses sorryAx"]));
    assert_eq!(dirty["result"]["messages"][0]["severity"], "warning");

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
    assert_eq!(first["snapshot"], again["snapshot"]);
    assert_eq!(first["result"], again["result"]);
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
        assert_eq!(r["labels"], json!({"run": run}), "{r}");
        created.push(r);
    }
    for (snapshot, run) in created.iter().zip(["1", "2"]) {
        let i = info(&c, snapshot).await;
        assert_eq!(i["entry"], snapshot["entry"], "{i}");
        assert_eq!(i["labels"], json!({"run": run}), "{i}");
        assert_eq!(i["snapshot"], created[0]["snapshot"], "{i}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_labels_are_rejected() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let body = json!({"code": ROOT, "labels": {"bad key": "x"}});
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["result"]["error"]["kind"], "bad_request", "{r}");
    assert!(r.get("entry").is_none(), "{r}");
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_eq!(stored(&c).await, (vec![], vec![]));
}

async fn create_keyed(c: &Cluster, code: &str, key: &str, labels: Value) -> Value {
    let body = json!({"code": code, "key": key, "labels": labels});
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    r
}

/// Whether the KV store has the idempotency key `k`.
async fn has_key(c: &Cluster, k: &str) -> bool {
    let want = [KV_ROOT, &c.version("v"), "key", k]
        .map(String::from)
        .to_vec();
    c.stored_keys("v").await.contains(&want)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_key_returns_the_same_entry() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let code = "import Foo\n\ntheorem s : 1 = 1 := rfl\n-- SLEEP 500";
    let first = create_keyed(&c, code, "k1", json!({"a": "1"})).await;
    assert!(first["snapshot"].is_string(), "{first}");
    assert_eq!(first["labels"], json!({"a": "1"}), "{first}");

    let start = Instant::now();
    let again = create_keyed(&c, code, "k1", json!({"a": "2"})).await;
    assert!(start.elapsed() < Duration::from_millis(400), "{again}");
    assert_eq!(again["entry"], first["entry"], "{again}");
    assert_eq!(again["snapshot"], first["snapshot"], "{again}");
    assert_eq!(again["labels"], json!({"a": "1"}), "{again}");
    assert_eq!(info(&c, &first).await["labels"], json!({"a": "1"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_with_different_code_is_refused() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    create_keyed(&c, ROOT, "k1", json!({})).await;
    let before = stored(&c).await;

    let other = "import Foo\n\ntheorem other : 2 = 2 := rfl";
    let r = create_keyed(&c, other, "k1", json!({})).await;
    assert_eq!(r["result"]["error"]["kind"], "bad_request", "{r}");
    assert_eq!(r["result"]["ok"], false, "{r}");
    assert!(r.get("entry").is_none(), "{r}");
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
    assert_eq!(r["result"]["ok"], false, "{r}");
    assert!(r.get("entry").is_none(), "{r}");
    assert!(!has_key(&c, "k2").await);

    let r = create_keyed(&c, ROOT, "k2", json!({})).await;
    assert!(r["entry"].is_string(), "{r}");
    assert!(has_key(&c, "k2").await);
}

/// A kept module that fails the re-check the request asked for gets no entry,
/// so the request's key stays unused.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_re_check_leaves_its_key_unused() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    kept(&c, FORGED).await;
    let body = json!({"code": FORGED, "verify": true, "key": "k5"});
    let (status, r) = c.post("/snapshots", body).await;
    assert_eq!(status, 200, "{r}");
    assert_rejected_by_kernel(&r);
    assert!(r.get("entry").is_none(), "{r}");
    assert!(!has_key(&c, "k5").await);

    let r = create_keyed(&c, FORGED, "k5", json!({})).await;
    assert!(r["entry"].is_string(), "{r}");
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
    assert!(a["entry"].is_string(), "{a}");
    assert_eq!(a["entry"], b["entry"], "{a} {b}");
    delete_all(&c, &[&a]).await;

    let other = "import Foo\n\ntheorem t : 2 = 2 := rfl\n-- SLEEP 500";
    let (a, b) = tokio::join!(creating(&c, code, "k7"), creating(&c, other, "k7"));
    let (a, b) = (a.unwrap(), b.unwrap());
    let (won, lost) = if a.get("entry").is_some() {
        (a, b)
    } else {
        (b, a)
    };
    assert!(won["entry"].is_string(), "{won}");
    assert_eq!(lost["result"]["error"]["kind"], "bad_request", "{lost}");
    assert!(lost.get("entry").is_none() && lost.get("snapshot").is_none());
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
    assert!(second["snapshot"].is_string(), "{second}");
    assert_ne!(entry(&second), entry(&first));
    assert_eq!(info(&c, &second).await["entry"], second["entry"]);
    delete_all(&c, &[&second]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_keys_are_rejected() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    for key in ["has space", "", &"k".repeat(257)] {
        let r = create_keyed(&c, ROOT, key, json!({})).await;
        assert_eq!(r["result"]["error"]["kind"], "bad_request", "{key:?}: {r}");
        assert!(r.get("entry").is_none(), "{r}");
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
        let r = check(
            &c,
            &format!(
                "import Bar\nimport {}\n",
                root["snapshot"].as_str().unwrap()
            ),
        )
        .await;
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

    c.add_worker(common::worker_config(c.router.worker_addr, "w1", "v", 1));
    c.wait_for_workers(2).await;
    c.workers[0].abort();
    c.wait_for_workers(1).await;

    let r = check(&c, &on(&child, "needs base\nneeds c")).await;
    assert_eq!(r["ok"], true, "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_snapshots_are_not_found() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
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
    let mut c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = kept(&c, ROOT).await;
    let other = kept(&c, "import Foo\n\ntheorem base : 1 = 1 := rfl\n-- other").await;
    let code = on(&root, "needs base\ntheorem t : 1 = 1 := by sorry");
    let (status, s) = c.post("/sessions", json!({"code": code})).await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["states"][0]["pos"]["line"], 5);

    c.add_worker(common::worker_config(c.router.worker_addr, "w1", "v", 1));
    c.wait_for_workers(2).await;
    c.workers[0].abort();
    c.wait_for_workers(1).await;

    let state = s["states"][0]["state"].as_str().unwrap();
    let (status, step) = c
        .post(&format!("/states/{state}/tactic"), json!({"tactic": "rfl"}))
        .await;
    assert_eq!(status, 200, "{step}");
    assert_eq!(step["complete"], true);

    let session = s["session"].as_str().unwrap();
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
    assert_eq!(i["entry"], child["entry"]);
    assert_eq!(i["snapshot"], child["snapshot"]);
    assert_eq!(i["imports"], json!([module(&root)]));
    assert_eq!(i["header"], child["header"]);
    assert_eq!(i["dirty"], false);
    assert_eq!(names(&i["result"]), ["c"]);
    assert!(
        i["code"]
            .as_str()
            .unwrap()
            .ends_with("theorem c : 2 = 2 := rfl")
    );

    let gone = delete(&c, &root).await;
    assert_eq!(gone, json!({"entry": entry(&root), "removed": []}));
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
    assert_eq!(first["snapshot"], second["snapshot"]);

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
    assert_eq!(again["snapshot"], first["snapshot"]);
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
    assert!(r.get("entry").is_none(), "{r}");
    assert_eq!(r["result"]["error"]["kind"], "not_found", "{r}");
    let message = r["result"]["error"]["message"].as_str().unwrap();
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

fn bwrap_works() -> bool {
    std::process::Command::new("bwrap")
        .args(["--unshare-all", "--ro-bind", "/", "/", "true"])
        .status()
        .is_ok_and(|s| s.success())
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

#[tokio::test(flavor = "multi_thread")]
async fn module_system_snapshots_keep_all_their_files() {
    let mut c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = kept(&c, MODULE_ROOT).await;
    let name = root["snapshot"].as_str().unwrap();
    assert_eq!(
        root["header"],
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
    let child_name = child["snapshot"].as_str().unwrap();
    assert_eq!(
        child["header"],
        format!("module\npublic import Foo\npublic import {child_name}")
    );

    // Another worker must fetch every file: a module-system module without
    // its .olean.private imports as empty.
    c.add_worker(common::worker_config(c.router.worker_addr, "w1", "v", 1));
    c.wait_for_workers(2).await;
    c.workers[0].abort();
    c.wait_for_workers(1).await;
    let r = check(&c, &on(&child, "needs mbase\nneeds mchild")).await;
    assert_eq!(r["ok"], true, "{r}");
    let r = check(&c, &on(&child, "needs mhidden")).await;
    assert_eq!(r["ok"], false, "only public declarations are visible: {r}");
    // Plain code can import module-system snapshots.
    let r = check(&c, &format!("import {child_name}\n\nneeds mchild")).await;
    assert_eq!(r["ok"], true, "{r}");

    delete_all(&c, &[&root, &child]).await;
}

/// fake-lean's verifier rejects any module whose code contains `forge`, so
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

fn module(snapshot: &Value) -> &str {
    snapshot["snapshot"].as_str().unwrap()
}

async fn info(c: &Cluster, snapshot: &Value) -> Value {
    let path = format!("/snapshots/{}", entry(snapshot));
    let (status, r) = request(c, reqwest::Method::GET, &path).await;
    assert_eq!(status, 200, "{r}");
    r
}

async fn verify(c: &Cluster, snapshot: &Value) -> Value {
    let path = format!("/snapshots/{}/verify", entry(snapshot));
    let (status, r) = request(c, reqwest::Method::POST, &path).await;
    assert_eq!(status, 200, "{r}");
    r
}

fn assert_rejected_by_kernel(r: &Value) {
    assert_eq!(r["result"]["ok"], false, "{r}");
    assert!(
        r["result"]["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("kernel re-check")),
        "{r}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_skip_the_kernel_re_check_by_default() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let forged = kept(&c, FORGED).await;
    assert_eq!(forged["verified"], false, "{forged}");
    assert_eq!(forged["result"]["ok"], true, "{forged}");
    assert_eq!(names(&forged["result"]), ["forge"]);
    assert_eq!(info(&c, &forged).await["verified"], false);

    // The axiom report still runs, so `sorry` is still caught.
    let dirty = kept(&c, "import Foo\n\ntheorem d : 1 = 1 := by sorry").await;
    assert_eq!(dirty["verified"], false, "{dirty}");
    assert_eq!(dirty["dirty"], true, "{dirty}");
    assert_eq!(dirty["result"]["reasons"], json!(["axiom: d uses sorryAx"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn macros_are_re_checked_unless_the_request_opts_out() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let macro_snap = kept(&c, MACRO).await;
    assert_eq!(macro_snap["verified"], true, "{macro_snap}");
    assert_eq!(macro_snap["result"]["ok"], true, "{macro_snap}");

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
    assert_eq!(child["verified"], true, "{child}");

    let opted_out = create_with(&c, &forged_macro, false).await;
    assert!(opted_out["snapshot"].is_string(), "{opted_out}");
    assert_eq!(opted_out["verified"], false, "{opted_out}");
    let code = on(&macro_snap, "theorem forge : 2 = 2 := twice");
    let child = create_with(&c, &code, false).await;
    assert!(child["snapshot"].is_string(), "{child}");
    assert_eq!(child["verified"], false, "{child}");

    // elab runs code at elaboration time, so opting out doesn't allow it.
    let elab = "import Foo\n\nelab \"x\" : term => pure default";
    let r = create_with(&c, elab, false).await;
    assert_eq!(r["result"]["error"]["kind"], "policy", "{r}");
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
    let root = kept(&c, ROOT).await;
    let chunk = kept(&c, &on(&root, "macro \"twice\" : term => `(2)")).await;
    assert_eq!(chunk["verified"], false, "{chunk}");

    let r = verify(&c, &chunk).await;
    assert_eq!(r["verified"], true, "{r}");
    assert_eq!(r["checked"], json!([module(&root), module(&chunk)]));
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_true_re_checks_the_chunk_and_its_imports() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let r = create_with(&c, FORGED, true).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_rejected_by_kernel(&r);
    assert_eq!(stored(&c).await, (vec![], vec![]));

    let parent = kept(&c, ROOT).await;
    let child = create_with(&c, &on(&parent, "theorem c : 2 = 2 := rfl"), true).await;
    assert_eq!(child["verified"], true, "{child}");
    assert_eq!(info(&c, &parent).await["verified"], true);

    // A forged import stops it, and the chunk is not built.
    let forged = kept(&c, FORGED).await;
    let before = stored(&c).await;
    let r = create_with(&c, &on(&forged, "theorem c : 2 = 2 := rfl"), true).await;
    assert!(r.get("snapshot").is_none(), "{r}");
    assert_rejected_by_kernel(&r);
    let message = r["result"]["error"]["message"].as_str().unwrap();
    assert!(message.contains(module(&forged)), "{r}");
    assert_eq!(stored(&c).await, before);

    // Asking again for code that is already kept verifies what is there.
    let code = "import Foo\n\ntheorem existing : 1 = 1 := rfl";
    let first = kept(&c, code).await;
    assert_eq!(first["verified"], false, "{first}");
    let again = create_with(&c, code, true).await;
    assert_eq!(again["snapshot"], first["snapshot"]);
    assert_eq!(again["verified"], true, "{again}");

    // A kept module that fails the re-check gets no new entry, as new code
    // that fails it isn't kept.
    let before = c.stored_keys("v").await;
    let r = create_with(&c, FORGED, true).await;
    assert_eq!(r["snapshot"], forged["snapshot"], "still kept: {r}");
    assert_eq!(r["header"], forged["header"], "{r}");
    assert!(r.get("entry").is_none(), "{r}");
    assert_eq!(r["verified"], false, "{r}");
    assert_rejected_by_kernel(&r);
    assert_eq!(c.stored_keys("v").await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_checks_imports_first_and_remembers() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = kept(&c, ROOT).await;
    let mid = kept(&c, &on(&root, "theorem mid : 2 = 2 := rfl")).await;
    let leaf = kept(&c, &on(&mid, "theorem leaf : 3 = 3 := rfl")).await;

    let r = verify(&c, &leaf).await;
    assert_eq!(
        r,
        json!({
            "entry": entry(&leaf),
            "snapshot": module(&leaf),
            "verified": true,
            "dirty": false,
            "checked": [module(&root), module(&mid), module(&leaf)],
        })
    );
    for snapshot in [&root, &mid, &leaf] {
        assert_eq!(info(&c, snapshot).await["verified"], true);
    }

    let r = verify(&c, &leaf).await;
    assert_eq!(r["verified"], true, "{r}");
    assert_eq!(r["checked"], json!([]));

    // A new chunk starts unverified, and only it is checked.
    let next = kept(&c, &on(&leaf, "theorem next : 4 = 4 := rfl\nneeds base")).await;
    assert_eq!(next["verified"], false, "{next}");
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
    let root = kept(&c, ROOT).await;
    let left = kept(&c, &on(&root, "theorem l : 2 = 2 := rfl")).await;
    let right = kept(&c, &on(&root, "theorem r : 3 = 3 := rfl")).await;
    let merge = format!(
        "import Foo\nimport {}\nimport {}\n\ntheorem m : 4 = 4 := rfl",
        module(&left),
        module(&right),
    );
    let merged = kept(&c, &merge).await;
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
    let root = kept(&c, ROOT).await;
    let forged = kept(&c, &on(&root, "theorem forge : 2 = 2 := rfl")).await;
    let leaf = kept(&c, &on(&forged, "theorem leaf : 3 = 3 := rfl")).await;

    let r = verify(&c, &leaf).await;
    assert_eq!(r["verified"], false, "{r}");
    assert_eq!(r["checked"], json!([module(&root)]));
    assert_eq!(r["failure"]["snapshot"], module(&forged));
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
    assert_eq!(r["failure"]["snapshot"], module(&forged));
    let r = check(&c, &on(&leaf, "needs leaf")).await;
    assert_eq!(r["ok"], true, "{r}");
}

/// The stored report is rewritten as a dishonest build could have left it.
#[tokio::test(flavor = "multi_thread")]
async fn verify_takes_the_axioms_from_the_kernel_re_check() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let snapshot = kept(&c, "import Foo\n\ntheorem d : 1 = 1 := by sorry").await;
    let id = &module(&snapshot)["PisaSnap.s".len()..];
    let record = key(&[KV_ROOT, &c.version("v"), "mod", id]);
    let mut txn = c.kv.begin().await.unwrap();
    let bytes = txn.get(&record).await.unwrap().unwrap();
    let mut meta: Value = serde_json::from_slice(&bytes).unwrap();
    meta["dirty"] = json!(false);
    meta["result"]["ok"] = json!(true);
    meta["result"]["reasons"] = json!([]);
    let bytes = serde_json::to_vec(&meta).unwrap();
    txn.put(record, bytes).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(info(&c, &snapshot).await["dirty"], false);

    let r = verify(&c, &snapshot).await;
    assert_eq!(r["verified"], true, "{r}");
    assert_eq!(r["dirty"], true, "{r}");
    let i = info(&c, &snapshot).await;
    assert_eq!(i["dirty"], true, "{i}");
    assert_eq!(i["result"]["ok"], false, "{i}");
    assert_eq!(i["result"]["reasons"], json!(["axiom: d uses sorryAx"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_runs_on_a_worker_that_never_saw_the_snapshot() {
    let mut c = Cluster::start(&[("w0", "v", 1)]).await;
    let root = kept(&c, MODULE_ROOT).await;
    let child = kept(&c, &on(&root, "public theorem mchild : 3 = 3 := rfl")).await;

    c.add_worker(common::worker_config(c.router.worker_addr, "w1", "v", 1));
    c.wait_for_workers(2).await;
    c.workers[0].abort();
    c.wait_for_workers(1).await;

    let r = verify(&c, &child).await;
    assert_eq!(r["verified"], true, "{r}");
    assert_eq!(r["checked"], json!([module(&root), module(&child)]));
    // Read from the fetched module: a missing file would report nothing.
    assert_eq!(names(&info(&c, &child).await["result"]), ["mchild"]);
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
    let snapshot = kept(&c, SLOW).await;
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
    assert_eq!((status, &r["verified"]), (200, &json!(true)), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_deleted_during_verification_stays_deleted() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let snapshot = kept(&c, SLOW).await;
    let running = verifying(&c, &snapshot).await;

    delete_all(&c, &[&snapshot]).await;

    let (status, r) = running.await.unwrap();
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));
    assert_eq!(stored(&c).await, (vec![], vec![]));
}

/// A create that asks to verify a kept module whose last entry is deleted
/// during the re-check starts over, and builds the module again.
#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_deleted_while_a_create_verifies_it_is_built_again() {
    let c = Cluster::start(&[("w0", "v", 2)]).await;
    let snapshot = kept(&c, SLOW).await;
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
    assert!(r["result"].get("error").is_none(), "{r}");
    assert_eq!(r["snapshot"], snapshot["snapshot"], "{r}");
    assert_eq!(r["verified"], true, "{r}");
    assert_ne!(entry(&r), entry(&snapshot));
    assert_eq!(info(&c, &r).await["verified"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_runs_on_the_requested_version() {
    let c = Cluster::start(&[("w0", "a", 1), ("w1", "b", 1)]).await;
    let (status, snapshot) = c
        .post("/snapshots", json!({"code": ROOT, "version": "b"}))
        .await;
    assert_eq!(status, 200, "{snapshot}");
    let path = |v: &str| format!("/snapshots/{}/verify?version={v}", entry(&snapshot));

    let (status, r) = request(&c, reqwest::Method::POST, &path("a")).await;
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));
    let (status, r) = request(&c, reqwest::Method::POST, &path("b")).await;
    assert_eq!((status, &r["verified"]), (200, &json!(true)), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_rejects_unknown_snapshots_and_other_modules() {
    let c = Cluster::start(&[("w0", "v", 1)]).await;
    let unknown = format!("/snapshots/{}/verify", "0".repeat(32));
    let (status, r) = request(&c, reqwest::Method::POST, &unknown).await;
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));

    // Modules aren't entries, even kept ones.
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
        assert!(message.contains("entry"), "{message}");
    }
    let (status, _) = request(&c, reqwest::Method::POST, "/snapshots/Mathlib/verify").await;
    assert_eq!(status, 400);
}
