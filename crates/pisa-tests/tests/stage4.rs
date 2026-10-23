//! Stage 4: environment snapshots, compiled by fake-lean and imported by
//! fake-repl. A `needs NAME` line fails unless `NAME` is declared, so it shows
//! what an import provides.

mod common;

use std::time::{Duration, Instant};

use common::Cluster;
use futures::StreamExt;
use object_store::ObjectStoreExt;
use object_store::path::Path;
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

async fn stored_objects(c: &Cluster) -> Vec<String> {
    let mut out = Vec::new();
    let mut list = c.store.list(Some(&Path::from("snapshots")));
    while let Some(meta) = list.next().await {
        out.push(meta.unwrap().location.to_string());
    }
    out
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
    assert_eq!(root["version"], "v");
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

    assert!(stored_objects(&c).await.is_empty());
    // The slot still works.
    kept(&c, ROOT).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sorry_is_kept_but_dirty_and_creates_are_idempotent() {
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
}

/// Checks on a snapshot go to the worker that built it, which owns its
/// header and keeps it warm. The stored module is corrupted, so any other
/// worker would refuse to load it.
#[tokio::test(flavor = "multi_thread")]
async fn checks_go_to_the_builder_and_corrupt_modules_are_refused() {
    let c = Cluster::start(&[("w0", "v", 1), ("w1", "v", 1)]).await;
    for i in 0..3 {
        let root = kept(&c, &format!("import Foo\n\ntheorem base{i} : 1 = 1 := rfl")).await;
        let id = &root["snapshot"].as_str().unwrap()["PisaSnap.s".len()..];
        let blob = Path::from(format!("snapshots/v/{id}.olean.zst"));
        let garbage = zstd::encode_all(&b"garbage"[..], 3).unwrap();
        c.store.put(&blob, garbage.into()).await.unwrap();
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
async fn snapshots_can_be_read_and_deleted_with_their_descendants() {
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
    let (root_name, child_name) = (
        root["snapshot"].as_str().unwrap(),
        child["snapshot"].as_str().unwrap(),
    );
    // Both workers have used both.
    for _ in 0..4 {
        let r = check(&c, &on(&child, "needs c")).await;
        assert_eq!(r["ok"], true, "{r}");
    }

    let (status, info) = request(
        &c,
        reqwest::Method::GET,
        &format!("/snapshots/{child_name}"),
    )
    .await;
    assert_eq!(status, 200, "{info}");
    assert_eq!(info["imports"], json!([root_name]));
    assert_eq!(info["header"], child["header"]);
    assert_eq!(info["dirty"], false);
    assert_eq!(names(&info["result"]), ["c"]);
    assert!(
        info["code"]
            .as_str()
            .unwrap()
            .ends_with("theorem c : 2 = 2 := rfl")
    );

    let (status, gone) = request(
        &c,
        reqwest::Method::DELETE,
        &format!("/snapshots/{root_name}"),
    )
    .await;
    assert_eq!(status, 200, "{gone}");
    assert_eq!(gone["deleted"], json!([root_name, child_name]));
    assert!(
        stored_objects(&c).await.is_empty(),
        "{:?}",
        stored_objects(&c).await
    );

    let (status, r) = request(
        &c,
        reqwest::Method::GET,
        &format!("/snapshots/{child_name}"),
    )
    .await;
    assert_eq!((status, &r["error"]["kind"]), (404, &json!("not_found")));
    let (status, _) = request(
        &c,
        reqwest::Method::DELETE,
        &format!("/snapshots/{root_name}"),
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = request(&c, reqwest::Method::GET, "/snapshots/Mathlib").await;
    assert_eq!(status, 400);
    for _ in 0..4 {
        let r = check(&c, &on(&child, "needs c")).await;
        assert_eq!(r["error"]["kind"], "not_found", "{r}");
    }
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
    let objects = stored_objects(&c).await;
    let id = &name["PisaSnap.s".len()..];
    for ext in ["olean", "olean.server", "olean.private", "ir", "ir.sig"] {
        let object = format!("snapshots/v/{id}.{ext}.zst");
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

    let (status, gone) = request(&c, reqwest::Method::DELETE, &format!("/snapshots/{name}")).await;
    assert_eq!(status, 200, "{gone}");
    assert!(stored_objects(&c).await.is_empty());
}
